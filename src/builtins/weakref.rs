// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx, GcWeak};

use crate::{
    Atom, ClassDef,
    class::ClassCtorDef,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{JS_PROP_CONFIGURABLE, Object, ObjectImpl, ObjectPayload},
    value::JSValue,
};

/// Internal data for a WeakRef instance.
pub(crate) struct WeakRefData {
    target: GcWeak<ObjectImpl>,
}

impl std::fmt::Debug for WeakRefData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakRefData").finish()
    }
}

impl GcTrace for WeakRefData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

fn js_weakref_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if new_target.is_undefined() {
        ctx.throw_type_error("constructor WeakRef requires 'new'");
        return Err(-1);
    }

    let target_val = cc.arg(0);
    let target_obj = match target_val.as_object() {
        Some(o) => *o,
        None => {
            ctx.throw_type_error("WeakRef target must be an object");
            return Err(-1);
        }
    };

    let weak = target_obj.downgrade();

    let obj = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::WEAKREF(Box::new(WeakRefData { target: weak })),
        )
        .map_err(|_| -1)?;

    Ok(JSValue::Object(obj))
}

fn js_weakref_deref(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = match cc.this().as_object() {
        Some(o) => *o,
        None => {
            ctx.throw_type_error("WeakRef.prototype.deref requires a WeakRef");
            return Err(-1);
        }
    };

    let wrd = match this_obj.payload().weak_ref() {
        Some(d) => d,
        None => {
            ctx.throw_type_error("WeakRef.prototype.deref called on non-WeakRef");
            return Err(-1);
        }
    };

    let heap = ctx.rt().gc_heap();
    match wrd.target.upgrade(heap) {
        Some(gc_ref) => Ok(JSValue::Object(Object::from(gc_ref.into_raw()))),
        None => Ok(JSValue::UNDEFINED),
    }
}

static WEAKREF_PROTO_FUNCS: &[FieldDef] = &[
    FieldDef::FUNC("deref", 0, js_weakref_deref, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "WeakRef",
        JS_PROP_CONFIGURABLE as u32,
    ),
];

pub(crate) static CLASS_DEF_WEAKREF: ClassDef = ClassDef::builtin(
    Atom::CONST_WeakRef,
    WEAKREF_PROTO_FUNCS,
    Some(ClassCtorDef::new(js_weakref_constructor, 1, false).expose_to_global()),
);

//
// FinalizationRegistry
//

struct FinRecEntry {
    target: GcWeak<ObjectImpl>,
    held_value: JSValue,
    token: Option<GcWeak<ObjectImpl>>,
}

impl std::fmt::Debug for FinRecEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinRecEntry").finish()
    }
}

impl GcTrace for FinRecEntry {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.held_value.trace(tr);
    }
}

/// Internal data for a FinalizationRegistry instance.
pub(crate) struct FinalizationRegistryData {
    pub(crate) callback: JSValue,
    entries: Vec<FinRecEntry>,
    pub(crate) realm: ContextPtr,
}

impl std::fmt::Debug for FinalizationRegistryData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinalizationRegistryData")
            .field("entries_len", &self.entries.len())
            .field("realm", &self.realm)
            .finish()
    }
}

impl GcTrace for FinalizationRegistryData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.callback.trace(tr);
        for e in self.entries.iter() {
            e.trace(tr);
        }
    }
}

impl FinalizationRegistryData {
    /// Remove entries whose target matches the given `dead_target`.
    /// Returns the held_values of removed entries.
    pub(crate) fn remove_entries_for_target(
        &mut self,
        dead_target: Object,
        heap: &gc_lite::GcHeap,
    ) -> Vec<JSValue> {
        let mut removed = Vec::new();
        self.entries.retain(|e| match e.target.upgrade(heap) {
            Some(gc_ref) => {
                let obj = Object::from(gc_ref.into_raw());
                if obj == dead_target {
                    removed.push(e.held_value);
                    false // remove
                } else {
                    true // keep
                }
            }
            None => {
                removed.push(e.held_value);
                false // remove
            }
        });
        removed
    }
}

fn js_finrec_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if new_target.is_undefined() {
        ctx.throw_type_error("constructor FinalizationRegistry requires 'new'");
        return Err(-1);
    }

    let cb = cc.arg(0);
    if !cb.is_callable() {
        ctx.throw_type_error("FinalizationRegistry argument must be a function");
        return Err(-1);
    }

    let ctx_ptr = ctx.ctx_ptr;

    let data = FinalizationRegistryData {
        callback: cb,
        entries: Vec::new(),
        realm: ctx_ptr,
    };

    let obj = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::FINALIZATION_REGISTRY(Box::new(data)),
        )
        .map_err(|_| -1)?;

    Ok(JSValue::Object(obj))
}

