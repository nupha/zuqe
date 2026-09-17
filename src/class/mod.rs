// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_def;
mod native;
mod using;

pub(crate) use class_def::*;
pub use {
    class_def::{ClassCtorDef, ClassDef},
    native::*,
};

use std::{
    ffi::c_void,
    sync::{
        OnceLock,
        atomic::{AtomicU16, Ordering},
    },
};

use gc_lite::GcTraceCtx;

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    object::{JSPropertyDescriptor, Object, PropertyKey},
    runtime::RuntimeImpl,
    value::JSValue,
};

pub type ClassID = u16;

// ── Host class definition (collected via linkme) ──

/// A host class entry collected from `def_native_object!(..)` in external crates.
/// `class_id` is 0 until the process-global init assigns the real id.
pub struct HostClassDef {
    pub def: ClassDef,
    pub class_id: AtomicU16,

    /// Resolves `ClassDef::payload_type_name` at runtime (the linked type name
    /// can't be computed in a const/static initializer). Called once during
    /// `init_static_class_defs`.
    #[cfg(debug_assertions)]
    pub payload_type_name: fn() -> Option<&'static str>,
}

unsafe impl Sync for HostClassDef {}

/// Distributed slice collecting all host `ClassDef`s at link time.
#[linkme::distributed_slice]
pub static HOST_CLASS_DEFS: [HostClassDef] = [..];

// ── Process-level static class tables ──

/// Process-global class table: builtin + host entries.
/// Initialized once by `init_static_class_defs()`, never mutated after.
static STATIC_CLASS_DEFS: OnceLock<Box<[ClassDef]>> = OnceLock::new();

/// Number of static entries (builtin + host). Dynamic class IDs start here.
pub(crate) fn static_class_count() -> usize {
    STATIC_CLASS_DEFS.get().map_or(0, |t| t.len())
}

/// Number of static classes (builtin + host). Dynamic classes live per-runtime.
pub(crate) fn class_count() -> usize {
    static_class_count()
}

/// Look up a static `ClassDef` by class ID.
#[inline]
pub(crate) fn get_static_class_def(cid: ClassID) -> Option<&'static ClassDef> {
    if cid == 0 {
        return None;
    }
    STATIC_CLASS_DEFS.get().and_then(|t| t.get(cid as usize))
}

/// Initialize the process-global static class table from builtin + host definitions.
/// Must be called exactly once during engine init (before any Runtime creation).
/// Returns the start of the dynamic class ID range.
pub(crate) fn init_static_class_defs() -> Result<ClassID, i32> {
    if STATIC_CLASS_DEFS.get().is_some() {
        // Already initialized by an earlier Runtime in this process.
        return Ok(static_class_count() as ClassID);
    }

    let builtin: &[ClassDef] = crate::BUILTIN_CLASS_DEFS;

    // Collect host entries, dropping linkme ICF duplicates: identical `static`
    // defs fold to a single address under ICF, so one class can appear twice.
    // Dedup by address — two distinct classes (even with the same name) always
    // keep separate addresses, so name-based dedup would wrongly drop one.
    let mut host: Vec<&HostClassDef> = Vec::new();
    for h in HOST_CLASS_DEFS {
        if !host.iter().any(|p| std::ptr::addr_eq(*p, h)) {
            host.push(h);
        }
    }

    let total = 1 + builtin.len() + host.len();
    let mut merged: Vec<ClassDef> = Vec::with_capacity(total);

    // Slot 0 is the null class; builtins occupy ids 1..=builtin.len().
    merged.push(ClassDef::default());
    merged.extend_from_slice(builtin);

    // Mint host const atoms so the class table can store name_atom ids.
    let name_atoms = crate::atom::init_host_const_atoms(host.iter().map(|h| h.def.name));

    // Host entries occupy ids builtin.len()+1 .. ; their class_id equals the
    // merged-table index. name_atom is resolved from the freshly-minted host
    // const atoms.
    for (i, h) in host.iter().enumerate() {
        let id = (1 + builtin.len() + i) as ClassID;
        h.class_id.store(id, Ordering::Relaxed);
        let mut def = h.def;
        def.name_atom = match name_atoms.get(def.name) {
            Some(&id) => Atom::from_id(id),
            _ => unreachable!("host const atom missing"),
        };

        #[cfg(debug_assertions)]
        {
            def.payload_type_name = (h.payload_type_name)();
        }

        merged.push(def);
    }

    if let Err(prev) = STATIC_CLASS_DEFS.set(merged.into_boxed_slice()) {
        // Raced with another thread's init; the table is build-determined,
        // so `prev` holds the same entries. Either outcome is valid.
        let _ = prev.len();
    }

    Ok(static_class_count() as ClassID)
}

