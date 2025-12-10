use gc_lite::{GcTracable, GcTracer};

use crate::{context::ContextImpl, error::JSResult, runtime::RuntimeImpl, value::JSValue};

use super::Promise;

pub(crate) struct Job {
    run: Box<dyn FnOnce() -> JSResult>,
    gc_trace: Option<Box<dyn Fn(&mut GcTracer)>>,
}

unsafe impl GcTracable for Job {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        if let Some(f) = self.gc_trace.as_ref() {
            f(tracer)
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
    pub(crate) fn queue_job_with_gc_trace(
        &mut self,
        run: impl FnOnce() -> JSResult + 'static,
        gc_trace: impl Fn(&mut GcTracer) + 'static,
    ) {
        self.job_list.push_back(Job {
            run: Box::new(run),
            gc_trace: Some(Box::new(gc_trace)),
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
        result = ctx.rt().take_exception();
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
        let exc = ctx.rt().take_exception();
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
            while let Some(job) = cx.job_list.pop_front() {
                job.run()?;
            }
        }
        Ok(())
    }

    /// execute pending jobs in queue.
    ///
    /// Returns:
    /// Some(_): the Context of the executed job.
    /// None: there is no pending job.
    /// Err: fail in execute job. ctx is the context of the executed job.
    #[deprecated]
    pub(crate) fn execute_jobs(&mut self) -> Result<Option<()>, i32> {
        let mut has_pending = false;

        //exec context jobs
        for ctx in self.context_list.iter() {
            let n = ctx.job_list.len();
            if n > 0 {
                // execute first job, then trace pending jobs.
                // don't change order.
                let mut cx = *ctx;
                if let Some(job) = cx.job_list.pop_front() {
                    job.run()?;
                }

                if !ctx.job_list.is_empty() {
                    let mut tracer = GcTracer::new();
                    ctx.job_list.iter().for_each(|x| x.trace(&mut tracer));
                    tracer.commit_marks(&self.gc_heap);

                    has_pending = true;
                }
            }
        }

        if has_pending { Ok(Some(())) } else { Ok(None) }
    }
}
