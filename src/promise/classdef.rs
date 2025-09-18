use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_PROP_STRING_DEF,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    js_core::{error::js_aggregate_error_constructor, js_get_this},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_SpeciesConstructor},
    value::JSValue,
};

use super::{
    PROMISE_MAGIC_all, PROMISE_MAGIC_allSettled, PROMISE_MAGIC_any, Promise,
    js_promise_all_resolve_element, remainingElementsCount_add,
};

//
// Instance methods
//

pub fn js_promise_then(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .as_object_Promise()
        .unwrap()
        .then(ctx, cc.arg(0).clone(), cc.arg(1).clone())
        .map(|p| p.into())
}

pub fn js_promise_catch(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .invoke(ctx, JS_ATOM_then, &[JSValue::UNDEFINED, cc.arg(1).clone()])
}

pub fn js_promise_then_finally_func(ctx: &JSContext, cc: CallContext) -> JSResult {
    let ctor = cc.data(0);
    let on_finally = cc.data(1);

    let res = on_finally.call(ctx, &JSValue::UNDEFINED, &[])?;

    let promise = {
        let tmp = [res];
        let cc = CallContext::new(ctx, ctor, &tmp, 0, None);
        js_promise_resolve(ctx, cc)?
    };

    let then_func: JSValue = ctx
        .create_native_function_with_data(
            |cx: &JSContext, cc: CallContext| -> JSResult {
                let val = cc.data(0);
                if cc.magic() == 0 {
                    val.err() // resolve
                } else {
                    cx.throw(val); // reject
                    Err(-1)
                }
            },
            1,
            cc.magic(),
            std::slice::from_ref(cc.arg(0)), // data: argument#0
        )?
        .into();

    // call promise.then(resolve)
    promise.invoke(ctx, JS_ATOM_then, std::slice::from_ref(&then_func))
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
            then_funcs[i] = ctx
                .create_native_function_with_data(
                    js_promise_then_finally_func,
                    1,
                    i as _,
                    &func_data,
                )?
                .into();
        }
    }

    cc.this().invoke(ctx, JS_ATOM_then, &then_funcs)
}

/// returned value is Promise object or Exception.
pub fn js_promise_resolve(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let is_reject = cc.magic() != 0;
    let arg0 = cc.arg(0).clone();

    if !is_reject && Promise::try_from(arg0.clone()).is_ok() {
        // resolve and argument #0 is a Promise
        let ctor = arg0.get_property(ctx, JS_ATOM_constructor)?;
        if ctor.is_same(cc.this()) {
            return Ok(arg0);
        }
    }

    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;
    resolving_funcs[is_reject as usize].call(ctx, &JSValue::UNDEFINED, cc.arguments())?;
    Ok(result_promise.into())
}

