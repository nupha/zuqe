mod class_def;
mod meta;
mod user;

pub use class_def::*;

pub(crate) use {meta::*, user::*};

use std::ffi::c_void;

use crate::{
    atom::Atom,
    context::{ContextHandle, ContextRef},
    error::JSResult,
    object::{JSPropertyDescriptor, ObjectPayload, ObjectRef, PropertyKey},
    runtime::RuntimeImpl,
    value::JSValue,
};

pub type ClassID = u16;

pub type ClassCallFn = fn(
    ctx: ContextRef,
    func_obj: JSValue,
    this_val: JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult;

/// initialize class proto, constructor to ctx.
/// returns the class proto object created for specified ctx.
pub type ClassInitializeFn =
    fn(ctx: ContextHandle, class_id: ClassID) -> Result<Option<ObjectRef>, i32>;

/// class instance finalize callback.
pub type ClassFinalizeFn = fn(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize);

pub type DefineOwnPropertyFn = fn(
    ctx: ContextRef,
    target: JSValue,
    prop: Atom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32>;

pub type HasPropertyFn = fn(ctx: ContextRef, target: ObjectRef, prop: Atom) -> Result<bool, i32>;

/// TODO: change target type to ObjectRef
pub type GetOwnPropertyFn =
    fn(ctx: ContextRef, target: JSValue, prop: Atom) -> Result<Option<JSPropertyDescriptor>, i32>;

pub type GetOwnPropertyNamesFn =
    fn(ctx: ContextRef, target: ObjectRef) -> Result<Box<[PropertyKey]>, i32>;

pub type GetPropertyFn =
    fn(ctx: ContextRef, target: JSValue, prop: Atom, this_obj: JSValue) -> JSResult;

pub type SetPropertyFn = fn(
    ctx: ContextRef,
    target: JSValue,
    prop: Atom,
    val: JSValue,
    this_obj: JSValue,
    flags: i32,
) -> Result<bool, i32>;

/// TODO: change target type to ObjectRef
pub type DeletePropertyFn = fn(ctx: ContextRef, target: JSValue, prop: Atom) -> Result<bool, i32>;

/// free user class instance handler
pub type FreeObjFn = fn(rt: &mut RuntimeImpl, opaque: *mut c_void, id: usize);

pub(crate) const OP_DEFINE_METHOD_METHOD: u8 = 0;
pub(crate) const OP_DEFINE_METHOD_GETTER: u8 = 1;
pub(crate) const OP_DEFINE_METHOD_SETTER: u8 = 2;
pub(crate) const OP_DEFINE_METHOD_ENUMERABLE: u8 = 4;
