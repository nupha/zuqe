// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Callback, JS_CLASS_PROMISE,
    array::Array,
    atom::Atom,
    builtins::{common::SPECIES_FIELD, error::js_aggregate_error_constructor},
    context::ScopedContext,
    error::{ErrorKind, JSResult},
    function::{CallContext, FieldDef, NativeFunction},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_C_W_E,
        JS_PROP_CONFIGURABLE, JS_SpeciesConstructor,
    },
    utils::unlikely,
    value::JSValue,
};

use super::{
    PROMISE_MAGIC_all, PROMISE_MAGIC_allKeyed, PROMISE_MAGIC_allSettled,
    PROMISE_MAGIC_allSettledKeyed, PROMISE_MAGIC_any, Promise, js_promise_all_resolve_element,
    remainingElementsCount_add,
};

fn js_promise_then(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    if let Some(mut p) = this.as_object_Promise() {
        p.then(ctx, cc.arg(0), cc.arg(1)).map(|p| p.into())
    } else {
        Err(ctx.throw_type_error_invalid_class(JS_CLASS_PROMISE))
    }
}

fn js_promise_catch(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    cc.this()
        .invoke(ctx, Atom::CONST_then, &[JSValue::UNDEFINED, cc.arg(0)])
}

fn js_promise_then_finally_func(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let ctor = cc.data(0);
    let on_finally = cc.data(1);

    let res = on_finally.call(ctx, JSValue::UNDEFINED, &[])?;
    let promise = js_promise_resolve(
        ctx,
        CallContext::new_simple(ctor, std::slice::from_ref(&res)),
    )?;

    // create inner resolving function for finally()
    let then_func: JSValue = NativeFunction::create(
        ctx,
        Callback::Func(|cx: &mut ScopedContext<'_>, cc: CallContext| -> JSResult {
            let val = cc.data(0);
            if cc.magic() == 0 {
                val.err() // resolve
            } else {
                Err(cx.throw_value(val)) // reject
            }
        }),
        Atom::CONST_empty_string,
        0, // resolve function has length=0
        cc.magic() as _,
        ctx.function_proto(),
        Some([cc.arg(0)].into()), // data: argument#0
    )?
    .into();

    // manual gc node protection for promise, then_func is not required now:
    // StackFrame will protect promise and then_func (as this_obj and args).
    promise.invoke(ctx, Atom::CONST_then, std::slice::from_ref(&then_func))
}

pub fn js_promise_finally(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let on_finally = cc.arg(0);

    let then_args: [JSValue; 2] = if !on_finally.is_callable() {
        [on_finally, on_finally]
    } else {
        let function_proto = ctx.function_proto();
        let ctor = JS_SpeciesConstructor(ctx, cc.this(), JSValue::UNDEFINED)?;

        [
            NativeFunction::create(
                ctx,
                Callback::Func(js_promise_then_finally_func),
                Atom::CONST_empty_string,
                1,
                0,
                function_proto,
                Some([ctor, on_finally].into()), // see callback for data usage
            )?
            .into(),
            NativeFunction::create(
                ctx,
                Callback::Func(js_promise_then_finally_func),
                Atom::CONST_empty_string,
                1,
                1,
                function_proto,
                Some([ctor, on_finally].into()), // see callback for data usage
            )?
            .into(),
        ]
    };

    cc.this().invoke(ctx, Atom::CONST_then, &then_args)
}

/// returned value is Promise object or Exception.
pub(crate) fn js_promise_resolve(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_reject = cc.magic() != 0;

    let this = cc.this();
    if !this.is_object() {
        return Err(ctx.throw_type_error_not_an_object());
    }

    let resolution = cc.arg(0);

    if !is_reject && resolution.as_object_Promise().is_some() {
        // resolve and argument #0 is Promise
        let ctor = resolution.get_property(ctx, Atom::CONST_constructor)?;
        if ctor.is_same(this) {
            return Ok(resolution);
        }
    }

    let (result_promise, resolving_funcs) = Promise::create_with_ctor(ctx, this)?;
    resolving_funcs[is_reject as usize].call(
        ctx,
        JSValue::UNDEFINED,
        std::slice::from_ref(&resolution),
    )?;

    Ok(result_promise.into())
}

