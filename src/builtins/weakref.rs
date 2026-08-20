// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx, GcWeak};

use crate::{
    Atom, ClassDef,
    atom::AtomKind,
    class::ClassCtorDef,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{JS_PROP_CONFIGURABLE, Object, ObjectImpl, ObjectPayload},
    value::JSValue,
};

/// Internal data for a WeakRef instance.
/// Per ES2027: target can be an Object or a non-registered Symbol.
pub struct WeakRefData {
    target: WeakRefTarget,
}

enum WeakRefTarget {
    Object(GcWeak<ObjectImpl>),
    Symbol(JSValue),
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
        return Err(ctx.throw_type_error("constructor WeakRef requires 'new'"));
    }

    let target_val = cc.arg(0);
    // CanBeHeldWeakly check
    let target = if let Some(obj) = target_val.as_object() {
        WeakRefTarget::Object(obj.downgrade())
    } else if can_be_held_weakly(target_val) {
        WeakRefTarget::Symbol(target_val)
    } else {
        return Err(ctx.throw_type_error("WeakRef target must be an object"));
    };

    let obj = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::WEAKREF(Box::new(WeakRefData { target })),
        )
        .map_err(|_| -1)?;

    Ok(JSValue::from(obj))
}

fn js_weakref_deref(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(this_obj) = cc.this().as_object() else {
        return Err(ctx.throw_type_error("WeakRef.prototype.deref requires a WeakRef"));
    };

    let wrd = match this_obj.payload().weak_ref() {
        Some(d) => d,
        None => {
            return Err(ctx.throw_type_error("WeakRef.prototype.deref called on non-WeakRef"));
        }
    };

    match &wrd.target {
        WeakRefTarget::Object(w) => {
            let heap = ctx.rt().gc_heap();
            match w.upgrade(heap) {
                Some(gc_ref) => Ok(JSValue::from(Object::from(gc_ref.into_raw()))),
                None => Ok(JSValue::UNDEFINED),
            }
        }
        // Symbol targets are primitives, never collected — always return
        WeakRefTarget::Symbol(sym) => Ok(*sym),
    }
}

static WEAKREF_PROTO_FUNCS: &[FieldDef] = &[
    FieldDef::FUNC("deref", 0, js_weakref_deref, 0),
    FieldDef::STR("[Symbol.toStringTag]", "WeakRef", JS_PROP_CONFIGURABLE),
];

pub(crate) static CLASS_DEF_WEAKREF: ClassDef = ClassDef::builtin(
    Atom::CONST_WeakRef,
    WEAKREF_PROTO_FUNCS,
    Some(ClassCtorDef::new(js_weakref_constructor, 1, false).expose_to_global()),
);

//
/// Per ES2027 §9.13 CanBeHeldWeakly: Objects and non-registered Symbols are valid.
/// Registered symbols (Symbol.for()) are rejected because KeyForSymbol(arg) is not undefined.
fn can_be_held_weakly(val: JSValue) -> bool {
    if val.as_object().is_some() {
        return true;
    }
    if val.is_symbol()
        && let Some(sym) = val.as_symbol(false)
    {
        sym.atom_info()
            .is_some_and(|info| info.kind() != AtomKind::GlobalSymbol)
    } else {
        false
    }
}

// FinalizationRegistry
//

/// A weak reference that can hold either an object weak ref or a Symbol value.
/// Per ES2024 CanBeHeldWeakly: undefined, Object, and Symbol are valid.
enum WeakRef {
    Object(GcWeak<ObjectImpl>),
    Symbol(JSValue),
}

impl WeakRef {
    fn matches(&self, other: &JSValue, heap: &gc_lite::GcHeap) -> bool {
        match self {
            WeakRef::Object(w) => match w.upgrade(heap) {
                Some(gc_ref) => {
                    let obj = Object::from(gc_ref.into_raw());
                    other.as_object() == Some(obj)
                }
                None => false,
            },
            WeakRef::Symbol(val) => val == other,
        }
    }
}

impl std::fmt::Debug for WeakRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeakRef::Object(_) => write!(f, "WeakRef::Object(..)"),
            WeakRef::Symbol(v) => write!(f, "WeakRef::Symbol({v:?})"),
        }
    }
}

struct FinRecEntry {
    target: WeakRef,
    held_value: JSValue,
    token: Option<WeakRef>,
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
pub struct FinalizationRegistryData {
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
        self.entries.retain(|e| {
            match &e.target {
                WeakRef::Object(w) => match w.upgrade(heap) {
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
                },
                WeakRef::Symbol(_) => true, // Symbol targets are never collected
            }
        });
        removed
    }
}

