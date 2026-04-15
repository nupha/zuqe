// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{ScopedContext, context::ContextImpl, value::JSValue};

use super::Promise;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum JobState {
    Done,
    Again,
}

pub(crate) struct Job {
    runnable: Box<dyn Fn(&mut ScopedContext) -> Result<JobState, i32>>,
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

impl ContextImpl {
    /// Append a job to job queue.
    pub(crate) fn queue_job_with_trace(
        &mut self,
        runnable: impl Fn(&mut ScopedContext) -> Result<JobState, i32> + 'static,
        trace: impl Fn(&mut GcTraceCtx) + 'static,
    ) {
        self.job_list.push_back(Job {
            runnable: Box::new(runnable),
            gc_trace: Some(Box::new(trace)),
        });
    }
}

pub(super) fn promise_reaction_job(
    ctx: &mut ScopedContext,
    resolving: [JSValue; 2],
    handler: JSValue,
    is_reject: bool,
    arg: JSValue,
) -> Result<JobState, i32> {
    let mut result = if handler.is_undefined() {
        if is_reject {
            ctx.throw(arg);
            JSValue::EXCEPTION
        } else {
            arg
        }
    } else {
        handler
            .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&arg))
            .unwrap_or(JSValue::EXCEPTION)
    };

    let is_reject = if result.is_exception() {
        result = ctx.rt_mut().take_exception();
        true
    } else {
        false
    };

    let func = &resolving[is_reject as usize];

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
    then: JSValue,
) -> Result<JobState, i32> {
    let rr = promise.create_resolving_functions(ctx)?;
    if then.call(ctx, thenable, &rr).is_err() {
        let exc = ctx.rt_mut().take_exception();
        rr[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc))?;
    }

    Ok(JobState::Done)
}