pub fn js_promise_all(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_promise_any = cc.magic() == PROMISE_MAGIC_any;

    let this = cc.this();
    if !this.is_object() {
        return Err(ctx.throw_type_error_not_an_object());
    }

    let (result_promise, resolving_funcs) = Promise::create_with_ctor(ctx, this)?;
    debug_assert!(result_promise.gc_node().is_local());

    // protect `resolving_funcs` before promise.call()
    // {
    //     let heap = ctx.gc_heap_mut();
    //     if let Some(n) = resolving_funcs[0].gc_node_ptr() {
    //         heap.protect_node(n);
    //     }
    //     if let Some(n) = resolving_funcs[1].gc_node_ptr() {
    //         heap.protect_node(n);
    //     }
    // }

    let fail_reject = |cx: &mut ScopedContext| -> JSResult {
        let err_val = cx.take_exception().to_value(cx);
        resolving_funcs[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;

        debug_assert!(result_promise.gc_node().is_local());
        Ok(JSValue::from(result_promise))
    };

    let promise_resolve = if let Ok(x) = cc.this().get_property(ctx, Atom::CONST_resolve) {
        if unlikely(x.ensure_callable(ctx).is_err()) {
            return fail_reject(ctx);
        } else {
            x
        }
    } else {
        return fail_reject(ctx);
    };

    let items = cc.arg(0);
    let Ok(mut it) = items.iter(ctx) else {
        return fail_reject(ctx);
    };

    let Ok(arr) = crate::array::Array::create(ctx) else {
        return fail_reject(ctx);
    };
    let Ok(resolve_element_env) = crate::array::Array::from_values(ctx, vec![JSValue::from(1)])
    else {
        return fail_reject(ctx);
    };

    // protect arr, resolve_element_env - THEY'RE IN SCOPE, NOT NEEDED ANY MORE
    // {
    //     let heap=ctx.gc_heap_mut();
    //     heap.protect_node(arr.gc_node_ptr());
    //     heap.protect_node(resolve_element_env.as_object().unwrap().gc_node_ptr());
    // }

    let mut values: JSValue = arr.into();
    let mut index = 0;

    while let Some(a) = it.next() {
        if let Ok(item) = a {
            let Ok(next_promise) =
                promise_resolve.call(ctx, cc.this(), std::slice::from_ref(&item))
            else {
                return fail_reject(ctx);
            };

            let Ok(resolve_element) = NativeFunction::create(
                ctx,
                Callback::Func(js_promise_all_resolve_element),
                Atom::CONST_empty_string,
                1,
                cc.magic() as _,
                ctx.function_proto(),
                Some(
                    [
                        JSValue::from(false),
                        JSValue::from(index),
                        values,
                        resolving_funcs[is_promise_any as usize],
                        resolve_element_env.into(),
                    ]
                    .into(),
                ),
            ) else {
                return fail_reject(ctx);
            };
            let mut resolve_element = JSValue::from(resolve_element);

            let reject_element: JSValue = match cc.magic() {
                PROMISE_MAGIC_allSettled => {
                    let Ok(r) = NativeFunction::create(
                        ctx,
                        Callback::Func(js_promise_all_resolve_element),
                        Atom::CONST_empty_string,
                        1,
                        (cc.magic() | 4) as _,
                        ctx.function_proto(),
                        Some(
                            [
                                JSValue::from(false),
                                JSValue::from(index),
                                values,
                                resolving_funcs[is_promise_any as usize],
                                resolve_element_env.into(),
                            ]
                            .into(),
                        ),
                    ) else {
                        return fail_reject(ctx);
                    };
                    JSValue::from(r)
                }
                PROMISE_MAGIC_any => {
                    if values
                        .define_property_value_u32(ctx, index as u32, JSValue::UNDEFINED, 0x7)
                        .is_err()
                    {
                        return fail_reject(ctx);
                    } else {
                        std::mem::replace(&mut resolve_element, resolving_funcs[0]) // take out old value
                    }
                }
                _ => resolving_funcs[1],
            };

            if remainingElementsCount_add(ctx, resolve_element_env.into(), 1).is_err() {
                return fail_reject(ctx);
            }

            // call wrapper promise's then() method.
            // next_promise, resolve_element, reject_element are protected
            // by StackFrame (gc will trace stackframe values), no manual required.
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
            // return fail_reject(ctx);

            debug_assert!(result_promise.gc_node().is_local());

            let err_val = ctx.take_exception().to_value(ctx);
            resolving_funcs[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;

            debug_assert!(result_promise.gc_node().is_local());
            return Ok(JSValue::from(result_promise));
        }
    }

    match remainingElementsCount_add(ctx, resolve_element_env.into(), -1) {
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

// Build a plain object mapping each keyed key to its resolved value. Used both
// for the empty-input fast path and when the last element settles.
fn js_promise_all_keyed_combine(
    ctx: &mut ScopedContext,
    keys: JSValue,
    values: JSValue,
) -> JSResult {
    let len = keys.objectify(ctx)?.get_length_prop(ctx)? as usize;
    let mut obj = ctx.create_base_object_with_proto(None)?;
    for i in 0..len {
        let key_val = keys.get_property_u32(ctx, i as u32)?;
        let v = values.get_property_u32(ctx, i as u32)?;
        let atom = key_val.to_atom(ctx)?;
        obj.define_property_value(ctx, atom, v, JS_PROP_C_W_E)?;
    }
    Ok(obj.into())
}

pub fn js_promise_all_keyed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_all_settled = cc.magic() == PROMISE_MAGIC_allSettledKeyed;

    let this = cc.this();
    if !this.is_object() {
        return Err(ctx.throw_type_error_not_an_object());
    }

    let (result_promise, resolving_funcs) = Promise::create_with_ctor(ctx, this)?;
    debug_assert!(result_promise.gc_node().is_local());

    // IfAbruptRejectPromise: reject the result promise and RETURN it (do not
    // throw synchronously). If a real exception is pending (e.g. a property
    // access threw), propagate that exact error; otherwise raise a TypeError
    // for the spec-mandated early rejections (non-object argument / non-callable
    // `resolve`).
    let fail_reject = |cx: &mut ScopedContext| -> JSResult {
        let exc = cx.take_exception();
        let err_val = exc.to_value(cx);
        let err_val = if err_val.is_undefined() {
            match cx.create_error_object(
                ErrorKind::Type,
                "Promise.allKeyed: 'resolve' is not a function or the argument is not an object",
                false,
            ) {
                Some(obj) => obj.into(),
                None => JSValue::UNDEFINED,
            }
        } else {
            err_val
        };
        resolving_funcs[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
        debug_assert!(result_promise.gc_node().is_local());
        Ok(JSValue::from(result_promise))
    };

    let promise_resolve = if let Ok(x) = cc.this().get_property(ctx, Atom::CONST_resolve) {
        if unlikely(!x.is_callable()) {
            return fail_reject(ctx);
        } else {
            x
        }
    } else {
        return fail_reject(ctx);
    };

    let Some(mut items_obj) = cc.arg(0).as_object() else {
        return fail_reject(ctx);
    };

    let pkeys = match items_obj.get_own_property_names(
        ctx,
        JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK,
    ) {
        Ok(k) => k,
        Err(_) => return fail_reject(ctx),
    };
    let n = pkeys.len();

    let Ok(values) = Array::from_values(ctx, vec![JSValue::UNDEFINED; n]) else {
        return fail_reject(ctx);
    };
    let Ok(resolve_element_env) = Array::from_values(ctx, vec![JSValue::from(1)]) else {
        return fail_reject(ctx);
    };

    let mut key_vals: Vec<JSValue> = Vec::with_capacity(n);
    for pk in pkeys.iter() {
        key_vals.push(pk.atom.to_value(ctx)?);
    }
    let Ok(keys) = Array::from_values(ctx, key_vals) else {
        return fail_reject(ctx);
    };

    let values_val: JSValue = values.into();
    let keys_val: JSValue = keys.into();
    let resolve_element_env_val: JSValue = resolve_element_env.into();

    for index in 0..n {
        let pk = &pkeys[index];
        let item = match items_obj.get_property(ctx, pk.atom) {
            Ok(v) => v,
            Err(_) => return fail_reject(ctx),
        };

        let Ok(next_promise) = promise_resolve.call(ctx, cc.this(), std::slice::from_ref(&item))
        else {
            return fail_reject(ctx);
        };

        // Per-key already_called flag: the resolve element and its reject twin
        // (allSettledKeyed) must share one, distinct keys must not.
        let Ok(already_called) = Array::from_values(ctx, vec![JSValue::from(false)]) else {
            return fail_reject(ctx);
        };
        let already_called_val: JSValue = already_called.into();

        let Ok(resolve_element) = NativeFunction::create(
            ctx,
            Callback::Func(js_promise_all_keyed_resolve_element),
            Atom::CONST_empty_string,
            1,
            (if is_all_settled {
                PROMISE_MAGIC_allSettled
            } else {
                PROMISE_MAGIC_all
            }) as _,
            ctx.function_proto(),
            Some(
                [
                    already_called_val,
                    JSValue::from(index as i32),
                    values_val,
                    keys_val,
                    resolving_funcs[0],
                    resolve_element_env_val,
                    resolving_funcs[1],
                ]
                .into(),
            ),
        ) else {
            return fail_reject(ctx);
        };
        let resolve_element = JSValue::from(resolve_element);

        let reject_element: JSValue = if is_all_settled {
            let Ok(reject_element) = NativeFunction::create(
                ctx,
                Callback::Func(js_promise_all_keyed_resolve_element),
                Atom::CONST_empty_string,
                1,
                (PROMISE_MAGIC_allSettled | 4) as _,
                ctx.function_proto(),
                Some(
                    [
                        already_called_val,
                        JSValue::from(index as i32),
                        values_val,
                        keys_val,
                        resolving_funcs[0],
                        resolve_element_env_val,
                        resolving_funcs[1],
                    ]
                    .into(),
                ),
            ) else {
                return fail_reject(ctx);
            };
            JSValue::from(reject_element)
        } else {
            resolving_funcs[1]
        };

        if remainingElementsCount_add(ctx, resolve_element_env_val, 1).is_err() {
            return fail_reject(ctx);
        }

        if next_promise
            .invoke(ctx, Atom::CONST_then, &[resolve_element, reject_element])
            .is_err()
        {
            return fail_reject(ctx);
        }
    }

    // Account for the initial "1" added so the first element to settle does not
    // resolve prematurely. When n == 0 this returns 1 and we resolve now.
    match remainingElementsCount_add(ctx, resolve_element_env_val, -1) {
        Ok(1) => {
            let obj = js_promise_all_keyed_combine(ctx, keys_val, values_val)?;
            if resolving_funcs[0]
                .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&obj))
                .is_err()
            {
                return fail_reject(ctx);
            }
        }
        Ok(0) => {}
        Err(_) => return fail_reject(ctx),
        _ => unreachable!(),
    }

    Ok(result_promise.into())
}

pub(crate) fn js_promise_all_keyed_resolve_element(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    let resolve_type = cc.magic() & 3;
    let is_reject = cc.magic() & 4 != 0;

    let mut already_called = cc.data(0);
    let called = already_called.get_property_u32(ctx, 0)?.to_bool()?;
    if called {
        return Ok(JSValue::UNDEFINED);
    }
    already_called.define_property_value_u32(ctx, 0, JSValue::from(true), JS_PROP_C_W_E)?;

    let index = cc.data(1).to_number(ctx)?.to_i32();
    let mut values = cc.data(2);
    let keys = cc.data(3);
    let resolve = cc.data(4);
    let resolve_element_env = cc.data(5);

    let value_to_store = if resolve_type == PROMISE_MAGIC_allSettled {
        let mut ob = ctx.create_base_object()?;
        let status_val =
            ctx.create_string_from_str(if is_reject { "rejected" } else { "fulfilled" })?;
        ob.define_property_value(
            ctx,
            Atom::CONST_status,
            JSValue::make_string(status_val),
            JS_PROP_C_W_E,
        )?;
        ob.define_property_value(
            ctx,
            if is_reject {
                Atom::CONST_reason
            } else {
                Atom::CONST_value
            },
            cc.arg(0),
            JS_PROP_C_W_E,
        )?;
        ob.into()
    } else {
        cc.arg(0)
    };

    // Protect value_to_store from GC before writing into values;
    // define_property_value_u32 may allocate and trigger arena GC.
    ctx.gc_protect_transient(&value_to_store);
    values.define_property_value_u32(ctx, index as u32, value_to_store, JS_PROP_C_W_E)?;

    match remainingElementsCount_add(ctx, resolve_element_env, -1)? {
        1 => {
            let obj = js_promise_all_keyed_combine(ctx, keys, values)?;
            if resolve
                .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&obj))
                .is_err()
            {
                let err_val = ctx.take_exception().to_value(ctx);
                cc.data(6)
                    .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
            }
        }
        0 => {}
        _ => unreachable!(),
    }

    Ok(JSValue::UNDEFINED)
}

