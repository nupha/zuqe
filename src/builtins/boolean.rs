// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use crate::{
    ClassDef, JS_CLASS_BOOLEAN,
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    value::JSValue,
};

pub fn js_boolean_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let b = cc.arg(0).to_bool().unwrap_or(true);

    if !new_target.is_undefined() {
        ctx.ordinary_create_from_constructor_class(
            new_target,
            JS_CLASS_BOOLEAN,
            Box::into_raw(Box::new(b)) as *mut c_void,
        )
        .map(JSValue::from)
        .map_err(|_| -1)
    } else {
        Ok(b.into())
    }
}

fn js_boolean_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(b) = cc.this().as_bool() else {
        return Err(ctx.throw_type_error("not a boolean"));
    };

    if b {
        Atom::CONST_true
    } else {
        Atom::CONST_false
    }
    .force_string(ctx)
    .map(JSValue::make_string)
}

fn js_boolean_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    cc.this()
        .as_bool()
        .ok_or_else(|| ctx.throw_type_error("not a boolean"))
        .map(JSValue::from)
}

pub const js_boolean_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_boolean_toString, 0),
    FieldDef::FUNC("valueOf", 0, js_boolean_valueOf, 0),
];

pub(crate) static CLASS_DEF_BOOLEAN: ClassDef = ClassDef::builtin(
    crate::Atom::CONST_Boolean,
    js_boolean_proto_funcs,
    Some(crate::class::ClassCtorDef::new(js_boolean_constructor, 1, true).expose_to_global()),
);
