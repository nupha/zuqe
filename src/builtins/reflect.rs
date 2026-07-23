// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ScopedContext,
    builtins::{
        iterator::JSIteratorKindEnum,
        object::{
            js_object_defineProperty, js_object_getOwnPropertyDescriptor, js_object_getPrototypeOf,
            js_object_isExtensible, js_object_preventExtensions,
        },
    },
    error::JSResult,
    function::{CallContext, FieldDef, build_arg_list, js_function_apply},
    object::{JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
    value::JSValue,
};

fn js_reflect_apply(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    js_function_apply(
        ctx,
        CallContext::new(
            cc.arg(0),
            if cc.length() == 0 {
                &[]
            } else {
                &cc.arguments()[1..]
            },
            2,
            None,
        ),
    )
}

fn js_reflect_construct(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let func = cc.arg(0);
    let array_arg = cc.arg(1);

    let new_target = if cc.length() > 2 {
        let target = cc.arg(2);
        if !target.is_constructor() {
            ctx.throw_type_error("not a constructor");
            return Err(-1);
        }
        target
    } else {
        func
    };

    let args = build_arg_list(ctx, array_arg)?;
    func.call_ctor_new_target(ctx, new_target, &args)
}

fn js_reflect_deleteProperty(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(mut obj) = cc.arg(0).as_object() {
        let atom = cc.arg(1).to_atom(ctx)?;
        let ret = obj.delete_property(ctx, atom, 0);
        ret.map(JSValue::from)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_reflect_get(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj_val = cc.arg(0);
    obj_val.ensure_object(ctx)?;

    let atom = cc.arg(1).to_atom(ctx)?;
    let receiver = if cc.length() > 2 { cc.arg(2) } else { obj_val };

    obj_val.get_property_internal(ctx, atom, receiver, false)
}

fn js_reflect_has(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(obj) = cc.arg(0).as_object() {
        let prop = cc.arg(1);
        let atom = prop.to_atom(ctx)?;
        obj.has_property(ctx, atom).map(JSValue::from)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_reflect_set(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let target = cc.arg(0).as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let prop = cc.arg(1);
    let val = cc.arg(2);
    let receiver = if cc.length() > 3 {
        cc.arg(3)
    } else {
        JSValue::from(target)
    };
    let atom = prop.to_atom(ctx)?;

    target
        .set_property_internal(ctx, atom, val, receiver.as_object(), 0)
        .map(JSValue::from)
}

fn js_reflect_setPrototypeOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut val = cc.arg(0);
    val.set_prototype_internal(ctx, cc.arg(1), false)
        .map(JSValue::from)
}

fn js_reflect_ownKeys(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    if val.is_object() {
        val.build_own_property_kvs_array(
            ctx,
            JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK,
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
        )
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

const js_reflect_funcs: &[FieldDef] = &[
    FieldDef::FUNC("apply", 3, js_reflect_apply, 0),
    FieldDef::FUNC("construct", 2, js_reflect_construct, 0),
    FieldDef::FUNC("defineProperty", 3, js_object_defineProperty, 1),
    FieldDef::FUNC("deleteProperty", 2, js_reflect_deleteProperty, 0),
    FieldDef::FUNC("get", 2, js_reflect_get, 0),
    FieldDef::FUNC(
        "getOwnPropertyDescriptor",
        2,
        js_object_getOwnPropertyDescriptor,
        1,
    ),
    FieldDef::FUNC("getPrototypeOf", 1, js_object_getPrototypeOf, 1),
    FieldDef::FUNC("has", 2, js_reflect_has, 0),
    FieldDef::FUNC("isExtensible", 1, js_object_isExtensible, 1),
    FieldDef::FUNC("ownKeys", 1, js_reflect_ownKeys, 0),
    FieldDef::FUNC("preventExtensions", 1, js_object_preventExtensions, 1),
    FieldDef::FUNC("set", 3, js_reflect_set, 0),
    FieldDef::FUNC("setPrototypeOf", 2, js_reflect_setPrototypeOf, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "Reflect", JS_PROP_CONFIGURABLE),
];

pub const js_reflect_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "Reflect",
    js_reflect_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