pub fn js_promise_race(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    if !this.is_object() {
        return Err(ctx.throw_type_error_not_an_object());
    }

    let (promise, resolving_funcs) = Promise::create_with_ctor(ctx, this)?;
    debug_assert!(promise.gc_node().is_local());

    let fail_reject = |cx: &mut ScopedContext| -> JSResult {
        let err_val = cx.take_exception().to_value(cx);
        resolving_funcs[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
        Ok(promise.into())
    };

    let promise_resolve = this
        .get_property(ctx, Atom::CONST_resolve)
        .unwrap_or(JSValue::EXCEPTION);

    if unlikely(promise_resolve.is_exception() || promise_resolve.ensure_callable(ctx).is_err()) {
        fail_reject(ctx)
    } else {
        let obj = cc.arg(0);
        let Ok(mut it) = obj.iter(ctx) else {
            return fail_reject(ctx);
        };

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

        Ok(promise.into())
    }
}

pub fn js_promise_withResolvers(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    if !this.is_object() {
        return Err(ctx.throw_type_error_not_an_object());
    }

    let (result_promise, resolving_funcs) = Promise::create_with_ctor(ctx, this)?;
    debug_assert!(result_promise.gc_node().is_local());

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

fn js_promise_try(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    if !this.is_object() {
        return Err(ctx.throw_type_error_not_an_object());
    }

    let (result_promise, resolving_funcs) = Promise::create_with_ctor(ctx, this)?;

    let fn_args = if cc.length() > 1 {
        &cc.arguments()[1..]
    } else {
        &[]
    };

    match cc.arg(0).call(ctx, JSValue::UNDEFINED, fn_args) {
        Ok(val) => {
            resolving_funcs[0].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&val))?;
        }
        Err(_) => {
            let err_val = ctx.take_exception().to_value(ctx);
            resolving_funcs[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
        }
    }

    Ok(result_promise.into())
}

pub(super) const js_promise_funcs: &[FieldDef] = &[
    FieldDef::FUNC("resolve", 1, js_promise_resolve, 0),
    FieldDef::FUNC("reject", 1, js_promise_resolve, 1),
    FieldDef::FUNC("all", 1, js_promise_all, PROMISE_MAGIC_all as _),
    FieldDef::FUNC(
        "allSettled",
        1,
        js_promise_all,
        PROMISE_MAGIC_allSettled as _,
    ),
    FieldDef::FUNC("any", 1, js_promise_all, PROMISE_MAGIC_any as _),
    FieldDef::FUNC(
        "allKeyed",
        1,
        js_promise_all_keyed,
        PROMISE_MAGIC_allKeyed as _,
    ),
    FieldDef::FUNC(
        "allSettledKeyed",
        1,
        js_promise_all_keyed,
        PROMISE_MAGIC_allSettledKeyed as _,
    ),
    FieldDef::FUNC("race", 1, js_promise_race, 0),
    FieldDef::FUNC("try", 1, js_promise_try, 0),
    FieldDef::FUNC("withResolvers", 0, js_promise_withResolvers, 0),
    SPECIES_FIELD,
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

pub(super) const js_promise_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("then", 2, js_promise_then, 0),
    FieldDef::FUNC("catch", 1, js_promise_catch, 0),
    FieldDef::FUNC("finally", 1, js_promise_finally, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Promise", JS_PROP_CONFIGURABLE),
];
