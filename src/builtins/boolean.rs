// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ClassDef,
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::ObjectPayload,
    value::JSValue,
};

pub fn js_boolean_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let b = cc.arg(0).to_bool().unwrap_or(true);

    if !new_target.is_undefined() {
        ctx.create_object_from_ctor(new_target, ObjectPayload::BOOLEAN(b))
            .map(JSValue::Object)
            .map_err(|_| -1)
    } else {
        Ok(b.into())
    }
}

fn js_thisBooleanValue(ctx: &mut ScopedContext, this_val: JSValue) -> JSResult {
    this_val.as_bool().map(JSValue::Bool).ok_or_else(|| {
        ctx.throw_type_error("not a boolean");
        -1
    })
}

fn js_boolean_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    match js_thisBooleanValue(ctx, cc.this())? {
        JSValue::Bool(true) => Atom::CONST_true,
        JSValue::Bool(false) => Atom::CONST_false,
        _ => {
            ctx.throw_type_error("not a boolean");
            return Err(-1);
        }
    }
    .force_string(ctx)
    .map(JSValue::from)
}

fn js_boolean_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    js_thisBooleanValue(ctx, cc.this())
}

pub const js_boolean_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("toString", 0, js_boolean_toString, 0),
    FieldDef::CALL("valueOf", 0, js_boolean_valueOf, 0),
];

pub(crate) const CLASS_DEF_BOOLEAN: ClassDef = ClassDef::builtin(
    crate::Atom::CONST_Boolean,
    crate::class::ClassInitType::Normal {
        proto_entries: js_boolean_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_boolean_constructor, 1, true).export_to_global(),
        ),
    },
);
