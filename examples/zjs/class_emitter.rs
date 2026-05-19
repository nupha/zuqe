// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, ffi::c_void};

use zuqe::{
    CallContext, ClassCtorDef, ClassDef, ClassID, FieldDef, JSResult, JSValue, Object, Runtime,
    RuntimeImpl, ScopedContext,
};

/// Simple Emitter data stored as opaque
struct EmitterData {
    listeners: Vec<(String, i32)>, // simplified: (event_name, count)
}

fn js_emitter_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let class_id = JSEmitter::class_id();
    let data = Box::new(EmitterData {
        listeners: Vec::new(),
    });
    let opaque = Box::into_raw(data).cast::<c_void>();
    ctx.create_class_instance_with_opaque(new_target, class_id, opaque)
        .map(JSValue::from)
        .map_err(|e| {
            let p = e.other().unwrap();
            let _ = unsafe { Box::from_raw(p.opaque().cast::<EmitterData>()) };
            -1
        })
}

fn js_emitter_on(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let event_name = cc.arg(0).to_string(ctx)?;
    let name_str = event_name.content().encode_utf8(false).to_string();

    let opaque = this
        .retrive_object_payload(ctx, JSEmitter::class_id())?
        .other()
        .unwrap()
        .opaque();

    if !opaque.is_null() {
        let data = unsafe { &mut *opaque.cast::<EmitterData>() };
        if let Some(entry) = data.listeners.iter_mut().find(|(n, _)| n == &name_str) {
            entry.1 += 1;
        } else {
            data.listeners.push((name_str, 1));
        }
    }

    Ok(this)
}

fn js_emitter_emit(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let event_name = cc.arg(0).to_string(ctx)?;
    let name_str = event_name.content().encode_utf8(false).to_string();

    let opaque = this
        .retrive_object_payload(ctx, JSEmitter::class_id())?
        .other()
        .unwrap()
        .opaque();

    let mut count = 0;
    if !opaque.is_null() {
        let data = unsafe { &mut *opaque.cast::<EmitterData>() };
        if let Some(entry) = data.listeners.iter().find(|(n, _)| n == &name_str) {
            count = entry.1;
        }
    }

    Ok(JSValue::Int(count))
}

const CLASS_EMITTER_PROTO: &[FieldDef] = &[
    FieldDef::CALL("on", 1, js_emitter_on, 0),
    FieldDef::CALL("emit", 1, js_emitter_emit, 0),
];

pub struct JSEmitter;

impl JSEmitter {
    fn handle_dispose(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
        if !opaque.is_null() {
            let p = unsafe { Box::from_raw(opaque.cast::<EmitterData>()) };
            drop(p);
        }
    }

    thread_local! {
        static CLASS_ID: Cell<ClassID> = const { Cell::new(0) };
    }

    pub fn register(mut rt: Runtime) -> Result<ClassID, i32> {
        let class_id = rt
            .register_class(
                ClassDef::new(
                    "Emitter",
                    CLASS_EMITTER_PROTO,
                    Some(ClassCtorDef::new(js_emitter_constructor, 0, false).expose_to_global()),
                )
                .on_dispose(Self::handle_dispose),
            )
            .expect("register class Emitter failed");

        Self::CLASS_ID.with(|s| s.set(class_id));
        Ok(class_id)
    }

    #[inline(always)]
    pub fn class_id() -> ClassID {
        Self::CLASS_ID.with(|x| x.get())
    }
}