fn js_finrec_register(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this_obj = match cc.this().as_object() {
        Some(o) => *o,
        None => {
            ctx.throw_type_error(
                "FinalizationRegistry.prototype.register requires a FinalizationRegistry",
            );
            return Err(-1);
        }
    };

    let frd = match this_obj.payload_mut().finalization_registry_mut() {
        Some(d) => d,
        None => {
            ctx.throw_type_error(
                "FinalizationRegistry.prototype.register called on non-FinalizationRegistry",
            );
            return Err(-1);
        }
    };

    let target_val = cc.arg(0);
    let held_val = cc.arg(1);
    let token_val = cc.arg(2);

    let mut target_obj = match target_val.as_object() {
        Some(o) => *o,
        None => {
            ctx.throw_type_error(
                "FinalizationRegistry.prototype.register: target must be an object",
            );
            return Err(-1);
        }
    };

    // held_value cannot be the same object as target (SameValue check)
    if target_val.is_object()
        && held_val.is_object()
        && target_val.as_object().unwrap() == held_val.as_object().unwrap()
    {
        ctx.throw_type_error(
            "FinalizationRegistry.prototype.register: held value cannot be the target",
        );
        return Err(-1);
    }

    // Validate unregister token: CanBeHeldWeakly check
    // Only objects can be held weakly; null, booleans, numbers, strings, symbols throw TypeError.
    if !token_val.is_undefined() && token_val.as_object().is_none() {
        ctx.throw_type_error(
            "FinalizationRegistry.prototype.register: unregisterToken must be an object or undefined",
        );
        return Err(-1);
    }

    let target_weak = target_obj.downgrade();
    let token_weak = if token_val.is_undefined() {
        None
    } else {
        let tok_obj = *token_val.as_object().unwrap();
        Some(tok_obj.downgrade())
    };

    let entry = FinRecEntry {
        target: target_weak,
        held_value: held_val,
        token: token_weak,
    };
    frd.entries.push(entry);

    // Add back-reference from target to this FinalizationRegistry
    target_obj.add_back_ref(this_obj.downgrade());

    Ok(JSValue::UNDEFINED)
}

fn js_finrec_unregister(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this_obj = match cc.this().as_object() {
        Some(o) => *o,
        None => {
            ctx.throw_type_error(
                "FinalizationRegistry.prototype.unregister requires a FinalizationRegistry",
            );
            return Err(-1);
        }
    };

    let frd = match this_obj.payload_mut().finalization_registry_mut() {
        Some(d) => d,
        None => {
            ctx.throw_type_error(
                "FinalizationRegistry.prototype.unregister called on non-FinalizationRegistry",
            );
            return Err(-1);
        }
    };

    let token_val = cc.arg(0);
    // CanBeHeldWeakly check — only objects can be held weakly.
    // Unlike register(), there is no exception for undefined in unregister.
    let token_obj = match token_val.as_object() {
        Some(o) => *o,
        None => {
            ctx.throw_type_error(
                "FinalizationRegistry.prototype.unregister: unregister token must be an object",
            );
            return Err(-1);
        }
    };

    let heap = ctx.rt().gc_heap();
    let before = frd.entries.len();

    frd.entries.retain(|e| match &e.token {
        Some(tok_weak) => match tok_weak.upgrade(heap) {
            Some(gc_ref) => {
                let obj = Object::from(gc_ref.into_raw());
                obj != token_obj
            }
            None => true,
        },
        None => true,
    });

    let removed = before != frd.entries.len();
    Ok(JSValue::Bool(removed))
}

static FINREC_PROTO_FUNCS: &[FieldDef] = &[
    FieldDef::FUNC("register", 2, js_finrec_register, 0),
    FieldDef::FUNC("unregister", 1, js_finrec_unregister, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "FinalizationRegistry",
        JS_PROP_CONFIGURABLE as u32,
    ),
];

pub(crate) static CLASS_DEF_FINALIZATION_REGISTRY: ClassDef = ClassDef::builtin(
    Atom::CONST_FinalizationRegistry,
    FINREC_PROTO_FUNCS,
    Some(ClassCtorDef::new(js_finrec_constructor, 1, false).expose_to_global()),
);
