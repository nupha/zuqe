use std::ops::DerefMut;

use crate::{
    context::{JSContext, JSContextRef},
    function::JS_Call,
    list_entry,
    runtime::JSRuntime,
    types::{list_add_tail, list_del, list_empty, list_head},
    value::{JSValue, JSValueConst, JS_FreeValue, JS_ToBool},
};

pub type JSJobFunc = unsafe fn(ctx: &mut JSContext, argc: i32, argv: *mut JSValueConst) -> JSValue;

#[repr(C)]
pub struct JSJobEntry {
    pub link: list_head,

    pub ctx: JSContextRef,
    pub job_func: JSJobFunc,

    pub args: [JSValue; 5],
    pub argc: u8,
}

impl JSContext {
    /// Append a job to queue.
    /// Note: `args` values are not consumed.
    pub unsafe fn enqueue_job(&mut self, job_func: JSJobFunc, args: &[JSValue]) -> Result<(), i32> {
        debug_assert!(args.len() <= 5);

        let mut e = Box::new(JSJobEntry {
            link: list_head::default(),
            ctx: self.into(),
            job_func,
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

pub unsafe fn promise_reaction_job(
    ctx: &mut JSContext,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    debug_assert_eq!(argc, 5);

    let handler = *argv.offset(2);
    let mut is_reject = JS_ToBool(ctx, *argv.offset(3)).unwrap();
    let arg = *argv.offset(4);

    #[cfg(DUMP_PROMISE)]
    println!("promise_reaction_job: is_reject={}", is_reject);

    let mut res = if handler.is_undefined() {
        if is_reject {
            ctx.JS_Throw(arg.dup())
        } else {
            arg.dup()
        }
    } else {
        JS_Call(ctx, handler, JSValue::UNDEFINED, 1, [arg].as_mut_ptr())
    };

    is_reject = res.is_exception();
    if is_reject {
        res = ctx.get_exception();
    }

    let func = *argv.offset(is_reject as isize); // resolve or reject function

    // as an extension, we support undefined as value to avoid
    // creating a dummy promise in the 'await' implementation of async
    // functions
    let res2 = if !func.is_undefined() {
        JS_Call(ctx, func, JSValue::UNDEFINED, 1, [res].as_mut_ptr())
    } else {
        JSValue::UNDEFINED
    };
    JS_FreeValue(ctx, res);

    res2
}

pub unsafe fn js_promise_resolve_thenable_job(
    ctx: &mut JSContext,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    debug_assert_eq!(argc, 3);

    #[cfg(feature = "DUMP_PROMISE")]
    println!("js_promise_resolve_thenable_job");

    let promise = (*argv.offset(0)).to_promise_unchecked();
    let thenable = *argv.offset(1);
    let then = *argv.offset(2);
    let mut args = if let Ok(f) = promise.create_resolving_functions(ctx) {
        f
    } else {
        return JSValue::EXCEPTION;
    };

    let res = JS_Call(ctx, then, thenable, 2, args.as_mut_ptr());
    if res.is_exception() {
        let error = ctx.get_exception();
        JS_Call(
            ctx,
            args[1], // reject
            JSValue::UNDEFINED,
            1,
            [error].as_mut_ptr(),
        );
        JS_FreeValue(ctx, error);
    }

    JS_FreeValue(ctx, args[0]);
    JS_FreeValue(ctx, args[1]);

    res
}

impl JSRuntime {
    /// take and execute first pending job (only one job) in queue.
    /// return #0: the JSContext of the executed job.
    /// return #1: true if it need more runs, false if there are no jobs left.
    pub fn execute_pending_job(
        &mut self,
    ) -> Result<(Option<JSContextRef>, bool), (Option<JSContextRef>, i32)> {
        if list_empty(&self.job_list) {
            return Ok((None, false));
        }

        // get the first pending job and execute it
        unsafe {
            let e = list_entry!(self.job_list.next, JSJobEntry, link);
            list_del(&mut (*e).link);

            let mut ctx = (*e).ctx;
            let res = ((*e).job_func)(ctx.deref_mut(), (*e).argc as _, (*e).args.as_mut_ptr());
            for i in 0..(*e).argc {
                JS_FreeValue(ctx.as_raw(), (*e).args[i as usize]);
            }

            JS_FreeValue(ctx.as_raw(), res);
            drop(Box::from_raw(e));

            res.check_exception().map_err(|_| (Some(ctx), -1))?;

            Ok((Some(ctx), true))
        }
    }
}

/// return < 0 if exception, 0 if no job pending, 1 if a job was
/// executed successfully. the context of the job is stored in '*pctx'
pub unsafe fn JS_ExecutePendingJob(rt: &mut JSRuntime, pctx: *mut *mut JSContext) -> i32 {
    if list_empty(&rt.job_list) {
        *pctx = std::ptr::null_mut();
        return 0;
    }

    // get the first pending job and execute it
    let e = list_entry!(rt.job_list.next, JSJobEntry, link);
    list_del(&mut (*e).link);

    let mut ctx = (*e).ctx;
    let res = ((*e).job_func)(ctx.deref_mut(), (*e).argc as _, (*e).args.as_mut_ptr());
    for i in 0..(*e).argc {
        JS_FreeValue(ctx.as_raw(), (*e).args[i as usize]);
    }
    let ret = if res.is_exception() { -1 } else { 1 };

    JS_FreeValue(ctx.as_raw(), res);
    drop(Box::from_raw(e));

    *pctx = ctx.as_raw();
    ret
}
