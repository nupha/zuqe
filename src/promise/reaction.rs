use std::ops::DerefMut;

use crate::{
    atom::consts::JS_ATOM_then,
    context::JSContext,
    error::JSResult,
    function::{CallArgs, JS_Call, JS_Invoke},
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{list_add_tail, list_head},
    value::{
        object::{JS_GetOpaque, JS_GetOpaque2, JS_SpeciesConstructor},
        JSValue, JS_FreeValue, JS_FreeValueRT,
    },
    JS_CLASS_PROMISE,
};

use super::{
    job::promise_reaction_job, js_promise_resolve, JSPromiseData, JSPromiseStateEnum, PromiseValue,
};

pub struct JSPromiseReactionData {
    // not used in promise_reaction_job
    pub link: list_head,

    pub resolving_funcs: [JSValue; 2],
    pub handler: JSValue,
}

impl JSPromiseReactionData {
    pub(super) fn free(self: Box<Self>, rt: *mut JSRuntime) {
        unsafe {
            JS_FreeValueRT(rt, self.resolving_funcs[0]);
            JS_FreeValueRT(rt, self.resolving_funcs[1]);
            JS_FreeValueRT(rt, self.handler);
        }
    }
}

impl PromiseValue {
    /// Create chained promise of `then()` reaction.
    /// `cap_resolving_funcs` - thrownawayCapability.
    /// `resolve_reject` and `cap_resolving_funcs` are not consumed.
    pub unsafe fn then(
        self,
        ctx: &mut JSContext,
        resolve_reject: &[JSValue; 2],
        cap_resolving_funcs: &[JSValue; 2],
    ) -> Result<(), i32> {
        let s = JS_GetOpaque(self.0, JS_CLASS_PROMISE).cast::<JSPromiseData>();
        let mut rd_array: [*mut JSPromiseReactionData; 2] = [std::ptr::null_mut(); 2];

        for i in 0..2 {
            let mut rd = if let Some(p) = js_malloc_typed::<JSPromiseReactionData>(ctx, 1, true) {
                Box::from_raw(p.as_ptr())
            } else {
                if i == 1 {
                    Box::from_raw(rd_array[0]).free(ctx.rt_mut());
                }
                return Err(-1);
            };

            rd.resolving_funcs[0] = cap_resolving_funcs[0].dup();
            rd.resolving_funcs[1] = cap_resolving_funcs[1].dup();

            let mut handler = resolve_reject[i];
            if !handler.is_function(ctx) {
                handler = JSValue::UNDEFINED;
            }

            rd.handler = handler.dup();
            rd_array[i] = Box::into_raw(rd);
        }

        if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_PENDING {
            list_add_tail(&mut (*rd_array[0]).link, &mut (*s).promise_reactions[0]);
            list_add_tail(&mut (*rd_array[1]).link, &mut (*s).promise_reactions[1]);
        } else {
            if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !(*s).is_handled {
                if let Some(cb) = ctx.rt().host_promise_rejection_tracker {
                    cb(
                        ctx,
                        self,
                        (*s).promise_result,
                        true as i32,
                        ctx.rt().host_promise_rejection_tracker_opaque,
                    );
                }
            }

            let i = if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
                0 // resolve
            } else {
                1 // reject
            };
            let rd = rd_array[i as usize];
            let args: [JSValue; 5] = [
                (*rd).resolving_funcs[0],
                (*rd).resolving_funcs[1],
                (*rd).handler,
                if i == 0 {
                    JSValue::FALSE
                } else {
                    JSValue::TRUE
                },
                (*s).promise_result,
            ];
            ctx.enqueue_job(promise_reaction_job, &args).unwrap();

            Box::from_raw(rd_array[0]).free(ctx.rt_mut());
            Box::from_raw(rd_array[1]).free(ctx.rt_mut());
        }

        (*s).is_handled = true;
        Ok(())
    }
}

//
// Reaction methods for Promise instance
//

pub unsafe fn js_promise_then(
    ctx: *mut JSContext,
    this_val: JSValue, // is actually a PromiseValue
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let s = JS_GetOpaque2(ctx, this_val, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    if s.is_null() {
        return JSValue::EXCEPTION;
    }

    let ctor = JS_SpeciesConstructor(ctx, this_val, JSValue::UNDEFINED).into_rc((*ctx).rt());
    if ctor.is_exception() {
        return JSValue::EXCEPTION;
    }

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(*ctor) {
        (pr, rr)
    } else {
        return JSValue::EXCEPTION;
    };

    let ret = this_val.as_promise_unchecked().then(
        ctx.as_mut().unwrap(),
        &[*argv.add(0), *argv.add(1)],
        &resolving_funcs,
    );
    JS_FreeValue(ctx, resolving_funcs[0]);
    JS_FreeValue(ctx, resolving_funcs[1]);

    if ret.is_err() {
        JS_FreeValue(ctx, result_promise);
        JSValue::EXCEPTION
    } else {
        result_promise
    }
}

pub unsafe fn js_promise_catch(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    JS_Invoke(
        ctx.as_mut().unwrap(),
        this_val,
        JS_ATOM_then,
        2,
        [JSValue::UNDEFINED, JSValue::UNDEFINED].as_mut_ptr(),
    )
}

pub fn js_promise_then_finally_func(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let ctor = args.data_raw(0);
    let onFinally = args.data_raw(1);

    let mut res = unsafe {
        JS_Call(ctx, onFinally, JSValue::UNDEFINED, 0, std::ptr::null_mut())
            .check_exception()?
            .into_rc(ctx.rt())
    };

    let promise = unsafe {
        js_promise_resolve(ctx, ctor, 1, res.deref_mut(), 0)
            .check_exception()?
            .into_rc(ctx.rt())
    };

    let mut then_func = ctx
        .create_native_callable_object(
            |cx: &mut JSContext, cc: CallArgs| -> JSResult {
                let val = cc.data_raw(0).dup();
                if cc.magic == 0 {
                    val.check_exception() // resolve
                } else {
                    cx.JS_Throw(val); // reject
                    Err(-1)
                }
            },
            1,
            args.magic,
            &[args.get_raw(0)], // data: argument#0
        )?
        .into_rc(ctx.rt());

    // call promise.then(resolve)
    unsafe { JS_Invoke(ctx, *promise, JS_ATOM_then, 1, then_func.deref_mut()).check_exception() }
}

pub unsafe fn js_promise_finally(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let on_finally = *argv.offset(0);

    let ctor = JS_SpeciesConstructor(ctx, this_val, JSValue::UNDEFINED);
    if ctor.is_exception() {
        return ctor;
    }

    let mut then_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    if !on_finally.is_function(ctx.as_ref().unwrap()) {
        then_funcs = [on_finally.dup(), on_finally.dup()];
    } else {
        let func_data = [ctor, on_finally];
        for i in 0..2 {
            if let Ok(f) = (*ctx).create_native_callable_object(
                js_promise_then_finally_func,
                1,
                i as _,
                &func_data,
            ) {
                then_funcs[i] = f;
            } else {
                if i != 0 {
                    JS_FreeValue(ctx, then_funcs[0]);
                }
                JS_FreeValue(ctx, ctor);
                return JSValue::EXCEPTION;
            }
        }
    }

    JS_FreeValue(ctx, ctor);

    let ret = JS_Invoke(
        ctx.as_mut().unwrap(),
        this_val,
        JS_ATOM_then,
        2,
        then_funcs.as_mut_ptr(),
    );
    JS_FreeValue(ctx, then_funcs[0]);
    JS_FreeValue(ctx, then_funcs[1]);

    ret
}
