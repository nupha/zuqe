use std::ptr::NonNull;

use crate::{
    atom::consts::*,
    context::JSContext,
    function::JS_Call,
    gc::{JS_MarkFunc, JS_MarkValue},
    runtime::JSRuntime,
    value::{object::JS_GetOpaque, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT},
    JS_CLASS_PROMISE, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
};

use super::fulfill_or_reject_promise;

pub struct JSPromiseFunctionData {
    pub promise: JSValue,
    pub presolved: NonNull<JSPromiseFunctionDataResolved>,
}

pub struct JSPromiseFunctionDataResolved {
    pub ref_count: i32,
    pub already_resolved: bool,
}

pub unsafe fn js_promise_resolve_function_free_resolved(sr: *mut JSPromiseFunctionDataResolved) {
    debug_assert!((*sr).ref_count > 0);
    (*sr).ref_count -= 1;
    if (*sr).ref_count == 0 {
        drop(Box::from_raw(sr));
    }
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

#[inline(always)]
pub unsafe fn reject_promise(ctx: *mut JSContext, promise: JSValueConst, value: JSValueConst) {
    fulfill_or_reject_promise(ctx, promise, value, true);
}

pub unsafe fn js_promise_resolve(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let is_reject = magic != 0;

    if !is_reject && !JS_GetOpaque(*argv.offset(0), JS_CLASS_PROMISE).is_null() {
        let ctor = (*argv.offset(0)).get_property(ctx.as_mut().unwrap(), JS_ATOM_constructor);
        if ctor.is_exception() {
            return ctor;
        }

        let is_same = ctor.is_same(this_val);
        JS_FreeValue(ctx, ctor);
        if is_same {
            return (*argv.offset(0)).dup();
        }
    }

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(this_val) {
        (pr, rr)
    } else {
        return JSValue::EXCEPTION;
    };

    let ret = JS_Call(
        ctx.as_mut().unwrap(),
        resolving_funcs[is_reject as usize],
        JSValue::UNDEFINED,
        1,
        argv,
    );

    JS_FreeValue(ctx, resolving_funcs[0]);
    JS_FreeValue(ctx, resolving_funcs[1]);

    if ret.is_exception() {
        JS_FreeValue(ctx, result_promise);
        JSValue::EXCEPTION
    } else {
        JS_FreeValue(ctx, ret);
        result_promise
    }
}

pub unsafe fn js_promise_resolve_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_val: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let p = func_obj.object();
    let s = if let Some(s) = p.u.promise_function_data.as_mut() {
        if s.presolved.as_ref().already_resolved {
            return JSValue::UNDEFINED;
        }
        s
    } else {
        return JSValue::UNDEFINED;
    };

    s.presolved.as_mut().already_resolved = true;
    let is_reject = (p.class_id() - JS_CLASS_PROMISE_RESOLVE_FUNCTION) != 0;
    let resolution = if argc > 0 {
        *argv.offset(0)
    } else {
        JSValue::UNDEFINED
    };

    #[cfg(DUMP_PROMISE)]
    {
        printf(
            c"js_promise_resolving_function_call: is_reject=%d resolution=",
            is_reject as i32,
        );
        JS_DumpValue(ctx, resolution);
        printf(c"\n");
    }

    if is_reject || !resolution.is_object() {
        fulfill_or_reject_promise(ctx, s.promise, resolution, is_reject);
        return JSValue::UNDEFINED;
    } else if resolution.is_same(s.promise) {
        ctx.JS_ThrowTypeError(c"promise self resolution");
        let error = ctx.get_exception();
        reject_promise(ctx, s.promise, error);
        JS_FreeValue(ctx, error);
        return JSValue::UNDEFINED;
    }

    let then = resolution.get_property(ctx, JS_ATOM_then);
    if then.is_exception() {
        let error = ctx.get_exception();
        reject_promise(ctx, s.promise, error);
        JS_FreeValue(ctx, error);
        JSValue::UNDEFINED
    } else if !then.is_function(ctx) {
        JS_FreeValue(ctx, then);
        fulfill_or_reject_promise(ctx, s.promise, resolution, is_reject);
        JSValue::UNDEFINED
    } else {
        // Note: arg values are duplicated in advance.
        ctx.enqueue_job(
            js_promise_resolve_thenable_job,
            &[s.promise, resolution, then],
        )
        .unwrap();

        JS_FreeValue(ctx, then);
        JSValue::UNDEFINED
    }
}

pub unsafe fn js_promise_resolve_function_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let s = val.object().u.promise_function_data;
    if !s.is_null() {
        js_promise_resolve_function_free_resolved((*s).presolved.as_ptr());
        JS_FreeValueRT(rt, (*s).promise);
        drop(Box::from_raw(s));
    }
}

pub unsafe fn js_promise_resolve_function_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    let s = val.object().u.promise_function_data;
    if !s.is_null() {
        JS_MarkValue(rt, (*s).promise, mark_func);
    }
}
