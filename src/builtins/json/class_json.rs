// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE,
        PropertyKey,
    },
    runtime::js_check_stack_overflow,
    value::JSValue,
};

fn js_json_stringify(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    let replacer = cc.arg(1);
    let space = cc.arg(2);

    ctx.with_new_scope(|mut scope| scope.json_stringify(obj, replacer, space))
}

fn internalize_json_property(
    ctx: &mut ScopedContext,
    holder: JSValue,
    name: Atom,
    reviver: JSValue,
) -> JSResult {
    if js_check_stack_overflow(ctx.rt(), 0) {
        ctx.throw_stack_overflow();
        return Err(-1);
    }

    let mut val = holder.get_property(ctx, name)?;

    if let Some(o) = val.as_object().copied() {
        let is_array = o.is_array(ctx)?;
        let len: u32;
        let pks: Box<[PropertyKey]>;

        if is_array {
            len = o.get_length_prop(ctx)? as u32;
            pks = [].into();
        } else {
            pks = o.get_own_property_names(ctx, JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK)?;
            len = pks.len() as u32;
        }

        for i in 0..len {
            let prop = if is_array {
                Atom::from_i64(ctx.rt_ptr(), i as i64)?
            } else {
                unsafe { pks.get_unchecked(i as usize).atom }
            };

            let new_el = internalize_json_property(ctx, val, prop, reviver)?;
            if new_el.is_undefined() {
                val.delete_property(ctx, prop, 0)
            } else {
                val.define_property_value(ctx, prop, new_el, JS_PROP_C_W_E)
            }?;
        }
    }

    let name_val = name.to_value(ctx)?;
    reviver.call(ctx, holder, &[name_val, val])
}

fn js_json_parse(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;

    let mut obj = ctx.with_new_scope(|scope| -> JSResult {
        scope.json_parse(&s.content().encode_utf8_bytes(true), "<input>", 0)
    })?;

    if cc.length() > 1 && cc.arg(1).is_function() {
        let reviver = cc.arg(1);
        let mut root: JSValue = ctx.create_base_object()?.into();
        root.define_property_value(ctx, Atom::CONST_empty_string, obj, JS_PROP_C_W_E)?;
        obj = internalize_json_property(ctx, root, Atom::CONST_empty_string, reviver)?;
    }

    Ok(obj)
}

const js_json_funcs: &[FieldDef] = &[
    FieldDef::FUNC("parse", 2, js_json_parse, 0),
    FieldDef::FUNC("stringify", 3, js_json_stringify, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "JSON", JS_PROP_CONFIGURABLE),
];

pub const js_json_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "JSON",
    js_json_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
