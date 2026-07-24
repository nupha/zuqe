// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::RefCell;

use zuqe::{
    CallContext, ClassCtorDef, FieldDef, JSResult, JSValue, ScopedContext, def_native_object,
};

/// Simple Emitter data stored as opaque
struct EmitterData {
    listeners: RefCell<Vec<(String, i32)>>, // simplified: (event_name, count)
}

fn get_emitter<'a>(ctx: &mut ScopedContext, val: &'a JSValue) -> Result<&'a EmitterData, i32> {
    JSEmitter::payload_of(val).ok_or_else(|| {
        ctx.throw_internal_error("not an Emitter instance");
        -1
    })
}

fn js_emitter_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    JSEmitter::new_instance(
        ctx,
        new_target,
        EmitterData {
            listeners: RefCell::new(Vec::new()),
        },
    )
    .map(JSValue::from)
}

fn js_emitter_on(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let emitter = get_emitter(ctx, &this)?;
    let event_name = cc.arg(0).to_string(ctx)?;
    let name_str = event_name.content().encode_utf8(false).to_string();

    let mut listeners = emitter.listeners.borrow_mut();
    if let Some(entry) = listeners.iter_mut().find(|(n, _)| n == &name_str) {
        entry.1 += 1;
    } else {
        listeners.push((name_str, 1));
    }

    Ok(this)
}

fn js_emitter_emit(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let emitter = get_emitter(ctx, &this)?;
    let event_name = cc.arg(0).to_string(ctx)?;
    let name_str = event_name.content().encode_utf8(false).to_string();

    let count = emitter
        .listeners
        .borrow()
        .iter()
        .find(|(n, _)| n == &name_str)
        .map(|(_, c)| *c)
        .unwrap_or(0);

    Ok(JSValue::from(count))
}

const CLASS_EMITTER_PROTO: &[FieldDef] = &[
    FieldDef::FUNC("on", 1, js_emitter_on, 0),
    FieldDef::FUNC("emit", 1, js_emitter_emit, 0),
];

def_native_object!(
    "Emitter",
    JSEmitter,
    EmitterData,
    CLASS_EMITTER_PROTO,
    Some(ClassCtorDef::new(js_emitter_constructor, 0, false).expose_to_global())
);
