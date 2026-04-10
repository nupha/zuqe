// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_def;
mod r#use;

pub use class_def::ClassDef;

pub(crate) use {class_def::*, r#use::*};

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

pub type ClassCallFn = fn(
    ctx: &mut ScopedContext,
    func_obj: JSValue,
    this_val: JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult;

/// Initialize class proto, constructor to context.
/// Returns the class proto object created for given `ctx`.
pub type ClassInitializeFn =
    fn(ctx: &mut ScopedContext, class_id: ClassID) -> Result<Option<Object>, i32>;
/// class instance finalize callback.
pub type ClassFinalizeFn = fn(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize);
/// free user class instance handler
pub type ClassFinalizeOpaqueFn = fn(rt: &mut RuntimeImpl, opaque: *mut c_void, id: usize);

/// class unregister handler.
pub type ClassUnregisterFn = fn();

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
