// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Callback, JS_CLASS_PROMISE,
    atom::Atom,
    builtins::{error::js_aggregate_error_constructor, js_get_this},
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunction},
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

fn js_promise_then(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this();
    if let Some(p) = this.as_object_Promise_mut() {
        p.then(ctx, cc.arg(0), cc.arg(1)).map(|p| p.into())
    } else {
        ctx.throw_type_error_invalid_class(JS_CLASS_PROMISE);
        Err(-1)
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
                cx.throw(val); // reject
                Err(-1)
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

    let then_funcs: [JSValue; 2] = if !on_finally.is_function() {
        [on_finally, on_finally]
    } else {
        let ctor = JS_SpeciesConstructor(ctx, cc.this(), JSValue::UNDEFINED)?;
        [
            NativeFunction::create(
                ctx,
                Callback::Func(js_promise_then_finally_func),
                Atom::CONST_empty_string,
                1,
                0,
                ctx.function_proto(),
                Some([ctor, on_finally].into()), // see callback for data usage
            )?
            .into(),
            NativeFunction::create(
                ctx,
                Callback::Func(js_promise_then_finally_func),
                Atom::CONST_empty_string,
                1,
                1,
                ctx.function_proto(),
                Some([ctor, on_finally].into()), // see callback for data usage
            )?
            .into(),
        ]
    };

    cc.this().invoke(ctx, Atom::CONST_then, &then_funcs)
}

/// returned value is Promise object or Exception.
pub(crate) fn js_promise_resolve(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_reject = cc.magic() != 0;

    let this = cc.this();
    if !this.is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let resolution = cc.arg(0);

    if !is_reject
        && resolution
            .retrive_object_payload0(JS_CLASS_PROMISE)
            .is_some()
    {
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
        ctx.throw_type_error_not_an_object();
        return Err(-1);
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
        let error = cx.rt_mut().take_exception();
        resolving_funcs[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&error))?;

        debug_assert!(result_promise.gc_node().is_local());
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

    let items = cc.arg(0);
    let Ok(mut it) = items.iter(ctx) else {
        return fail_reject(ctx);
    };

    let Ok(arr) = ctx.create_array() else {
        return fail_reject(ctx);
    };
    let Ok(resolve_element_env) = ctx.create_array_with_values(vec![JSValue::Int(1)]) else {
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
                        JSValue::Bool(false),
                        JSValue::Int(index),
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
                                JSValue::Bool(false),
                                JSValue::Int(index),
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
            // next_promise, resolve_element, reject_element will be protected
            // by StackFrame, no manual required.
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

pub fn js_promise_race(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    if !this.is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let (promise, resolving_funcs) = Promise::create_with_ctor(ctx, this)?;
    debug_assert!(promise.gc_node().is_local());

    // protect resoving_funcs before calling promise.call()
    // {
    //     let heap = ctx.gc_heap_mut();
    //     if let Some(n) = resolving_funcs[0].gc_node_ptr() {
    //         heap.protect_node_v2(n);
    //     }
    //     if let Some(n) = resolving_funcs[1].gc_node_ptr() {
    //         heap.protect_node_v2(n);
    //     }
    // }

    let fail_reject = |cx: &mut ScopedContext| -> JSResult {
        let error = cx.rt_mut().take_exception();
        resolving_funcs[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&error))?;
        Ok(promise.into())
    };

    let promise_resolve = this
        .get_property(ctx, Atom::CONST_resolve)
        .unwrap_or(JSValue::EXCEPTION);

    if unlikely(promise_resolve.is_exception() || promise_resolve.ensure_function(ctx).is_err()) {
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
        ctx.throw_type_error_not_an_object();
        return Err(-1);
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
