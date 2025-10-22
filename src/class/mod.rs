mod class_def;

pub use class_def::*;

use crate::{
    atom::Atom,
    context::JSContext,
    error::JSResult,
    object::{JSObject, JSPropertyDescriptor, ObjectPayload, PropertyKey},
    runtime::JSRuntime,
    value::JSValue,
};

pub type JSClassID = u32;

pub type ClassCallFn = fn(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_val: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult;

/// class instance finalize callback. `isa` is the object's payload.
pub type ClassFinalizeFn = fn(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize);

pub type ClassGCMarkFn = fn(val: JSValue);

pub type DefineOwnPropertyFn = fn(
    ctx: &JSContext,
    target: &JSValue,
    prop: &Atom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32>;

pub type HasPropertyFn = fn(ctx: &JSContext, target: &JSObject, prop: &Atom) -> Result<bool, i32>;

/// TODO: change target type to &JSObject
pub type GetOwnPropertyFn =
    fn(ctx: &JSContext, target: JSValue, prop: &Atom) -> Result<Option<JSPropertyDescriptor>, i32>;

pub type GetOwnPropertyNamesFn =
    fn(ctx: &JSContext, target: &JSObject) -> Result<Box<[PropertyKey]>, i32>;

pub type GetPropertyFn =
    fn(ctx: &JSContext, target: JSValue, prop: &Atom, this_obj: JSValue) -> JSResult;

pub type SetPropertyFn = fn(
    ctx: &JSContext,
    target: JSValue,
    prop: &Atom,
    val: JSValue,
    this_obj: JSValue,
    flags: i32,
) -> Result<bool, i32>;

/// TODO: change target type to &JSObject
pub type DeletePropertyFn = fn(ctx: &JSContext, target: JSValue, prop: &Atom) -> Result<bool, i32>;
