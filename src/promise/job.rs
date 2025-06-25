use crate::{
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_Call},
    list_entry,
    promise::PromiseValue,
    runtime::JSRuntime,
    utils::{UncheckedInto, list_add_tail, list_del, list_empty, list_head},
    value::JSValue,
};

pub type JobCallback = for<'a> fn(ctx: &'a JSContext, args: CallContext<'a>) -> JSResult;

#[repr(C)]
pub struct JSJobEntry {
    pub link: list_head,

    pub ctx: JSContext,
    pub callback: JobCallback,

    /// args array, 5 in max
    pub args: [JSValue; 5],
    pub argc: u8,
}

impl JSContext {
    /// Append a job to queue.
    /// Note: `args` values are not consumed, args count must not greater than 5.
    pub fn enqueue_job(&self, callback: JobCallback, args: &[JSValue]) -> Result<(), i32> {
        debug_assert!(args.len() <= 5);

        let mut e = Box::new(JSJobEntry {
            link: list_head::default(),
            ctx: self.clone(),
            callback,
            args: [const { JSValue::UNDEFINED }; 5],
            argc: args.len() as u8,
        });

        for (i, v) in args.iter().enumerate() {
            e.args[i] = v.clone();
        }

        let ent = Box::into_raw(e);
        unsafe {
            list_add_tail(&mut (*ent).link, &mut self.rt_mut().job_list);
        }

        Ok(())
    }
}

pub fn promise_reaction_job(ctx: &JSContext, args: CallContext) -> JSResult {
    debug_assert_eq!(args.length(), 5);

    let handler = args.arg(2);
    let mut is_reject = args.arg(3).coerce_to_bool(ctx)?;
    let mut arg = args.arg(4);

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
    /// - 0: the Context of the executed job.
    /// - 1: false if there are no jobs left; true if it need more runs.
    pub fn execute_pending_job(
        &mut self,
    ) -> Result<(Option<JSContext>, bool), (Option<JSContext>, i32)> {
        if list_empty(&self.job_list) {
            return Ok((None, false));
        }

        // get the first pending job and execute it
        unsafe {
            let ent = list_entry!(self.job_list.next, JSJobEntry, link);
            list_del(&mut (*ent).link);

            let job = Box::from_raw(ent);
            let mut ctx = job.ctx;

            let res = {
                let cc = CallContext::new(
                    &ctx,
                    JSValue::UNDEFINED,
                    &job.args[..job.argc as usize],
                    0,
                    None,
                );
                (job.callback)(&ctx, cc)
            };

            // free job arguments
            // for i in 0..job.argc {
            //     JS_FreeValue(ctx.into(), job.args[i as usize]);
            // }

            if let Ok(r) = res {
                Ok((Some(ctx), true))
            } else {
                Err((Some(ctx), -1))
            }
        }
    }
}
