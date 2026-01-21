use crate::{
    JS_CLASS_PROMISE,
    atom::Atom,
    builtins::{error::js_aggregate_error_constructor, js_get_this},
    context::ContextImpl,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunctionWithData},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_SpeciesConstructor},
    utils::unlikely,
    value::JSValue,
};

use super::{
    PROMISE_MAGIC_all, PROMISE_MAGIC_allSettled, PROMISE_MAGIC_any, Promise,
    js_promise_all_resolve_element, remainingElementsCount_add,
};

//
// Instance methods
//

fn js_promise_then(cc: CallContext) -> JSResult {
    let mut this = *cc.this();
    if let Some(p) = this.as_object_Promise_mut() {
        p.then(&mut cc.context(), *cc.arg(0), *cc.arg(1))
            .map(|p| p.into())
    } else {
        cc.ctx().throw_type_error_invalid_class(JS_CLASS_PROMISE);
        Err(-1)
    }
}

fn js_promise_catch(cc: CallContext) -> JSResult {
    cc.this().invoke(
        cc.ctx(),
        Atom::CONST_then,
        &[JSValue::UNDEFINED, *cc.arg(0)],
    )
}

fn js_promise_then_finally_func(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let ctor = cc.data(0);
    let on_finally = cc.data(1);

    let res = on_finally.call(ctx, JSValue::UNDEFINED, &[])?;

    let promise = js_promise_resolve(CallContext::new_simple(
        cc.ctx_ptr(),
        ctor,
        std::slice::from_ref(&res),
    ))?;

    // create inner resolving function for finally()
    let then_func: JSValue = NativeFunctionWithData::create(
        ctx,
        |cc: CallContext| -> JSResult {
            let val = cc.data(0);
            if cc.magic() == 0 {
                val.err() // resolve
            } else {
                cc.ctx().throw(val); // reject
                Err(-1)
            }
        },
        0, // resolve function has length=0
        cc.magic(),
        [*cc.arg(0)].into(), // data: argument#0
    )?
    .into();

    promise.invoke(ctx, Atom::CONST_then, std::slice::from_ref(&then_func))
}

pub fn js_promise_finally(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let on_finally = *cc.arg(0);

    let then_funcs: [JSValue; 2] = if !on_finally.is_function() {
        [on_finally, on_finally]
    } else {
        let ctor = JS_SpeciesConstructor(ctx, *cc.this(), JSValue::UNDEFINED)?;
        [
            NativeFunctionWithData::create(
                ctx,
                js_promise_then_finally_func,
                1,
                0,
                // see callback for data usage
                [ctor, on_finally].into(),
            )?
            .into(),
            NativeFunctionWithData::create(
                ctx,
                js_promise_then_finally_func,
                1,
                1,
                // see callback for data usage
                [ctor, on_finally].into(),
            )?
            .into(),
        ]
    };

    cc.this().invoke(ctx, Atom::CONST_then, &then_funcs)
}

/// returned value is Promise object or Exception.
pub fn js_promise_resolve(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let is_reject = cc.magic() != 0;
    let arg0 = *cc.arg(0);

    if !is_reject && arg0.retrive_object_payload0(JS_CLASS_PROMISE).is_some() {
        // resolve and argument #0 is Promise
        let ctor = arg0.get_property(ctx, Atom::CONST_constructor)?;
        if ctor.is_same(*cc.this()) {
            return Ok(arg0);
        }
    }

    let (result_promise, resolving_funcs) = Promise::create_from_ctor(ctx, *cc.this())?;
    resolving_funcs[is_reject as usize].call(
        ctx,
        JSValue::UNDEFINED,
        std::slice::from_ref(&arg0),
    )?;
    Ok(result_promise.into())
}

