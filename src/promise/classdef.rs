use std::ops::DerefMut;

use crate::{
    array::JS_NewArray,
    atom::consts::*,
    context::JSContext,
    function::{JSCFunctionListEntry, JS_Call, JS_Invoke},
    iterator::{JS_GetIterator, JS_IteratorClose, JS_IteratorNext},
    js_core::{error::js_aggregate_error_constructor, js_get_this},
    obsoletes::JS_NewCFunctionData,
    value::{
        object::{
            JS_GetOpaque, JS_GetOpaque2, JS_NewObject, JS_SpeciesConstructor, JS_PROP_CONFIGURABLE,
            JS_PROP_C_W_E,
        },
        JSValue, JSValueRc, JS_FreeValue,
    },
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CGETSET_DEF, JS_CLASS_PROMISE, JS_PROP_STRING_DEF,
};

use super::{
    js_promise_all_resolve_element, js_promise_then_finally_func, perform_promise_then,
    remainingElementsCount_add, JSPromiseData, PROMISE_MAGIC_all, PROMISE_MAGIC_allSettled,
    PROMISE_MAGIC_any,
};

//
// Promise class
//

/// returned value is Promise object or Exception.
pub unsafe fn js_promise_resolve(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
    magic: i32,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let is_reject = magic != 0;

    if !is_reject && !JS_GetOpaque(*argv.offset(0), JS_CLASS_PROMISE).is_null() {
        let ctor = (*argv.offset(0)).get_property(ctx.as_mut().unwrap(), JS_ATOM_constructor);
        if ctor.is_exception() {
            return JSValue::EXCEPTION;
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

pub unsafe fn js_promise_all(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
    magic: i32,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }
    let is_promise_any = magic == PROMISE_MAGIC_any as i32;

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(this_val) {
        (
            pr.into_rc((*ctx).rt()),
            [rr[0].into_rc((*ctx).rt()), rr[1].into_rc((*ctx).rt())],
        )
    } else {
        return JSValue::EXCEPTION;
    };

    let fail_reject = |promise: JSValueRc| -> JSValue {
        let mut error = (*ctx).get_exception().into_rc((*ctx).rt());
        let ret = JS_Call(
            ctx.as_mut().unwrap(),
            *resolving_funcs[1],
            JSValue::UNDEFINED,
            1,
            error.deref_mut(),
        )
        .into_rc((*ctx).rt());
        return if ret.is_exception() {
            JSValue::EXCEPTION
        } else {
            promise.into()
        };
    };

    let promise_resolve = this_val
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_resolve)
        .into_rc((*ctx).rt());

    if promise_resolve.is_exception()
        || promise_resolve
            .ensure_callable(ctx.as_mut().unwrap())
            .is_err()
    {
        return fail_reject(promise_resolve);
    }

    let iter = JS_GetIterator(ctx, *argv.offset(0), false).into_rc((*ctx).rt());
    if iter.is_exception() {
        return fail_reject(promise_resolve);
    }

    let next_method = iter
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_next)
        .into_rc((*ctx).rt());
    if next_method.is_exception() {
        return fail_reject(promise_resolve);
    };

    let mut values = JS_NewArray(ctx).into_rc((*ctx).rt());
    if values.is_exception() {
        return fail_reject(promise_resolve);
    }

    let resolve_element_env = JS_NewArray(ctx).into_rc((*ctx).rt());
    if resolve_element_env.is_exception() {
        return fail_reject(promise_resolve);
    } else if resolve_element_env
        .define_property_value_u32(ctx.as_mut().unwrap(), 0, JSValue::from_i32(1), 0x7)
        .is_err()
    {
        return fail_reject(promise_resolve);
    }

    let mut index = 0;

    loop {
        let mut done = false;
        let item = JS_IteratorNext(ctx, *iter, *next_method, 0, std::ptr::null_mut(), &mut done)
            .into_rc((*ctx).rt());

        if item.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        if done {
            break;
        }

        let next_promise = JS_Call(
            ctx.as_mut().unwrap(),
            *promise_resolve,
            this_val,
            1,
            [*item].as_mut_ptr(),
        )
        .into_rc((*ctx).rt());
        if next_promise.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        let resolve_element_data = [
            JSValue::FALSE,
            JSValue::from_i32(index),
            *values,
            *resolving_funcs[is_promise_any as usize],
            *resolve_element_env,
        ];

        let mut resolve_element = (*ctx)
            .create_native_callable_object(
                js_promise_all_resolve_element,
                1,
                magic,
                &resolve_element_data,
            )
            .unwrap_or(JSValue::EXCEPTION)
            .into_rc((*ctx).rt());

        if resolve_element.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        let reject_element: JSValueRc = match magic {
            PROMISE_MAGIC_allSettled => {
                let r = JS_NewCFunctionData(
                    ctx,
                    js_promise_all_resolve_element,
                    1,
                    magic | 4,
                    5,
                    resolve_element_data.as_ptr(),
                )
                .into_rc((*ctx).rt());
                if r.is_exception() {
                    JS_IteratorClose(ctx, *iter, true);
                    return fail_reject(promise_resolve);
                }
                r
            }
            PROMISE_MAGIC_any => {
                if values
                    .define_property_value_u32(
                        ctx.as_mut().unwrap(),
                        index as u32,
                        JSValue::UNDEFINED,
                        0x7,
                    )
                    .is_err()
                {
                    JS_IteratorClose(ctx, *iter, true);
                    return fail_reject(promise_resolve);
                }
                std::mem::replace(&mut resolve_element, resolving_funcs[0].clone())
            }
            _ => resolving_funcs[1].clone(),
        };

        if remainingElementsCount_add(ctx.as_mut().unwrap(), *resolve_element_env, 1).is_err() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        let ret = JS_Invoke(
            ctx.as_mut().unwrap(),
            *next_promise,
            JS_ATOM_then,
            2,
            [*resolve_element, *reject_element].as_mut_ptr(),
        )
        .into_rc((*ctx).rt());
        if ret.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        index += 1;
    } // loop

    match remainingElementsCount_add(ctx.as_mut().unwrap(), *resolve_element_env, -1) {
        Ok(1) => {
            if magic == PROMISE_MAGIC_any as i32 {
                let error = js_aggregate_error_constructor(ctx.as_mut().unwrap(), *values)
                    .into_rc((*ctx).rt());
                if error.is_exception() {
                    return fail_reject(promise_resolve);
                }
                values = error;
            }

            let ret = JS_Call(
                ctx.as_mut().unwrap(),
                *resolving_funcs[is_promise_any as usize],
                JSValue::UNDEFINED,
                1,
                values.deref_mut(),
            )
            .into_rc((*ctx).rt());

            if ret.is_exception() {
                return fail_reject(promise_resolve);
            }
        }
        Ok(0) => {}
        Err(_) => {
            return fail_reject(promise_resolve);
        }
        Ok(_) => {
            unreachable!()
        }
    }

    result_promise.into()
}

