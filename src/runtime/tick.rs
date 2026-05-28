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
            // SAFETY: The return value is Result<(), i32> which has no GC nodes.
            unsafe {
                ctx.with_new_scope_internal(|mut scope| -> Result<(), i32> {
                    // 1. run pending jobs (not for new added jobs)

                    while let Some(job) = scope.job_list.pop_front() {
                        // put job into gc-visible collection first, so GC can trace it during run()
                        self.gc_jobs.push(job);

                        let job = self.gc_jobs.last().unwrap();
                        if job.run(&mut scope)? == JobState::Again {
                            // multi-run task: leave in gc_jobs, run again in next tick
                        } else {
                            self.gc_jobs.pop();
                        }
                    }

                    // re-enqueue pending (Again) jobs to gc_jobs for next round
                    if !self.gc_jobs.is_empty() {
                        scope.job_list.extend(self.gc_jobs.drain(..));
                        more_ticks = true;
                    }

                    // 2. run tick handlers
                    let ctx_ptr = ContextPtr::from(scope.gc_ref());
                    for f in ctx_ptr.tick_handlers.iter() {
                        if f(&mut scope) {
                            more_ticks = true;
                        }
                    }

                    Ok(())
                })?
            };
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
