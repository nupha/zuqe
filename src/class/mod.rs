mod class_def;
mod meta;
mod user;

pub use class_def::*;

pub(crate) use {meta::*, user::*};

use std::ffi::c_void;

use crate::{
    atom::Atom,
    context::JSContext,
    error::JSResult,
    object::{JSObject, JSPropertyDescriptor, ObjectPayload, PropertyKey},
    runtime::JSRuntime,
    value::JSValue,
};

pub type ClassID = u16;

pub type ClassCallFn = for<'a> fn(
    ctx: &JSContext<'a>,
    func_obj: &JSValue<'a>,
    this_val: &JSValue<'a>,
    args: &[JSValue<'a>],
    flags: i32,
) -> JSResult<'a>;

/// initialize class proto, constructor to ctx.
/// returns the class proto object created for specified ctx.
pub type ClassInitializeFn =
    for<'a> fn(&JSContext<'a>, ClassID) -> Result<Option<JSObject<'a>>, i32>;

/// class instance finalize callback. `isa` is the object's payload.
pub type ClassFinalizeFn = fn(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize);

pub type ClassGCMarkFn = fn(val: JSValue);

pub type DefineOwnPropertyFn = for<'a> fn(
    ctx: &JSContext<'a>,
    target: &JSValue<'a>,
    prop: &Atom,
    val: JSValue<'a>,
    getter: JSValue<'a>,
    setter: JSValue<'a>,
    flags: i32,
) -> Result<bool, i32>;

pub type HasPropertyFn =
    for<'a> fn(ctx: &JSContext<'a>, target: &JSObject<'a>, prop: &Atom) -> Result<bool, i32>;

/// TODO: change target type to &JSObject
pub type GetOwnPropertyFn = for<'a> fn(
    ctx: &JSContext<'a>,
    target: JSValue<'a>,
    prop: &Atom,
) -> Result<Option<JSPropertyDescriptor<'a>>, i32>;

pub type GetOwnPropertyNamesFn =
    for<'a> fn(ctx: &JSContext<'a>, target: &JSObject<'a>) -> Result<Box<[PropertyKey]>, i32>;

pub type GetPropertyFn = for<'a> fn(
    ctx: &JSContext<'a>,
    target: JSValue<'a>,
    prop: &Atom,
    this_obj: JSValue<'a>,
) -> JSResult<'a>;

pub type SetPropertyFn = for<'a> fn(
    ctx: &JSContext<'a>,
    target: JSValue<'a>,
    prop: &Atom,
    val: JSValue<'a>,
    this_obj: JSValue<'a>,
    flags: i32,
) -> Result<bool, i32>;

/// TODO: change target type to &JSObject
pub type DeletePropertyFn =
    for<'a> fn(ctx: &JSContext<'a>, target: JSValue<'a>, prop: &Atom) -> Result<bool, i32>;

/// free user class instance handler
pub type FreeObjFn = fn(rt: &mut JSRuntime, opaque: *mut c_void, id: usize);

pub(crate) const OP_DEFINE_METHOD_METHOD: u8 = 0;
pub(crate) const OP_DEFINE_METHOD_GETTER: u8 = 1;
pub(crate) const OP_DEFINE_METHOD_SETTER: u8 = 2;
pub(crate) const OP_DEFINE_METHOD_ENUMERABLE: u8 = 4;