pub unsafe fn js_promise_race(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(this_val) {
        (
            pr.into_rc((*ctx).rt()),
            [rr[0].into_rc((*ctx).rt()), rr[1].into_rc((*ctx).rt())],
        )
    } else {
        return JSValue::EXCEPTION;
    };

    let fail_reject = |promise: JSValueRc| -> JSValue {
        let mut error = (*ctx).get_exception().into_rc((*ctx).rt());
        let ret = JS_Call(
            ctx.as_mut().unwrap(),
            *resolving_funcs[1],
            JSValue::UNDEFINED,
            1,
            error.deref_mut(),
        )
        .into_rc((*ctx).rt());
        return if ret.is_exception() {
            JSValue::EXCEPTION
        } else {
            promise.into()
        };
    };

    let promise_resolve = this_val
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_resolve)
        .into_rc((*ctx).rt());

    if promise_resolve.is_exception()
        || promise_resolve
            .ensure_callable(ctx.as_mut().unwrap())
            .is_err()
    {
        return fail_reject(promise_resolve);
    }

    let iter = JS_GetIterator(ctx, *argv.offset(0), false).into_rc((*ctx).rt());
    if iter.is_exception() {
        return fail_reject(promise_resolve);
    }

    let next_method = iter
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_next)
        .into_rc((*ctx).rt());
    if next_method.is_exception() {
        return fail_reject(promise_resolve);
    }

    loop {
        let mut done = false;
        let mut item =
            JS_IteratorNext(ctx, *iter, *next_method, 0, std::ptr::null_mut(), &mut done)
                .into_rc((*ctx).rt());

        if item.is_exception() {
            return fail_reject(promise_resolve);
        }
        if done {
            break;
        }

        let next_promise = JS_Call(
            ctx.as_mut().unwrap(),
            *promise_resolve,
            this_val,
            1,
            item.deref_mut(),
        )
        .into_rc((*ctx).rt());
        if next_promise.is_exception() {
            return fail_reject(promise_resolve);
        }

        let ret = JS_Invoke(
            ctx.as_mut().unwrap(),
            *next_promise,
            JS_ATOM_then,
            2,
            [*resolving_funcs[0], *resolving_funcs[1]].as_mut_ptr(),
        )
        .into_rc((*ctx).rt());
        if ret.is_exception() {
            return fail_reject(promise_resolve);
        }
    }

    result_promise.into()
}

