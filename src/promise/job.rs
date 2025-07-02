use crate::{
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_Call},
    promise::PromiseValue,
    runtime::JSRuntime,
    utils::UncheckedInto,
    value::JSValue,
};

pub type JobCallback = for<'a> fn(ctx: &'a JSContext, args: CallContext<'a>) -> JSResult;

pub struct JSJobEntry {
    pub ctx: JSContext,
    pub callback: JobCallback,

    /// args array, 5 in max
    pub args: [JSValue; 5],
    pub argc: u8,
}

impl JSContext {
    /// Append a job to queue.
    /// Note: `args` values are not consumed, args count must not greater than 5.
    pub(super) fn queue_job(&self, callback: JobCallback, args: &[JSValue]) -> Result<(), i32> {
        debug_assert!(args.len() < 6);

        let mut job = Box::new(JSJobEntry {
            ctx: self.clone(),
            callback,
            args: [const { JSValue::UNDEFINED }; 5],
            argc: args.len() as u8,
        });

        for (i, v) in args.iter().enumerate() {
            job.args[i] = v.clone();
        }

        self.rt_mut().job_list.push_back(job);

        Ok(())
    }
}

pub fn promise_reaction_job(ctx: &JSContext, args: CallContext) -> JSResult {
    debug_assert_eq!(args.length(), 5);

    let handler = args.arg(2);
    let mut is_reject = args.arg(3).coerce_to_bool(ctx)?;
    let arg = args.arg(4);

    #[cfg(DUMP_PROMISE)]
    println!("promise_reaction_job: is_reject={}", is_reject);

    let mut result = if handler.is_undefined() {
        if is_reject {
            ctx.throw(arg.clone());
            JSValue::EXCEPTION
        } else {
            arg.clone()
        }
    } else {
        JS_Call(ctx, handler, &JSValue::UNDEFINED, std::slice::from_ref(arg))
    };

    is_reject = result.is_exception();
    if is_reject {
        result = ctx.take_exception();
    }

    let func = args.arg(is_reject as usize); // resolve or reject function

    // as an extension, we support undefined as value to avoid
    // creating a dummy promise in the 'await' implementation of async functions
    if !func.is_undefined() {
        JS_Call(ctx, func, &JSValue::UNDEFINED, &[result]).check_exception()
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub fn js_promise_resolve_thenable_job(ctx: &JSContext, cc: CallContext) -> JSResult {
    debug_assert_eq!(cc.length(), 3);

    #[cfg(feature = "DUMP_PROMISE")]
    println!("js_promise_resolve_thenable_job");

    let promise: PromiseValue = unsafe { cc.get_raw(0).unchecked_into() };
    let thenable = cc.arg(1);
    let then = cc.arg(2);
    let rr = promise.create_resolving_functions(ctx)?;

    let res = JS_Call(ctx, then, thenable, &rr);
    if res.is_exception() {
        let error = ctx.take_exception();
        JS_Call(
            ctx,
            &rr[1], // reject
            &JSValue::UNDEFINED,
            &[error],
        );
    }

    res.check_exception()
}

impl JSRuntime {
    /// take and execute first pending job in queue.
    /// this will only execute one job, keep rest jobs in queue.
    ///
    /// Returns:
    /// Some(ctx): the Context of the executed job.
    /// None: there is no pending job.
    /// Err: fail in execute job. ctx is the context of the executed job.
    pub fn execute_job_once(&mut self) -> Result<Option<JSContext>, (JSContext, i32)> {
        // fetch the first pending job and execute it
        if let Some(job) = self.job_list.pop_front() {
            let res = {
                let cc = CallContext::new(
                    &job.ctx,
                    JSValue::UNDEFINED,
                    &job.args[..job.argc as usize],
                    0,
                    None,
                );
                (job.callback)(&job.ctx, cc)
            };

            if res.is_ok() {
                Ok(Some(job.ctx))
            } else {
                Err((job.ctx, -1))
            }
        } else {
            Ok(None) // no job to run
        }
    }
}
