// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;
use smallvec::SmallVec;

use crate::{
    builtins::promise::{Job, JobState},
    context::{ContextPtr, ScopedContext},
};

use super::RuntimeImpl;

impl<'c> ScopedContext<'c> {
    pub fn add_tick_handler(&mut self, f: fn(&mut ScopedContext) -> bool) {
        self.tick_handlers.push(f);
    }
}

pub enum TickStatus {
    Continue,
    Stop(i32),
}

impl RuntimeImpl {
    pub const fn stop(&mut self, code: i32) {
        self.stop_flag = Some(code);
    }

    pub(crate) fn tick_internal(&mut self) -> Result<TickStatus, i32> {
        if let Some(n) = self.stop_flag.take() {
            return Ok(TickStatus::Stop(n));
        }

        let mut need_more_tick = false;

        for mut ctx in self.context_list.iter().copied() {
            ctx.with_new_scope(|mut scope| -> Result<(), i32> {
                // 1. run pending jobs (not for new added jobs)
                let mut again = SmallVec::<[Job; 8]>::new();

                while let Some(mut job) = scope.job_list.pop_front() {
                    self.gc_cur_job_trace = job.gc_trace.take(); // save job gc tracing temporarily

                    if job.run(&mut scope)? == JobState::Again {
                        // multi-run task: run again in next iter
                        job.gc_trace = self.gc_cur_job_trace.take(); // restore job gc tracing
                        again.push(job);
                    } else {
                        self.gc_cur_job_trace = None;
                    }
                }

                // sched jobs to run again in next round
                if !again.is_empty() {
                    scope.job_list.extend(again);
                    need_more_tick = true;
                }

                // 2. run tick handlers
                let ctx_ptr = ContextPtr::from(scope.gc_ref());
                for f in ctx_ptr.tick_handlers.iter() {
                    if f(&mut scope) {
                        need_more_tick = true;
                    }
                }

                Ok(())
            })?;
        }

        if self.gc_req || self.gc_heap().should_gc() {
            self.gc_collect();
        }

        Ok(if need_more_tick {
            TickStatus::Continue
        } else {
            TickStatus::Stop(0)
        })
    }

    #[cfg(not(feature = "async_support"))]
    #[inline(always)]
    pub fn tick(&mut self) -> Result<TickStatus, i32> {
        self.tick_internal()
    }
}
