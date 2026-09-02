// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx, GcTraceFn};

use crate::{ScopedContext, context::ContextImpl, value::JSValue};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobState {
    /// Job completed; removed from queue.
    Done,
    /// Job is blocked waiting for an external event (I/O, Timer, Promise
    /// resolve, …). Re-queued for the next tick **and** counts as parked:
    /// the event loop must not exit while a blocked job is pending.
    Block,
    /// Job is still in progress and will be re-queued for the next tick,
    /// but does NOT count as parked — the event loop may go idle (and the
    /// process exit) while this job is pending. Used by promises that only
    /// need polling while observed (awaited / `.then`-ed).
    Continue,
}

/// A polling job running in background, known as 'micro task'.
/// May return `Block` or `Continue` to be re-queued for the next tick.
pub(crate) struct PollingJob {
    /// It's safe to call js functions inside this closure.
    runnable: Box<dyn Fn(&mut ScopedContext) -> Result<JobState, i32>>,
    /// To trace gc nodes that are referenced by this job.
    pub(crate) gc_trace: Option<Box<dyn GcTraceFn>>,
}

impl GcTrace for PollingJob {
    #[inline(always)]
    fn trace(&self, gcx: &mut GcTraceCtx) {
        if let Some(f) = self.gc_trace.as_ref() {
            f(gcx)
        }
    }
}

impl PollingJob {
    #[inline(always)]
    pub fn run(&self, ctx: &mut ScopedContext) -> Result<JobState, i32> {
        (self.runnable)(ctx)
    }
}

/// One-shot job — executed once and always completes; never retried.
pub(crate) struct OneshotJob {
    pub(crate) runnable: Box<dyn Fn(&mut ScopedContext) -> Result<(), i32>>,
    pub(crate) gc_trace: Option<Box<dyn GcTraceFn>>,
}

impl OneshotJob {
    #[inline(always)]
    pub fn run(self, ctx: &mut ScopedContext) -> Result<(), i32> {
        (self.runnable)(ctx)
    }
}

impl GcTrace for OneshotJob {
    #[inline(always)]
    fn trace(&self, gcx: &mut GcTraceCtx) {
        if let Some(f) = self.gc_trace.as_ref() {
            f(gcx)
        }
    }
}

pub trait JobGcTrace {
    fn job_trace_values(&self, gcx: &mut GcTraceCtx);
}

impl JobGcTrace for () {
    #[inline(always)]
    fn job_trace_values(&self, _: &mut GcTraceCtx) {}
}

impl JobGcTrace for JSValue {
    #[inline(always)]
    fn job_trace_values(&self, gcx: &mut GcTraceCtx) {
        self.trace(gcx);
    }
}

impl ContextImpl {
    /// Add a pending micro-task (polling mode, may return `Block` or `Continue`).
    pub fn queue_job<GCN, F>(&mut self, runnable: F, gc_trace_nodes: GCN)
    where
        GCN: JobGcTrace + 'static,
        F: Fn(&mut ScopedContext) -> Result<JobState, i32> + 'static,
    {
        self.polling_jobs.push_back(PollingJob {
            runnable: Box::new(runnable),
            gc_trace: Some(Box::new(move |gcx| gc_trace_nodes.job_trace_values(gcx))),
        });
    }

    /// Add a one-shot micro-task — executed once and always completes; never retried.
    pub fn queue_oneshot_job<GCN, F>(&mut self, runnable: F, gc_trace_nodes: GCN)
    where
        GCN: JobGcTrace + 'static,
        F: Fn(&mut ScopedContext) -> Result<(), i32> + 'static,
    {
        self.oneshot_jobs.push_back(OneshotJob {
            runnable: Box::new(runnable),
            gc_trace: Some(Box::new(move |gcx| gc_trace_nodes.job_trace_values(gcx))),
        });
    }
}
