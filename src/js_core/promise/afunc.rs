use crate::{
    context::JSContext,
    function::{
        async_func_free, async_func_init, async_func_resume, js_async_function_resolve_create,
        JSAsyncFunctionState, JSCFunctionListEntry, JS_Call,
    },
    promise::perform_promise_then,
    value::{object::JS_PROP_CONFIGURABLE, JSValue, JSValueConst, JS_FreeValue},
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_PROP_STRING_DEF,
};

use super::js_promise_resolve;

pub unsafe fn js_async_function_resolve_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let p = func_obj.object();
    let s = p.u.async_function_data;
    let is_reject = (p.class_id() - JS_CLASS_ASYNC_FUNCTION_RESOLVE) != 0;

    let arg = if argc > 0 {
        *argv.offset(0)
    } else {
        JSValue::UNDEFINED
    };

    (*s).throw_flag = is_reject;
    if is_reject {
        ctx.JS_Throw(arg.dup());
    } else {
        // return value of await
        *(*s)
            .frame
            .stack_base_ptr()
            .as_ptr()
            .add((*s).frame.cur_sp_offset - 1) = arg.dup();
    }

    js_async_function_resume(ctx, s);
    JSValue::UNDEFINED
}

pub unsafe fn js_async_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let s = async_func_init(ctx, func_obj, this_obj, argc, argv);
    if s.is_null() {
        return JSValue::EXCEPTION;
    }

    if let Ok((promise, resolving_funcs)) = ctx.create_promise(JSValue::UNDEFINED) {
        (*s).resolving_funcs = resolving_funcs;
        js_async_function_resume(ctx, s);
        async_func_free(ctx.rt_mut(), s);
        promise
    } else {
        async_func_free(ctx.rt_mut(), s);
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_async_function_resume(ctx: *mut JSContext, s: *mut JSAsyncFunctionState) {
    let func_ret = async_func_resume(ctx.as_mut().unwrap(), s);

    if (*s).is_completed {
        let ret2: JSValue;

        if func_ret.is_exception() {
            let error = (*ctx).get_exception();
            ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[1], // reject
                JSValue::UNDEFINED,
                1,
                [error].as_mut_ptr(),
            );
            JS_FreeValue(ctx, error);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        } else {
            // normal return
            ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[0], // resolve
                JSValue::UNDEFINED,
                1,
                [func_ret].as_mut_ptr(),
            );
            JS_FreeValue(ctx, func_ret);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        }
    } else {
        let mut i: i32;
        let mut res: i32;

        let cur_sp = (*s)
            .frame
            .stack_base_ptr()
            .as_ptr()
            .add((*s).frame.cur_pc_offset);

        let mut value = std::ptr::replace(cur_sp.offset(-1), JSValue::UNDEFINED);

        // await
        JS_FreeValue(ctx, func_ret); // not used

        let promise = js_promise_resolve(ctx, (*ctx).promise_ctor, 1, &mut value, 0);
        JS_FreeValue(ctx, value);
        if promise.is_exception() {
            let mut error = (*ctx).get_exception();
            let ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[1],
                JSValue::UNDEFINED,
                1,
                &mut error,
            );
            JS_FreeValue(ctx, error);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        }

        // let mut resolving_funcs: [JSValue; 2] = [JSValue::UNDEFINED; 2];
        // if js_async_function_resolve_create(ctx, s, resolving_funcs.as_mut_ptr()) {
        //     JS_FreeValue(ctx, promise);

        //     let error = (*ctx).get_exception();
        //     let ret2 = JS_Call(
        //         ctx.as_mut().unwrap(),
        //         (*s).resolving_funcs[1],
        //         JSValue::UNDEFINED,
        //         1,
        //         &mut error,
        //     );
        //     JS_FreeValue(ctx, error);
        //     JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        // }

        let resolving_funcs =
            if let Ok(rr) = js_async_function_resolve_create(ctx.as_mut().unwrap(), s) {
                rr
            } else {
                JS_FreeValue(ctx, promise);
                let mut error = (*ctx).get_exception();
                let ret2 = JS_Call(
                    ctx.as_mut().unwrap(),
                    (*s).resolving_funcs[1],
                    JSValue::UNDEFINED,
                    1,
                    &mut error,
                );
                JS_FreeValue(ctx, error);
                JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
                [JSValue::UNDEFINED; 2]
            };

        let res = perform_promise_then(
            ctx.as_mut().unwrap(),
            promise,
            &resolving_funcs,
            &[JSValue::UNDEFINED; 2], // no need to create 'thrownawayCapability' as in the spec
        );
        JS_FreeValue(ctx, promise);
        JS_FreeValue(ctx, resolving_funcs[0]);
        JS_FreeValue(ctx, resolving_funcs[1]);
        if res.is_err() {
            let mut error = (*ctx).get_exception();
            let ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[1],
                JSValue::UNDEFINED,
                1,
                &mut error,
            );
            JS_FreeValue(ctx, error);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        }
    }
}

pub const js_async_function_proto_funcs: [JSCFunctionListEntry; 1] = [JS_PROP_STRING_DEF!(
    c"[Symbol.toStringTag]",
    c"AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
