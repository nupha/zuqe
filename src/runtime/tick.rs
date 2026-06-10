// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::process::ExitCode;

use gc_lite::GcNode;

use crate::{
    Exception,
    context::{ContextPtr, ScopedContext},
    microtask::JobState,
};

use super::RuntimeImpl;

impl<'c> ScopedContext<'c> {
    pub fn add_tick_handler(&mut self, f: fn(&mut ScopedContext) -> bool) {
        self.tick_handlers.push(f);
    }
}

pub enum TickStatus {
    Continue,
    Stop,
}

impl RuntimeImpl {
    /// Interrupt running.
    /// When in native function handler, after calling this, must returns Err
    pub fn interrupt(&mut self, code: ExitCode) {
        self.set_exception(Exception::Interrupted(code));
    }

    /// to run background jobs for one round
    pub(crate) fn tick(&mut self) -> Result<TickStatus, i32> {
        let mut more_ticks = false;

        for mut ctx in self.context_list.iter().copied() {
            ctx.with_new_scope_internal(|mut cx| {
                if !cx.oneshot_jobs.is_empty() {
                    // 1. Run all one-shot jobs first (always complete, never retry).
                    //    New oneshot jobs may be queued during execution, so keep
                    //    draining until the queue is empty.
                    while let Some(job) = cx.oneshot_jobs.pop_front() {
                        self.gc_oneshot_job = Some(job); // place job in gc-visible slot so GC can trace it during run()

                        let job = self.gc_oneshot_job.as_ref().unwrap();
                        if let Err(e) = job.run(&mut cx) {
                            self.gc_oneshot_job = None;
                            return cx.return_with(Err(e));
                        }
                    }

                    self.gc_oneshot_job = None;
                }

                // 2. Run pending polling jobs (may return `Again`).
                while let Some(job) = cx.polling_jobs.pop_front() {
                    // put job into gc-visible collection first, so GC can trace it during run()
                    self.gc_polling_jobs.push(job);

                    let job = self.gc_polling_jobs.last().unwrap();
                    let again = match job.run(&mut cx) {
                        Ok(s) => s == JobState::Again,
                        Err(e) => return cx.return_with(Err(e)),
                    };
                    if again {
                        // multi-run task: leave in gc_jobs, run again in next tick
                    } else {
                        self.gc_polling_jobs.pop();
                    }
                }

                // re-enqueue pending (Again) jobs for next round
                if !self.gc_polling_jobs.is_empty() {
                    cx.polling_jobs.extend(self.gc_polling_jobs.drain(..));
                    more_ticks = true;
                }

                // 3. run tick handlers
                let ctx_ptr = ContextPtr::from(cx.gc_ref());
                for f in ctx_ptr.tick_handlers.iter() {
                    if f(&mut cx) {
                        more_ticks = true;
                    }
                }

                cx.return_with(Ok(()))
            })?;
        }

        if self.gc_req || self.gc_heap().should_gc() {
            self.gc_collect();
        }

        Ok(if more_ticks {
            TickStatus::Continue
        } else {
            TickStatus::Stop
        })
    }
}
