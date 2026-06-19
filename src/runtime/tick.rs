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
            ctx.with_new_scope(|mut cx| {
                // 1. Run all one-shot jobs first (always complete, never retry).
                //    New oneshot jobs may be queued during execution, so keep
                //    draining until the queue is empty.
                if !cx.oneshot_jobs.is_empty() {
                    while let Some(mut job) = cx.oneshot_jobs.pop_front() {
                        // hold job gc tracer so GC can trace it during run()
                        self.gc_job = job.gc_trace.take();
                        if let Err(_e) = job.run(&mut cx) {
                            self.gc_job = None;
                            // Process-level interrupt takes priority.
                            if matches!(self.cur_exception, Exception::Interrupted(_)) {
                                return cx.return_with(Err(-1));
                            }
                            // Per ES §9.5: isolate job error, continue to next job.
                            log::warn!(
                                "[task error] one-shot job failed: {:?}",
                                self.cur_exception
                            );
                            self.cur_exception = Exception::No;
                            continue;
                        }
                    }
                    self.gc_job = None;
                }

                // 2. Run polling jobs (may return `Again`) — one round only.
                let num = cx.polling_jobs.len();
                for _ in 0..num {
                    let Some(mut job) = cx.polling_jobs.pop_front() else {
                        break;
                    };

                    // put job trace to temp holder and run the job
                    self.gc_job = job.gc_trace.take();
                    let res = job.run(&mut cx);
                    let gc_trace = self.gc_job.take();

                    match res {
                        Ok(JobState::Again) => {
                            job.gc_trace = gc_trace;
                            cx.polling_jobs.push_back(job);
                            more_ticks = true;
                        }
                        Ok(JobState::Done) => {}
                        Err(_) => {
                            // Per ES §9.5: isolate job error.
                            if matches!(self.cur_exception, Exception::Interrupted(_)) {
                                return cx.return_with(Err(-1));
                            }
                            log::warn!("[task error] polling job failed: {:?}", self.cur_exception);
                            self.cur_exception = Exception::No;
                            // Don't re-queue — treat as Done.
                        }
                    }
                }

                // Oneshot jobs may also be queued by tick handlers; keep ticking if any remain.
                if !cx.oneshot_jobs.is_empty() {
                    more_ticks = true;
                }

                // 3. customized tick handlers
                let ctx_ptr = ContextPtr::from(cx.gc_ref());
                for f in ctx_ptr.tick_handlers.iter() {
                    if f(&mut cx) {
                        more_ticks = true;
                    }
                }

                cx.return_with(Ok(()))
            })?;
        }

        // Catch-all: interrupt set by native code not caught above (e.g. tick handlers).
        match self.cur_exception {
            Exception::No => {}
            Exception::Interrupted(_) => {
                return Err(-1);
            }
            _ => {
                // Clear any lingering exception before GC and exit.
                // Job error handlers already cleaned up; anything left (e.g. from tick handlers)
                // must not leak to the next tick() call.
                log::warn!(
                    "[task error] lingering exception cleared: {:?}",
                    self.cur_exception
                );
                self.cur_exception = Exception::No;
            }
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
