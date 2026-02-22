// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ContextImpl,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::ObjectPayload,
    value::JSValue,
};

pub fn js_boolean_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let new_target = *cc.this();
    let b = cc.arg(0).to_bool().unwrap_or(true);

    if !new_target.is_undefined() {
        ctx.create_object_from_ctor(new_target, ObjectPayload::BOOLEAN(b))
            .map(JSValue::Object)
            .map_err(|_| -1)
    } else {
        Ok(b.into())
    }
}

pub fn js_thisBooleanValue(ctx: &ContextImpl, this_val: JSValue) -> JSResult {
    this_val.as_bool().map(JSValue::Bool).ok_or_else(|| {
        ctx.throw_type_error("not a boolean");
        -1
    })
}

fn js_boolean_toString(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    match js_thisBooleanValue(ctx, *cc.this())? {
        JSValue::Bool(true) => Atom::CONST_true,
        JSValue::Bool(false) => Atom::CONST_false,
        _ => {
            ctx.throw_type_error("not a boolean");
            return Err(-1);
        }
    }
    .force_string(ctx)
    .map(JSValue::String)
}

fn js_boolean_valueOf(cc: CallContext) -> JSResult {
    js_thisBooleanValue(cc.ctx(), *cc.this())
}

pub const js_boolean_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("toString", 0, js_boolean_toString, 0),
    FieldDef::CALL("valueOf", 0, js_boolean_valueOf, 0),
];
