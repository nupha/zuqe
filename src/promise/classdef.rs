use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_PROP_STRING_DEF,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_Call, JS_Invoke, JSCFunctionListEntry},
    iterator::{JS_GetIterator, JS_IteratorClose, JS_IteratorNext},
    js_core::{error::js_aggregate_error_constructor, js_get_this},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_SpeciesConstructor},
    utils::UncheckedInto,
    value::JSValue,
};

use super::{
    PROMISE_MAGIC_all, PROMISE_MAGIC_allSettled, PROMISE_MAGIC_any, PromiseValue,
    js_promise_all_resolve_element, remainingElementsCount_add,
};

//
// Instance methods
//

pub fn js_promise_then(ctx: &JSContext, cc: CallContext) -> JSResult {
    unsafe { UncheckedInto::<PromiseValue>::unchecked_into(cc.this().clone()) }
        .then(ctx, cc.arg(0).clone(), cc.arg(1).clone())
        .map(|p| p.into())
}

pub fn js_promise_catch(ctx: &JSContext, cc: CallContext) -> JSResult {
    JS_Invoke(
        ctx,
        cc.this(),
        JS_ATOM_then,
        &[JSValue::UNDEFINED, JSValue::UNDEFINED],
    )
    .check_exception()
}

pub fn js_promise_then_finally_func(ctx: &JSContext, cc: CallContext) -> JSResult {
    let ctor = cc.data_raw(0);
    let on_finally = cc.data_raw(1);

    let res = JS_Call(ctx, &on_finally, &JSValue::UNDEFINED, &[]).check_exception()?;

    let promise = {
        let tmp = [res];
        let cc = CallContext::new(ctx, ctor, &tmp, 0, None);
        js_promise_resolve(ctx, cc)?
    };

    let mut then_func = ctx.create_native_callable_object(
        |cx: &JSContext, cc: CallContext| -> JSResult {
            let val = cc.data_raw(0);
            if cc.magic() == 0 {
                val.check_exception() // resolve
            } else {
                cx.throw(val); // reject
                Err(-1)
            }
        },
        1,
        cc.magic(),
        std::slice::from_ref(cc.arg(0)), // data: argument#0
    )?;

    // call promise.then(resolve)
    JS_Invoke(
        ctx,
        &promise,
        JS_ATOM_then,
        std::slice::from_ref(&then_func),
    )
    .check_exception()
}

pub fn js_promise_finally(ctx: &JSContext, cc: CallContext) -> JSResult {
    let on_finally = cc.arg(0);

    let ctor = JS_SpeciesConstructor(ctx, cc.this(), JSValue::UNDEFINED)?;

    let mut then_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    if !on_finally.is_callable() {
        then_funcs = [on_finally.clone(), on_finally.clone()];
    } else {
        let func_data = [ctor, on_finally.clone()];
        for i in 0..2 {
            let f = ctx.create_native_callable_object(
                js_promise_then_finally_func,
                1,
                i as _,
                &func_data,
            )?;
            then_funcs[i] = f;
        }
    }

    JS_Invoke(ctx, cc.this(), JS_ATOM_then, &then_funcs).check_exception()
}

/// returned value is Promise object or Exception.
pub fn js_promise_resolve(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let is_reject = cc.magic() != 0;
    let arg0 = cc.arg(0).clone();

    if !is_reject && PromiseValue::try_from(arg0.clone()).is_ok() {
        // resolve and argument #0 is a Promise
        let ctor = arg0
            .get_property(ctx, JS_ATOM_constructor)
            .check_exception()?;

        if ctor.is_same(cc.this()) {
            return Ok(arg0);
        }
    }

    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;

    JS_Call(
        ctx,
        &resolving_funcs[is_reject as usize],
        &JSValue::UNDEFINED,
        cc.arguments(),
    )
    .check_exception()?;

    Ok(result_promise.into())
}

