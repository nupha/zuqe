// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::process::ExitCode;

use gc_lite::GcNode;
use smallvec::SmallVec;

use crate::{
    Exception,
    context::{ContextPtr, ScopedContext},
    microtask::{Job, JobState},
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
