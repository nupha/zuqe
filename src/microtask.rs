// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{ScopedContext, context::ContextImpl, value::JSValue};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobState {
    Done,
    Again,
}

/// A job running in background, known as 'micro task'.
pub(crate) struct Job {
    /// It's safe to call js functions inside this closure.
    runnable: Box<dyn Fn(&mut ScopedContext) -> Result<JobState, i32>>,

    /// To trace gc nodes that are referenced by this job.
    pub(crate) gc_trace: Option<Box<dyn Fn(&mut GcTraceCtx)>>,
}

impl GcTrace for Job {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(f) = self.gc_trace.as_ref() {
            f(tr)
        }
    }
}

impl Job {
    #[inline(always)]
    pub fn run(&self, ctx: &mut ScopedContext) -> Result<JobState, i32> {
        (self.runnable)(ctx)
    }
}

pub trait JobGcTrace {
    fn trace_values(&self, tr: &mut GcTraceCtx);
}

impl JobGcTrace for () {
    #[inline(always)]
    fn trace_values(&self, _tr: &mut GcTraceCtx) {}
}

impl JobGcTrace for JSValue {
    #[inline(always)]
    fn trace_values(&self, tr: &mut GcTraceCtx) {
        self.trace(tr);
    }
}

impl ContextImpl {
    pub fn queue_job<GCN, F>(&mut self, runnable: F, gc_trace_nodes: GCN)
    where
        GCN: JobGcTrace + 'static,
        F: Fn(&mut ScopedContext) -> Result<JobState, i32> + 'static,
    {
        self.job_list.push_back(Job {
            runnable: Box::new(runnable),
            gc_trace: Some(Box::new(move |tr| gc_trace_nodes.trace_values(tr))),
        });
    }
}
