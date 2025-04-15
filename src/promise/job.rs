use crate::{
    context::{JSContext, JSContextRef},
    function::JS_Call,
    types::{list_add_tail, list_head},
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
    /// Note: `args` element values will be duplicated inside.
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

/// return 0 if OK, < 0 if exception.
#[deprecated(note = "use JSContext::enqueue_job() instead")]
pub unsafe fn JS_EnqueueJob(
    ctx: *mut JSContext,
    job_func: *mut JSJobFunc,
    argc: i32,
    argv: *const JSValueConst,
) -> i32 {
    unreachable!()
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
