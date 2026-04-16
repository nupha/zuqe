// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{ScopedContext, context::ContextImpl, value::JSValue};

use super::{Promise, PromiseReaction, PromiseResolveFunc};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum JobState {
    Done,
    Again,
}

/// A job running in background, known as 'micro task'.
pub(crate) struct Job {
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

pub(crate) trait JobGcTrace {
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

impl JobGcTrace for PromiseReaction {
    #[inline(always)]
    fn trace_values(&self, tr: &mut GcTraceCtx) {
        self.trace(tr);
    }
}

impl JobGcTrace for PromiseResolveFunc {
    #[inline(always)]
    fn trace_values(&self, tr: &mut GcTraceCtx) {
        self.trace(tr);
    }
}

pub(crate) struct JobGcTrace2<A, B> {
    pub a: A,
    pub b: B,
}

impl<A, B> JobGcTrace for JobGcTrace2<A, B>
where
    A: JobGcTrace,
    B: JobGcTrace,
{
    #[inline(always)]
    fn trace_values(&self, tr: &mut GcTraceCtx) {
        self.a.trace_values(tr);
        self.b.trace_values(tr);
    }
}

pub(crate) struct JobGcTrace3<A, B, C> {
    pub a: A,
    pub b: B,
    pub c: C,
}

impl<A, B, C> JobGcTrace for JobGcTrace3<A, B, C>
where
    A: JobGcTrace,
    B: JobGcTrace,
    C: JobGcTrace,
{
    #[inline(always)]
    fn trace_values(&self, tr: &mut GcTraceCtx) {
        self.a.trace_values(tr);
        self.b.trace_values(tr);
        self.c.trace_values(tr);
    }
}

pub(crate) struct JobGcTrace4<A, B, C, D> {
    pub a: A,
    pub b: B,
    pub c: C,
    pub d: D,
}

impl<A, B, C, D> JobGcTrace for JobGcTrace4<A, B, C, D>
where
    A: JobGcTrace,
    B: JobGcTrace,
    C: JobGcTrace,
    D: JobGcTrace,
{
    #[inline(always)]
    fn trace_values(&self, tr: &mut GcTraceCtx) {
        self.a.trace_values(tr);
        self.b.trace_values(tr);
        self.c.trace_values(tr);
        self.d.trace_values(tr);
    }
}

impl ContextImpl {
    pub(crate) fn queue_job<GCN, F>(&mut self, runnable: F, gc_trace_nodes: GCN)
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

/// Execute a Promise reaction job for a settled Promise.
///
/// # Description
///
/// The originating Promise is guaranteed to be already fulfilled or rejected
/// when this function is called; this function is never invoked while the Promise is pending.
/// So, this is a oneshot microtask, it won't schedule any new jobs.
pub(super) fn js_promise_reaction_job(
    ctx: &mut ScopedContext,
    action: PromiseReaction,
    arg: JSValue,
    is_reject: bool,
) -> Result<JobState, i32> {
    let mut result = if action.handler.is_undefined() {
        if is_reject {
            ctx.throw(arg);
            JSValue::EXCEPTION
        } else {
            arg
        }
    } else {
        action
            .handler
            .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&arg))
            .unwrap_or(JSValue::EXCEPTION)
    };

    let is_reject = if result.is_exception() {
        result = ctx.rt_mut().take_exception();
        true
    } else {
        false
    };

    let func = &action.resolving_funcs[is_reject as usize];

    // as an extension, we support undefined as value to avoid
    // creating a dummy promise in the 'await' implementation of async functions
    if !func.is_undefined() {
        func.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&result))?;
    }

    Ok(JobState::Done)
}

pub(super) fn js_promise_resolve_thenable_job(
    ctx: &mut ScopedContext,
    promise: Promise,
    thenable: JSValue,
    then_func: JSValue,
) -> Result<JobState, i32> {
    let rr = promise.create_resolving_functions(ctx)?;
    if then_func.call(ctx, thenable, &rr).is_err() {
        let exc = ctx.rt_mut().take_exception();
        rr[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc))?;
    }

    Ok(JobState::Done)
}
