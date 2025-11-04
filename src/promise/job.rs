use crate::{
    context::JSContext, error::JSResult, promise::Promise, runtime::JSRuntime, value::JSValue,
};

#[repr(transparent)]
pub(crate) struct Job<'a>(Box<dyn FnOnce() -> JSResult<'a> + 'a>);

impl<'a> Job<'a> {
    #[inline(always)]
    pub fn run(self) -> JSResult<'a> {
        (self.0)()
    }
}

impl<'a> JSContext<'a> {
    /// Append a job to job queue.
    pub(super) fn queue_job(&self, run: impl FnOnce() -> JSResult<'a> + 'a) {
        self.0.borrow_mut().job_list.push_back(Job(Box::new(run)));
    }
}

pub(super) fn promise_reaction_job<'a>(
    ctx: &JSContext<'a>,
    resolving: [JSValue<'a>; 2],
    handler: JSValue<'a>,
    is_reject: bool,
    arg: JSValue<'a>,
) -> JSResult<'a> {
    let mut result = if handler.is_undefined() {
        if is_reject {
            ctx.throw(arg.clone());
            JSValue::EXCEPTION
        } else {
            arg.clone()
        }
    } else {
        handler
            .call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&arg))
            .unwrap_or(JSValue::EXCEPTION)
    };

    let is_reject = if result.is_exception() {
        result = ctx.take_exception();
        true
    } else {
        false
    };

    let func = &resolving[is_reject as usize];

    // as an extension, we support undefined as value to avoid
    // creating a dummy promise in the 'await' implementation of async functions
    if !func.is_undefined() {
        func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&result))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub(super) fn js_promise_resolve_thenable_job<'a>(
    ctx: &JSContext<'a>,
    promise: Promise<'a>,
    thenable: JSValue<'a>,
    then: JSValue<'a>,
) -> JSResult<'a> {
    let rr = promise.create_resolving_functions(ctx)?;
    if let Ok(r) = then.call(ctx, &thenable, &rr) {
        Ok(r)
    } else {
        let exc = ctx.take_exception();
        rr[1].call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&exc))
    }
}

impl<'r> JSRuntime<'r> {
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
        for ctx in self.context_list.iter() {
            let n = ctx.0.borrow().job_list.len();
            if n > 0 {
                for _ in 0..n {
                    if let Some(job) = ctx.0.borrow_mut().job_list.pop_front() {
                        job.run()?;
                    }
                }
                if !ctx.0.borrow().job_list.is_empty() {
                    has_pending = true;
                }
            }
        }

        if has_pending { Ok(Some(())) } else { Ok(None) }
    }
}
