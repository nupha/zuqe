use std::ops::DerefMut;

use crate::{
    context::{JSContext, JSContextHandle},
    error::JSResult,
    function::{CallArgs, JS_Call},
    list_entry,
    runtime::JSRuntime,
    types::{list_add_tail, list_del, list_empty, list_head},
    value::{JSValue, JS_FreeValue, JS_ToBool},
};

pub type JobCallback = for<'a> fn(ctx: &'a mut JSContext, args: CallArgs<'a>) -> JSResult;

#[repr(C)]
pub struct JSJobEntry {
    pub link: list_head,

    pub ctx: JSContextHandle,
    pub callback: JobCallback,

    /// args array, 5 in max
    pub args: [JSValue; 5],
    pub argc: u8,
}

impl JSContext {
    /// Append a job to queue.
    /// Note: `args` values are not consumed (dup inside).
    pub unsafe fn enqueue_job(
        &mut self,
        callback: JobCallback,
        args: &[JSValue],
    ) -> Result<(), i32> {
        debug_assert!(args.len() <= 5);

        let mut e = Box::new(JSJobEntry {
            link: list_head::default(),
            ctx: self.handle(),
            callback,
            args: [JSValue::UNDEFINED; 5],
            argc: args.len() as u8,
        });

        for (i, v) in args.iter().enumerate() {
            e.args[i] = v.dup();
        }

        let ent = Box::into_raw(e);
        list_add_tail(&mut (*ent).link, &mut self.rt_mut().job_list);

        Ok(())
    }
}

pub fn promise_reaction_job(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    debug_assert_eq!(args.length(), 5);

    let handler = args.get_raw(2);
    let mut is_reject = unsafe { JS_ToBool(ctx, args.get_raw(3)).unwrap() };
    let mut arg = args.get_raw(4);

    #[cfg(DUMP_PROMISE)]
    println!("promise_reaction_job: is_reject={}", is_reject);

    let mut res = if handler.is_undefined() {
        if is_reject {
            ctx.JS_Throw(arg.dup())
        } else {
            arg.dup()
        }
    } else {
        unsafe { JS_Call(ctx, handler, JSValue::UNDEFINED, 1, &mut arg) }
    };

    is_reject = res.is_exception();
    if is_reject {
        res = ctx.get_exception();
    }

    let func = args.get_raw(is_reject as usize); // resolve or reject function

    // as an extension, we support undefined as value to avoid
    // creating a dummy promise in the 'await' implementation of async
    // functions
    let res2 = if !func.is_undefined() {
        unsafe { JS_Call(ctx, func, JSValue::UNDEFINED, 1, &mut res) }
    } else {
        JSValue::UNDEFINED
    };

    unsafe {
        JS_FreeValue(ctx, res);
    }

    res2.check_exception()
}

pub fn js_promise_resolve_thenable_job(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    debug_assert_eq!(args.length(), 3);

    #[cfg(feature = "DUMP_PROMISE")]
    println!("js_promise_resolve_thenable_job");

    let promise = unsafe { args.get_raw(0).as_promise_unchecked() };
    let thenable = args.get_raw(1);
    let then = args.get_raw(2);
    let mut rr = promise.create_resolving_functions(ctx)?;

    let res = unsafe { JS_Call(ctx, then, thenable, 2, rr.as_mut_ptr()) };
    if res.is_exception() {
        let error = ctx.get_exception();
        unsafe {
            JS_Call(
                ctx,
                rr[1], // reject
                JSValue::UNDEFINED,
                1,
                [error].as_mut_ptr(),
            );
            JS_FreeValue(ctx, error);
        }
    }

    unsafe {
        JS_FreeValue(ctx, rr[0]);
        JS_FreeValue(ctx, rr[1]);
    }

    res.check_exception()
}

impl JSRuntime {
    /// take and execute first pending job (only one job) in queue.
    /// return #0: the JSContext of the executed job.
    /// return #1: true if it need more runs, false if there are no jobs left.
    pub fn execute_pending_job(
        &mut self,
    ) -> Result<(Option<JSContextHandle>, bool), (Option<JSContextHandle>, i32)> {
        if list_empty(&self.job_list) {
            return Ok((None, false));
        }

        // get the first pending job and execute it
        unsafe {
            let ent = list_entry!(self.job_list.next, JSJobEntry, link);
            list_del(&mut (*ent).link);

            let job = Box::from_raw(ent);

            let mut ctx = job.ctx;
            let cc = CallArgs::new(
                ctx,
                JSValue::UNDEFINED,
                &job.args[..job.argc as usize],
                0,
                None,
            );
            let res = ((*job).callback)(ctx.deref_mut(), cc);
            for i in 0..(*job).argc {
                JS_FreeValue(ctx.deref_mut(), (*job).args[i as usize]);
            }
            if let Ok(r) = res {
                JS_FreeValue(ctx.deref_mut(), r);
                Ok((Some(ctx), true))
            } else {
                Err((Some(ctx), -1))
            }
        }
    }
}
