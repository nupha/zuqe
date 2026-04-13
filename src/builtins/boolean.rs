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

pub const CLASS_DEF_BOOLEAN: ClassDef = ClassDef {
    name: "",
    name_atom: crate::Atom::CONST_Boolean,
    proto_entries: js_boolean_proto_funcs,
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: crate::class::ClassInitType::Entries {
        proto_entries: js_boolean_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef {
            callback: crate::Callback::ConstructorOrFunc(js_boolean_constructor),
            length: 1,
            magic: 0,
            entries: &[],
        }),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