pub fn js_promise_all(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let is_promise_any = cc.magic() == PROMISE_MAGIC_any;
    let (result_promise, resolving_funcs) = Promise::create_from_ctor(ctx, *cc.this())?;

    let fail_reject = |cx: &ContextImpl| -> JSResult {
        let error = cx.rt_ptr().take_exception();
        resolving_funcs[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&error))?;
        Ok(JSValue::Object(result_promise))
    };

    let promise_resolve = if let Ok(x) = cc.this().get_property(ctx, Atom::CONST_resolve) {
        if unlikely(x.ensure_function(ctx).is_err()) {
            return fail_reject(ctx);
        } else {
            x
        }
    } else {
        return fail_reject(ctx);
    };

    let Ok(mut it) = cc.arg(0).iter(ctx) else {
        return fail_reject(ctx);
    };
    let mut values: JSValue = if let Ok(a) = ctx.create_array() {
        a.into()
    } else {
        return fail_reject(ctx);
    };
    let mut resolve_element_env: JSValue = if let Ok(a) = ctx.create_array() {
        a.into()
    } else {
        return fail_reject(ctx);
    };

    if resolve_element_env
        .define_property_value_u32(ctx, 0, JSValue::Int(1), 0x7)
        .is_err()
    {
        return fail_reject(ctx);
    }

    let mut index = 0;

    while let Some(a) = it.next() {
        if let Ok(item) = a {
            let Ok(next_promise) =
                promise_resolve.call(ctx, *cc.this(), std::slice::from_ref(&item))
            else {
                return fail_reject(ctx);
            };

            let mut resolve_element: JSValue = if let Ok(v) = NativeFunctionWithData::create(
                ctx,
                js_promise_all_resolve_element,
                1,
                cc.magic(),
                [
                    JSValue::Bool(false),
                    JSValue::Int(index),
                    values,
                    resolving_funcs[is_promise_any as usize],
                    resolve_element_env,
                ]
                .into(),
            ) {
                v.into()
            } else {
                return fail_reject(ctx);
            };

            let reject_element: JSValue = match cc.magic() {
                PROMISE_MAGIC_allSettled => {
                    if let Ok(r) = NativeFunctionWithData::create(
                        ctx,
                        js_promise_all_resolve_element,
                        1,
                        cc.magic() | 4,
                        [
                            JSValue::Bool(false),
                            JSValue::Int(index),
                            values,
                            resolving_funcs[is_promise_any as usize],
                            resolve_element_env,
                        ]
                        .into(),
                    ) {
                        r.into()
                    } else {
                        return fail_reject(ctx);
                    }
                }
                PROMISE_MAGIC_any => {
                    if values
                        .define_property_value_u32(ctx, index as u32, JSValue::UNDEFINED, 0x7)
                        .is_err()
                    {
                        return fail_reject(ctx);
                    }
                    std::mem::replace(&mut resolve_element, resolving_funcs[0]) // take out old value
                }
                _ => resolving_funcs[1],
            };

            if remainingElementsCount_add(ctx, resolve_element_env, 1).is_err() {
                return fail_reject(ctx);
            }

            if next_promise
                .invoke(ctx, Atom::CONST_then, &[resolve_element, reject_element])
                .is_err()
            {
                return fail_reject(ctx);
            }

            index += 1;
        } else {
            // has error when accessing 'value' property.
            // conformance: should close the iterator if error on 'done' access, but not on 'value' access.
            // so we set field .done to true by hand to avoid closing the iterator in ObjectIter::drop();
            it.done = true;
            return fail_reject(ctx);
        }
    }

    match remainingElementsCount_add(ctx, resolve_element_env, -1) {
        Ok(1) => {
            if cc.magic() == PROMISE_MAGIC_any {
                let error = js_aggregate_error_constructor(ctx, values);
                if error.is_exception() {
                    return fail_reject(ctx);
                }
                values = error;
            }

            let ret = resolving_funcs[is_promise_any as usize].call(
                ctx,
                JSValue::UNDEFINED,
                std::slice::from_ref(&values),
            );
            if ret.is_err() {
                return fail_reject(ctx);
            }
        }
        Ok(0) => {}
        Err(_) => {
            return fail_reject(ctx);
        }
        Ok(_) => {
            unreachable!()
        }
    }

    Ok(result_promise.into())
}

pub fn js_promise_race(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let (result_promise, resolving_funcs) = Promise::create_from_ctor(ctx, *cc.this())?;

    let fail_reject = |cx: &ContextImpl| -> JSResult {
        let error = cx.rt_ptr().take_exception();
        resolving_funcs[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&error))?;
        Ok(result_promise.into())
    };

    let promise_resolve = cc
        .this()
        .get_property(ctx, Atom::CONST_resolve)
        .unwrap_or(JSValue::EXCEPTION);

    if unlikely(promise_resolve.is_exception() || promise_resolve.ensure_function(ctx).is_err()) {
        fail_reject(ctx)
    } else {
        let Ok(mut it) = cc.arg(0).iter(ctx) else {
            return fail_reject(ctx);
        };
        let this = *cc.this();

        while let Some(item) = it.next() {
            if let Ok(val) = item {
                if promise_resolve
                    .call(ctx, this, std::slice::from_ref(&val))
                    .and_then(|pr| pr.invoke(ctx, Atom::CONST_then, &resolving_funcs))
                    .is_err()
                {
                    return fail_reject(ctx);
                }
            } else {
                // has error when accessing 'value' property.
                // conformance: should close the iterator if error on 'done' access, but not on 'value' access.
                // so we set field .done to true by hand to avoid closing the iterator in ObjectIter::drop();
                it.done = true;
                return fail_reject(ctx);
            }
        }

        Ok(result_promise.into())
    }
}

pub fn js_promise_withResolvers(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let (result_promise, resolving_funcs) = Promise::create_from_ctor(ctx, *cc.this())?;
    let mut obj = ctx.create_base_object()?;

    obj.define_property_value(
        ctx,
        Atom::CONST_promise,
        result_promise.into(),
        JS_PROP_C_W_E,
    )?;
    obj.define_property_value(ctx, Atom::CONST_resolve, resolving_funcs[0], JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, Atom::CONST_reject, resolving_funcs[1], JS_PROP_C_W_E)?;

    Ok(obj.into())
}

pub const js_promise_funcs: &[FieldDef] = &[
    FieldDef::CALL("resolve", 1, js_promise_resolve, 0),
    FieldDef::CALL("reject", 1, js_promise_resolve, 1),
    FieldDef::CALL("all", 1, js_promise_all, PROMISE_MAGIC_all as _),
    FieldDef::CALL(
        "allSettled",
        1,
        js_promise_all,
        PROMISE_MAGIC_allSettled as _,
    ),
    FieldDef::CALL("any", 1, js_promise_all, PROMISE_MAGIC_any as _),
    FieldDef::CALL("race", 1, js_promise_race, 0),
    FieldDef::CALL("withResolvers", 0, js_promise_withResolvers, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

//
// Prototype methods
//

// pub fn js_promise_finally_value_thunk(ctx: ContextPtr, args: CallContext) -> JSResult {
//     args.data_raw(0).dup().check_exception()
// }
// pub fn js_promise_finally_thrower(ctx: ContextPtr, args: CallContext) -> JSResult {
//     ctx.throw(args.data_raw(0).dup());
//     Err(-1)
// }

pub const js_promise_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("then", 2, js_promise_then, 0),
    FieldDef::CALL("catch", 1, js_promise_catch, 0),
    FieldDef::CALL("finally", 1, js_promise_finally, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "Promise", JS_PROP_CONFIGURABLE),
];
