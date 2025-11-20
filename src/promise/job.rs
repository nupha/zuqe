use crate::{
    context::ContextImpl, error::JSResult, promise::Promise, runtime::RuntimeImpl, value::JSValue,
};

#[repr(transparent)]
pub(crate) struct Job(Box<dyn FnOnce() -> JSResult>);

impl Job {
    #[inline(always)]
    pub fn run(self) -> JSResult {
        (self.0)()
    }
}

impl ContextImpl {
    /// Append a job to job queue.
    pub(super) fn queue_job(&mut self, run: impl FnOnce() -> JSResult + 'static) {
        self.job_list.push_back(Job(Box::new(run)));
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
            ctx.throw(arg.clone());
            JSValue::EXCEPTION
        } else {
            arg.clone()
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
    /// execute pending jobs in queue.
    ///
    /// Returns:
    /// Some(_): the Context of the executed job.
    /// None: there is no pending job.
    /// Err: fail in execute job. ctx is the context of the executed job.
    pub fn execute_job(&mut self) -> Result<Option<()>, i32> {
        let mut has_pending = false;

        // exec runtime jobs
        let n = self.job_list.len();
        if n > 0 {
            for _ in 0..n {
                if let Some(job) = self.job_list.pop_front() {
                    job.run()?;
                }
            }
            if !self.job_list.is_empty() {
                has_pending = true;
            }
        }

        //exec context jobs
        for ctx in self.context_list.iter_mut() {
            let n = ctx.job_list.len();
            if n > 0 {
                for _ in 0..n {
                    if let Some(job) = ctx.job_list.pop_front() {
                        job.run()?;
                    }
                }
                if !ctx.job_list.is_empty() {
                    has_pending = true;
                }
            }
        }

        if has_pending { Ok(Some(())) } else { Ok(None) }
    }
}
