// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_def;
mod native;
mod using;

use gc_lite::GcTraceCtx;

pub use {
    class_def::{ClassCtorDef, ClassDef},
    native::*,
};

pub(crate) use {class_def::*, using::*};

use std::ffi::c_void;

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    object::{JSPropertyDescriptor, Object, ObjectPayload, PropertyKey},
    runtime::RuntimeImpl,
    value::JSValue,
};

pub type ClassID = u16;

/// Class instance finalize callback, called from `ObjectImpl::drop()` when a class instance is garbage collected.
pub type ClassFinalizeFn = fn(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize);
/// free user class instance handler
pub(crate) type ClassFinalizeOpaqueFn = fn(
    rt: &mut RuntimeImpl,
    payload_opaque: *mut c_void,
    class_id: usize,
    id: usize,
);
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