fn js_finrec_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if new_target.is_undefined() {
        return Err(ctx.throw_type_error("constructor FinalizationRegistry requires 'new'"));
    }

    let cb = cc.arg(0);
    if !cb.is_callable() {
        return Err(ctx.throw_type_error("FinalizationRegistry argument must be a function"));
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

    Ok(JSValue::from(obj))
}

fn js_finrec_register(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(this_obj) = cc.this().as_object() else {
        return Err(ctx.throw_type_error(
            "FinalizationRegistry.prototype.register requires a FinalizationRegistry",
        ));
    };

    let target_val = cc.arg(0);
    let held_val = cc.arg(1);
    let token_val = cc.arg(2);

    // Validate this is a FinalizationRegistry and pre-compute its weak ref
    let this_weak = this_obj.downgrade();

    // CanBeHeldWeakly(target): Objects and non-registered Symbols
    let target_weak = if let Some(target_obj) = target_val.as_object() {
        WeakRef::Object(target_obj.downgrade())
    } else if can_be_held_weakly(target_val) {
        WeakRef::Symbol(target_val)
    } else {
        return Err(ctx.throw_type_error(
            "FinalizationRegistry.prototype.register: target must be an object",
        ));
    };

    // SameValue(target, heldValue) check
    let same_value = match &target_weak {
        WeakRef::Object(_) => {
            target_val.is_object()
                && held_val.is_object()
                && target_val.as_object() == held_val.as_object()
        }
        WeakRef::Symbol(sym) => held_val.is_symbol() && sym == &held_val,
    };
    if same_value {
        return Err(ctx.throw_type_error(
            "FinalizationRegistry.prototype.register: held value cannot be the target",
        ));
    }

    // CanBeHeldWeakly(unregisterToken): undefined, Object, or non-registered Symbol
    let token_weak = if token_val.is_undefined() {
        None
    } else if let Some(tok_obj) = token_val.as_object() {
        Some(WeakRef::Object(tok_obj.downgrade()))
    } else if can_be_held_weakly(token_val) {
        Some(WeakRef::Symbol(token_val))
    } else {
        return Err(ctx.throw_type_error(
            "FinalizationRegistry.prototype.register: unregisterToken must be an object or undefined",
        ));
    };

    let entry = FinRecEntry {
        target: target_weak,
        held_value: held_val,
        token: token_weak,
    };

    // Validate this is a FinalizationRegistry and push entry
    {
        let mut this_obj = this_obj;
        let frd = match this_obj.payload_mut().finalization_registry_mut() {
            Some(d) => d,
            None => {
                return Err(ctx.throw_type_error(
                    "FinalizationRegistry.prototype.register called on non-FinalizationRegistry",
                ));
            }
        };
        frd.entries.push(entry);
    }

    // Add back-reference from target to this FinalizationRegistry (objects only)
    if let Some(mut target_obj) = target_val.as_object() {
        target_obj.add_back_ref(this_weak);
    }

    Ok(JSValue::UNDEFINED)
}

fn js_finrec_unregister(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(mut this_obj) = cc.this().as_object() else {
        return Err(ctx.throw_type_error(
            "FinalizationRegistry.prototype.unregister requires a FinalizationRegistry",
        ));
    };

    let frd = match this_obj.payload_mut().finalization_registry_mut() {
        Some(d) => d,
        None => {
            return Err(ctx.throw_type_error(
                "FinalizationRegistry.prototype.unregister called on non-FinalizationRegistry",
            ));
        }
    };

    let token_val = cc.arg(0);
    // CanBeHeldWeakly check: Object or non-registered Symbol accepted.
    // Unlike register(), undefined is not accepted here.
    if !can_be_held_weakly(token_val) {
        return Err(ctx.throw_type_error(
            "FinalizationRegistry.prototype.unregister: unregister token must be an object",
        ));
    }

    let heap = ctx.rt().gc_heap();
    let before = frd.entries.len();

    frd.entries.retain(|e| match &e.token {
        Some(tok_weak) => !tok_weak.matches(&token_val, heap),
        None => true,
    });

    let removed = before != frd.entries.len();
    Ok(JSValue::from(removed))
}

static FINREC_PROTO_FUNCS: &[FieldDef] = &[
    FieldDef::FUNC("register", 2, js_finrec_register, 0),
    FieldDef::FUNC("unregister", 1, js_finrec_unregister, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "FinalizationRegistry",
        JS_PROP_CONFIGURABLE,
    ),
];

pub(crate) static CLASS_DEF_FINALIZATION_REGISTRY: ClassDef = ClassDef::builtin(
    Atom::CONST_FinalizationRegistry,
    FINREC_PROTO_FUNCS,
    Some(ClassCtorDef::new(js_finrec_constructor, 1, false).expose_to_global()),
);