pub fn js_promise_all(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let is_promise_any = cc.magic() == PROMISE_MAGIC_any;
    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;

    let fail_reject = |ret: JSValue| -> JSResult {
        let error = ctx.take_exception();
        JS_Call(
            ctx,
            &resolving_funcs[1],
            &JSValue::UNDEFINED,
            std::slice::from_ref(&error),
        )
        .check_exception()?;
        Ok(ret)
    };

    let promise_resolve = cc.this().get_property(ctx, JS_ATOM_resolve);

    if promise_resolve.is_exception() || promise_resolve.ensure_callable(ctx).is_err() {
        return fail_reject(promise_resolve);
    }

    let iter = JS_GetIterator(ctx, cc.arg(0), false);
    if iter.is_exception() {
        return fail_reject(promise_resolve);
    }

    let next_method = iter.get_property(ctx, JS_ATOM_next);
    if next_method.is_exception() {
        return fail_reject(promise_resolve);
    };

    let mut values = if let Ok(a) = ctx.create_array() {
        a
    } else {
        return fail_reject(promise_resolve);
    };

    let resolve_element_env = if let Ok(a) = ctx.create_array() {
        a
    } else {
        return fail_reject(promise_resolve);
    };

    if resolve_element_env
        .define_property_value_u32(ctx, 0, JSValue::from_i32(1), 0x7)
        .is_err()
    {
        return fail_reject(promise_resolve);
    }

    let mut index = 0;

    loop {
        let mut done = false;
        let item = JS_IteratorNext(ctx, &iter, &next_method, &[], &mut done);
        if item.is_exception() {
            let _ = JS_IteratorClose(ctx, &iter, true);
            return fail_reject(promise_resolve);
        }

        if done {
            break;
        }

        let next_promise = JS_Call(
            ctx,
            &promise_resolve,
            cc.this(),
            std::slice::from_ref(&item),
        );
        if next_promise.is_exception() {
            let _ = JS_IteratorClose(ctx, &iter, true);
            return fail_reject(promise_resolve);
        }

        let resolve_element_data = [
            JSValue::Bool(false),
            JSValue::Int(index),
            values.clone(),
            resolving_funcs[is_promise_any as usize].clone(),
            resolve_element_env.clone(),
        ];

        let mut resolve_element = ctx
            .create_native_callable_object(
                js_promise_all_resolve_element,
                1,
                cc.magic(),
                &resolve_element_data,
            )
            .unwrap_or(JSValue::EXCEPTION);

        if resolve_element.is_exception() {
            let _ = JS_IteratorClose(ctx, &iter, true);
            return fail_reject(promise_resolve);
        }

        let reject_element = match cc.magic() {
            PROMISE_MAGIC_allSettled => {
                let r = ctx.create_native_callable_object(
                    js_promise_all_resolve_element,
                    1,
                    cc.magic() | 4,
                    &resolve_element_data,
                );
                if let Ok(r) = r {
                    r
                } else {
                    let _ = JS_IteratorClose(ctx, &iter, true);
                    return fail_reject(promise_resolve);
                }

                // let r = JS_NewCFunctionData(
                //     ctx,
                //     js_promise_all_resolve_element,
                //     1,
                //     cc.magic() | 4,
                //     5,
                //     resolve_element_data.as_ptr(),
                // );
                // if r.is_exception() {
                //     JS_IteratorClose(ctx, &iter, true);
                //     return fail_reject(promise_resolve);
                // }
                // r
            }
            PROMISE_MAGIC_any => {
                if values
                    .define_property_value_u32(ctx, index as u32, JSValue::UNDEFINED, 0x7)
                    .is_err()
                {
                    let _ = JS_IteratorClose(ctx, &iter, true);
                    return fail_reject(promise_resolve);
                }
                std::mem::replace(&mut resolve_element, resolving_funcs[0].clone()) // take out old value
            }
            _ => resolving_funcs[1].clone(),
        };

        if remainingElementsCount_add(ctx, resolve_element_env.clone(), 1).is_err() {
            let _ = JS_IteratorClose(ctx, &iter, true);
            return fail_reject(promise_resolve);
        }

        let ret = JS_Invoke(
            ctx,
            &next_promise,
            JS_ATOM_then,
            &[resolve_element, reject_element],
        );
        if ret.is_exception() {
            let _ = JS_IteratorClose(ctx, &iter, true);
            return fail_reject(promise_resolve);
        }

        index += 1;
    } // loop

    match remainingElementsCount_add(ctx, resolve_element_env, -1) {
        Ok(1) => {
            if cc.magic() == PROMISE_MAGIC_any {
                let error = js_aggregate_error_constructor(ctx, values);
                if error.is_exception() {
                    return fail_reject(promise_resolve);
                }
                values = error;
            }

            let ret = JS_Call(
                ctx,
                &resolving_funcs[is_promise_any as usize],
                &JSValue::UNDEFINED,
                std::slice::from_ref(&values),
            );
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

    Ok(result_promise.into())
}

pub fn js_promise_race(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;

    let fail_reject = |retval: JSValue| -> JSResult {
        let error = ctx.take_exception();
        JS_Call(
            ctx,
            &resolving_funcs[1],
            &JSValue::UNDEFINED,
            std::slice::from_ref(&error),
        )
        .check_exception()?;

        Ok(retval)
    };

    let promise_resolve = cc.this().get_property(ctx, JS_ATOM_resolve);

    if promise_resolve.is_exception() || promise_resolve.ensure_callable(ctx).is_err() {
        return fail_reject(promise_resolve);
    }

    let iter = JS_GetIterator(ctx, cc.arg(0), false);
    if iter.is_exception() {
        return fail_reject(promise_resolve);
    }

    let next_method = iter.get_property(ctx, JS_ATOM_next);
    if next_method.is_exception() {
        return fail_reject(promise_resolve);
    }

    loop {
        let mut done = false;
        let item = JS_IteratorNext(ctx, &iter, &next_method, &[], &mut done);
        if item.is_exception() {
            return fail_reject(promise_resolve);
        }
        if done {
            break;
        }

        let next_promise = JS_Call(
            ctx,
            &promise_resolve,
            cc.this(),
            std::slice::from_ref(&item),
        );
        if next_promise.is_exception() {
            return fail_reject(promise_resolve);
        }

        let ret = JS_Invoke(ctx, &next_promise, JS_ATOM_then, &resolving_funcs);
        if ret.is_exception() {
            return fail_reject(promise_resolve);
        }
    }

    Ok(result_promise.into())
}

pub fn js_promise_withResolvers(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;
    let obj = ctx.create_base_object()?;

    obj.define_property_value(
        ctx,
        JS_ATOM_promise,
        result_promise.as_value(),
        JS_PROP_C_W_E,
    )?;
    obj.define_property_value(
        ctx,
        JS_ATOM_resolve,
        resolving_funcs[0].clone(),
        JS_PROP_C_W_E,
    )?;
    obj.define_property_value(
        ctx,
        JS_ATOM_reject,
        resolving_funcs[1].clone(),
        JS_PROP_C_W_E,
    )?;

    Ok(obj)
}

pub const js_promise_funcs: [JSCFunctionListEntry; 8] = [
    JS_CFUNC_NATIVE_DEF!("resolve", 1, js_promise_resolve, 0),
    JS_CFUNC_NATIVE_DEF!("reject", 1, js_promise_resolve, 1),
    JS_CFUNC_NATIVE_DEF!("all", 1, js_promise_all, PROMISE_MAGIC_all as _),
    JS_CFUNC_NATIVE_DEF!(
        "allSettled",
        1,
        js_promise_all,
        PROMISE_MAGIC_allSettled as _
    ),
    JS_CFUNC_NATIVE_DEF!("any", 1, js_promise_all, PROMISE_MAGIC_any as _),
    JS_CFUNC_NATIVE_DEF!("race", 1, js_promise_race, 0),
    JS_CFUNC_NATIVE_DEF!("withResolvers", 0, js_promise_withResolvers, 0),
    JS_CGETSET_DEF!("[Symbol.species]", js_get_this, NONE),
];

//
// Prototype methods
//

// pub fn js_promise_finally_value_thunk(ctx: &JSContext, args: CallContext) -> JSResult {
//     args.data_raw(0).dup().check_exception()
// }
// pub fn js_promise_finally_thrower(ctx: &JSContext, args: CallContext) -> JSResult {
//     ctx.throw(args.data_raw(0).dup());
//     Err(-1)
// }

pub const js_promise_proto_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_NATIVE_DEF!("then", 2, js_promise_then, 0),
    JS_CFUNC_NATIVE_DEF!("catch", 1, js_promise_catch, 0),
    JS_CFUNC_NATIVE_DEF!("finally", 1, js_promise_finally, 0),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Promise", JS_PROP_CONFIGURABLE),
];