/// Class instance finalize callback, called from `ObjectImpl::finalize()` when a
/// class instance is garbage collected. Unified for both builtin and user classes.
pub type ClassFinalizeFn =
    fn(rt: &mut RuntimeImpl, payload_ptr: *mut c_void, class_id: usize, id: usize);
/// class unregister handler.
///
/// # Note
/// When this callback is invoked, the runtime's `class_registry` has already been cleared,
/// so implementations **must not** index into `rt.class_registry` to reference other classes.
pub type ClassUnregisterFn = fn(rt: &mut RuntimeImpl);
/// Initialize a class to context `ctx`, including setup proto, constructor.
pub type ClassInitializeFn = fn(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32>;
/// Callback when a class instance is invoked as a function (`myInstance(args)`) or
/// as a constructor (`new myInstance(args)`). The `flags` parameter carries
/// `JS_CALL_FLAG_CONSTRUCTOR` to distinguish the latter case.
pub type ClassCallFn = fn(
    ctx: &mut ScopedContext,
    func_obj: JSValue,
    this_val: JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult;
/// GC marker for non-builtin class instance's opaque payload.
/// Called during GC mark phase to trace gc-nodes held inside opaque.
pub(crate) type ClassGcTraceFn =
    fn(payload_opaque: *mut c_void, gcx: &mut GcTraceCtx, class_id: usize);

pub type DefineOwnPropertyFn = fn(
    ctx: &mut ScopedContext,
    target: JSValue,
    prop: Atom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32>;

pub type HasPropertyFn =
    fn(ctx: &mut ScopedContext, target: Object, prop: Atom) -> Result<bool, i32>;

/// TODO: change target type to Object
pub type GetOwnPropertyFn = fn(
    ctx: &mut ScopedContext,
    target: JSValue,
    prop: Atom,
) -> Result<Option<JSPropertyDescriptor>, i32>;

pub type GetOwnPropertyNamesFn =
    fn(ctx: &mut ScopedContext, target: Object) -> Result<Box<[PropertyKey]>, i32>;

pub type GetPropertyFn =
    fn(ctx: &mut ScopedContext, target: JSValue, prop: Atom, this_obj: JSValue) -> JSResult;

pub type SetPropertyFn = fn(
    ctx: &mut ScopedContext,
    target: JSValue,
    prop: Atom,
    val: JSValue,
    this_obj: JSValue,
    flags: i32,
) -> Result<bool, i32>;

/// TODO: change target type to Object
pub type DeletePropertyFn =
    fn(ctx: &mut ScopedContext, target: JSValue, prop: Atom) -> Result<bool, i32>;
pub type GetPrototypeFn = fn(ctx: &mut ScopedContext, obj: JSValue) -> JSResult;
pub type SetPrototypeFn =
    fn(ctx: &mut ScopedContext, obj: JSValue, proto_val: JSValue) -> Result<bool, i32>;

pub(crate) const OP_DEFINE_METHOD_METHOD: u8 = 0;
pub(crate) const OP_DEFINE_METHOD_GETTER: u8 = 1;
pub(crate) const OP_DEFINE_METHOD_SETTER: u8 = 2;
pub(crate) const OP_DEFINE_METHOD_ENUMERABLE: u8 = 4;
