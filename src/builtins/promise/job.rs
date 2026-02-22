// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTracable, GcTraceCtx};

use crate::{context::ContextImpl, error::JSResult, runtime::RuntimeImpl, value::JSValue};

use super::Promise;

pub(crate) struct Job {
    run: Box<dyn FnOnce() -> JSResult>,
    gc_trace: Option<Box<dyn Fn(&mut GcTraceCtx)>>,
}

unsafe impl GcTracable for Job {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(f) = self.gc_trace.as_ref() {
            f(tr)
        }
    }
}

impl Job {
    #[inline(always)]
    pub fn run(self) -> JSResult {
        (self.run)()
    }
}

impl ContextImpl {
    /// Append a job to job queue.
    pub(crate) fn queue_job_with_trace(
        &mut self,
        run: impl FnOnce() -> JSResult + 'static,
        trace: impl Fn(&mut GcTraceCtx) + 'static,
    ) {
        self.job_list.push_back(Job {
            run: Box::new(run),
            gc_trace: Some(Box::new(trace)),
        });
    }
}

pub(super) fn promise_reaction_job(
    ctx: &mut ContextImpl,
    resolving: [JSValue; 2],
    handler: JSValue,
    is_reject: bool,
    arg: JSValue,
) -> JSResult {
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
        result = ctx.rt_ptr().take_exception();
        true
    } else {
        false
    };

    let func = &resolving[is_reject as usize];

    // as an extension, we support undefined as value to avoid
    // creating a dummy promise in the 'await' implementation of async functions
    if !func.is_undefined() {
        func.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&result))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub(super) fn js_promise_resolve_thenable_job(
    ctx: &ContextImpl,
    promise: Promise,
    thenable: JSValue,
    then: JSValue,
) -> JSResult {
    let rr = promise.create_resolving_functions(ctx)?;
    if let Ok(r) = then.call(ctx, thenable, &rr) {
        Ok(r)
    } else {
        let exc = ctx.rt_ptr().take_exception();
        rr[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc))
    }
}

impl RuntimeImpl {
    /// Execute all jobs in queue.
    ///
    /// Returns Err when has fail in executing job,
    /// remainder jobs are still in queue in this case.
    pub(crate) fn execute_all_jobs(&mut self) -> Result<(), i32> {
        for ctx in self.context_list.iter() {
            let mut cx = *ctx;
            while let Some(mut job) = cx.job_list.pop_front() {
                self.gc_cur_job_trace = job.gc_trace.take();
                job.run()?;
                self.gc_cur_job_trace = None;
            }
        }
        Ok(())
    }
}