pub fn js_promise_all(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let is_promise_any = cc.magic() == PROMISE_MAGIC_any;
    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;

    let fail_reject = || -> JSResult {
        let error = ctx.take_exception();
        resolving_funcs[1].call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&error))?;
        Ok(JSValue::Object(result_promise.clone().into()))
    };

    let promise_resolve = if let Ok(x) = cc.this().get_property(ctx, JS_ATOM_resolve) {
        if x.ensure_callable(ctx).is_err() {
            return fail_reject();
        } else {
            x
        }
    } else {
        return fail_reject();
    };

    let Ok(it) = cc.arg(0).iter(ctx) else {
        return fail_reject();
    };
    let mut values: JSValue = if let Ok(a) = ctx.create_array() {
        a.into()
    } else {
        return fail_reject();
    };
    let resolve_element_env: JSValue = if let Ok(a) = ctx.create_array() {
        a.into()
    } else {
        return fail_reject();
    };

    if resolve_element_env
        .define_property_value_u32(ctx, 0, JSValue::Int(1), 0x7)
        .is_err()
    {
        return fail_reject();
    }

    let mut index = 0;
    for item in it {
        if let Ok(item) = item {
            let Ok(next_promise) =
                promise_resolve.call(ctx, cc.this(), std::slice::from_ref(&item))
            else {
                return fail_reject();
            };

            let resolve_element_data = [
                JSValue::Bool(false),
                JSValue::Int(index),
                values.clone().into(),
                resolving_funcs[is_promise_any as usize].clone(),
                resolve_element_env.clone(),
            ];
            let mut resolve_element: JSValue = if let Ok(v) = ctx.create_native_function_with_data(
                js_promise_all_resolve_element,
                1,
                cc.magic(),
                &resolve_element_data,
            ) {
                v.into()
            } else {
                return fail_reject();
            };

            let reject_element: JSValue = match cc.magic() {
                PROMISE_MAGIC_allSettled => {
                    if let Ok(r) = ctx.create_native_function_with_data(
                        js_promise_all_resolve_element,
                        1,
                        cc.magic() | 4,
                        &resolve_element_data,
                    ) {
                        r.into()
                    } else {
                        return fail_reject();
                    }
                }
                PROMISE_MAGIC_any => {
                    if values
                        .define_property_value_u32(ctx, index as u32, JSValue::UNDEFINED, 0x7)
                        .is_err()
                    {
                        return fail_reject();
                    }
                    std::mem::replace(&mut resolve_element, resolving_funcs[0].clone()) // take out old value
                }
                _ => resolving_funcs[1].clone(),
            };

            if remainingElementsCount_add(ctx, resolve_element_env.clone(), 1).is_err() {
                return fail_reject();
            }

            if next_promise
                .invoke(ctx, JS_ATOM_then, &[resolve_element, reject_element])
                .is_err()
            {
                return fail_reject();
            }

            index += 1;
        } else {
            return fail_reject();
        }
    }

    match remainingElementsCount_add(ctx, resolve_element_env, -1) {
        Ok(1) => {
            if cc.magic() == PROMISE_MAGIC_any {
                let error = js_aggregate_error_constructor(ctx, values);
                if error.is_exception() {
                    return fail_reject();
                }
                values = error;
            }

            let ret = resolving_funcs[is_promise_any as usize].call(
                ctx,
                &JSValue::UNDEFINED,
                std::slice::from_ref(&values),
            );
            if ret.is_err() {
                return fail_reject();
            }
        }
        Ok(0) => {}
        Err(_) => {
            return fail_reject();
        }
        Ok(_) => {
            unreachable!()
        }
    }

    Ok(result_promise.into())
}

pub fn js_promise_race(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;

    let fail_reject = || -> JSResult {
        let error = ctx.take_exception();
        resolving_funcs[1].call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&error))?;
        Ok(result_promise.clone().into())
    };

    let promise_resolve = cc
        .this()
        .get_property(ctx, JS_ATOM_resolve)
        .unwrap_or(JSValue::EXCEPTION);

    if promise_resolve.is_exception() || promise_resolve.ensure_callable(ctx).is_err() {
        return fail_reject();
    }

    let Ok(it) = cc.arg(0).iter(ctx) else {
        return fail_reject();
    };
    let this = cc.this();

    for item in it {
        if item
            .and_then(|val| {
                promise_resolve
                    .call(ctx, this, std::slice::from_ref(&val))
                    .and_then(|pr| pr.invoke(ctx, JS_ATOM_then, &resolving_funcs))
            })
            .is_err()
        {
            return fail_reject();
        }
    }

    Ok(result_promise.into())
}

pub fn js_promise_withResolvers(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let (result_promise, resolving_funcs) = ctx.create_promise(cc.this())?;
    let obj = ctx.create_base_object()?;

    obj.define_property_value(ctx, JS_ATOM_promise, result_promise.into(), JS_PROP_C_W_E)?;
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

    Ok(obj.into())
}

pub static js_promise_funcs: [FieldDef; 8] = [
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
    JS_CGETSET_NATIVE_DEF!("[Symbol.species]", js_get_this, NONE, 0),
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

pub static js_promise_proto_funcs: [FieldDef; 4] = [
    JS_CFUNC_NATIVE_DEF!("then", 2, js_promise_then, 0),
    JS_CFUNC_NATIVE_DEF!("catch", 1, js_promise_catch, 0),
    JS_CFUNC_NATIVE_DEF!("finally", 1, js_promise_finally, 0),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Promise", JS_PROP_CONFIGURABLE),
];