pub unsafe fn js_promise_withResolvers(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(this_val) {
        (pr, rr)
    } else {
        return JSValue::EXCEPTION;
    };

    let obj = JS_NewObject(ctx);
    if obj.is_exception() {
        JS_FreeValue(ctx, resolving_funcs[0]);
        JS_FreeValue(ctx, resolving_funcs[1]);
        JS_FreeValue(ctx, result_promise);
        return JSValue::EXCEPTION;
    }

    obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_promise,
        result_promise,
        JS_PROP_C_W_E,
    )
    .unwrap();

    obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_resolve,
        resolving_funcs[0],
        JS_PROP_C_W_E,
    )
    .unwrap();

    obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_reject,
        resolving_funcs[1],
        JS_PROP_C_W_E,
    )
    .unwrap();

    obj
}

pub const js_promise_funcs: [JSCFunctionListEntry; 8] = [
    JS_CFUNC_MAGIC_DEF!(c"resolve", 1, js_promise_resolve, 0),
    JS_CFUNC_MAGIC_DEF!(c"reject", 1, js_promise_resolve, 1),
    JS_CFUNC_MAGIC_DEF!(c"all", 1, js_promise_all, PROMISE_MAGIC_all as _),
    JS_CFUNC_MAGIC_DEF!(
        c"allSettled",
        1,
        js_promise_all,
        PROMISE_MAGIC_allSettled as _
    ),
    JS_CFUNC_MAGIC_DEF!(c"any", 1, js_promise_all, PROMISE_MAGIC_any as _),
    JS_CFUNC_DEF!(c"race", 1, js_promise_race),
    JS_CFUNC_DEF!(c"withResolvers", 0, js_promise_withResolvers),
    JS_CGETSET_DEF!(c"[Symbol.species]", js_get_this, NONE),
];

//
// Prototype
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

    let ret = perform_promise_then(
        ctx.as_mut().unwrap(),
        this_val.as_promise_unchecked(),
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

pub unsafe fn js_promise_finally(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let onFinally = *argv.offset(0);

    let ctor = JS_SpeciesConstructor(ctx, this_val, JSValue::UNDEFINED);
    if ctor.is_exception() {
        return ctor;
    }

    let mut then_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    if !onFinally.is_function(ctx.as_ref().unwrap()) {
        then_funcs = [onFinally.dup(), onFinally.dup()];
    } else {
        let func_data = [ctor, onFinally];
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

pub const js_promise_proto_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_DEF!(c"then", 2, js_promise_then),
    JS_CFUNC_DEF!(c"catch", 1, js_promise_catch),
    JS_CFUNC_DEF!(c"finally", 1, js_promise_finally),
    JS_PROP_STRING_DEF!(c"[Symbol.toStringTag]", c"Promise", JS_PROP_CONFIGURABLE),
];
