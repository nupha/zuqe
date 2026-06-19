// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

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

pub(super) enum TickStatus {
    /// More jobs to process — call tick() again.
    Continue,
    /// Event loop stopped normally (or interrupted — check interrupt_code).
    Stop,
}

impl RuntimeImpl {
    /// Run background jobs for one round.
    pub(super) fn tick(&mut self) -> TickStatus {
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
                            // Per ES §9.5: isolate job error, continue to next job.
                            log::warn!(
                                "[task error] one-shot job failed: {:?}",
                                self.cur_exception
                            );
                            self.cur_exception = Exception::No;
                            continue;
                        }
                        // Interrupt detected — stop processing more jobs.
                        // The catch-all below will return Err(-1) from tick().
                        if self.interrupt_code.is_some() {
                            break;
                        }
                    }

                    self.gc_job = None;
                }

                // 2. Run polling jobs (may return `Again`) — one round only.
                //    Skip if interrupt is pending.
                if self.interrupt_code.is_none() {
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
                                log::warn!(
                                    "[task error] polling job failed: {:?}",
                                    self.cur_exception
                                );
                                self.cur_exception = Exception::No;
                                // Don't re-queue — treat as Done.
                            }
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

                cx.return_with(())
            });
        }

        // ES §9.5 ensures all job exceptions are isolated inside scopes.
        debug_assert_eq!(self.cur_exception, Exception::No);

        // Interrupt check — independent from exception.
        if self.interrupt_code.is_some() {
            return TickStatus::Stop;
        }

        if self.gc_req || self.gc_heap().should_gc() {
            self.gc_collect();
        }

        if more_ticks {
            TickStatus::Continue
        } else {
            TickStatus::Stop
        }
    }
}
