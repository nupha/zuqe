// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Class / object-literal / private-field opcode semantics, shared by the
//! bytecode interpreter (`exec_stackless`) and the JIT helpers (`jit/abi.rs`).
//!
//! Each function is the interpreter arm's object work verbatim; the
//! operand-stack movement stays in the caller (the interpreter updates
//! `cur_sp_offset`, the JIT rewrites the frame's `sp`). Every doc comment
//! states the opcode's stack window and the net `sp` delta the caller applies —
//! including on the error path, which mirrors `SP_GO!` running before the
//! `is_err` check in the original arms.

use crate::{
    atom::{Atom, AtomKind},
    class::{
        OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD,
        OP_DEFINE_METHOD_SETTER,
    },
    context::ScopedContext,
    function::{BCWrapper, JS_CALL_FLAG_CONSTRUCTOR},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
        JS_PROP_HAS_WRITABLE, JS_PROP_THROW, JS_PROP_WRITABLE, Object, ThrowPolicy,
    },
    value::JSValue,
    vm_stack::{FrameIndex, VmStack},
};

use super::operators::js_method_set_properties;

/// `define_field`: `[this_obj, value] -> [this_obj]` (delta `-1`, always).
pub(crate) fn define_field(
    ctx: &mut ScopedContext,
    this_obj: &mut JSValue,
    atom: Atom,
    value: JSValue,
) -> Result<(), i32> {
    this_obj
        .define_property_value(ctx, atom, value, JS_PROP_C_W_E | JS_PROP_THROW)
        .map(|_| ())
}

/// `define_array_el`: `[obj, prop, value] -> [obj, prop]` (delta `-1`).
pub(crate) fn define_array_el(
    ctx: &mut ScopedContext,
    obj: &mut JSValue,
    prop: JSValue,
    value: JSValue,
) -> Result<(), i32> {
    let atom = prop.to_atom(ctx)?;
    obj.define_property_value(ctx, atom, value, JS_PROP_C_W_E | JS_PROP_THROW)
        .map(|_| ())
}

/// `set_proto`: `[obj, proto] -> [obj]`. Delta `-1`, except that the inner
/// error breaks before the drop — the caller must then keep `sp` too.
pub(crate) fn set_proto(
    ctx: &mut ScopedContext,
    obj: &mut JSValue,
    proto: JSValue,
) -> Result<(), i32> {
    if proto.is_object() || proto.is_null() {
        obj.set_prototype_internal(ctx, proto, true).map(|_| ())
    } else {
        Ok(())
    }
}

/// `set_home_object`: `[home_obj, func] -> [home_obj, func]` (delta `0`).
pub(crate) fn set_home_object(ctx: &ScopedContext, func: &mut JSValue, home_obj: JSValue) {
    if let Some(mut fo) = func.as_object_BCWrapper() {
        fo.method_set_home_object(ctx, home_obj.as_object());
    } else {
        #[cfg(debug_assertions)]
        unreachable!("SET_HOME_OBJECT expect BytecodeFunction object");
    }
}

/// `set_name_computed`: `[name, value] -> [name, value]` (delta `0`).
pub(crate) fn set_name_computed(
    ctx: &mut ScopedContext,
    name: JSValue,
    value: &mut JSValue,
) -> Result<(), i32> {
    value
        .as_object()
        .ok_or(-1)
        .and_then(|mut o| o.define_name_computed(ctx, name, JS_PROP_CONFIGURABLE))
        .map(|_| ())
}

/// `copy_data_properties`: three operands in place, delta `0`.
pub(crate) fn copy_data_properties(
    ctx: &mut ScopedContext,
    target: &mut JSValue,
    source: JSValue,
    exclude: JSValue,
) -> Result<(), i32> {
    target.copy_data_properties_from(ctx, &source, &exclude, false)
}

/// `define_method` / `define_method_computed`. Delta `-1` (normal) or `-2`
/// (computed), applied before the error is checked. `name` is `Atom::NULL` for
/// the computed form, whose key is `computed_name`.
pub(crate) fn define_method(
    ctx: &mut ScopedContext,
    this_obj: &mut JSValue,
    value: JSValue,
    name: Atom,
    computed_name: JSValue,
    op_flags: u8,
) -> Result<(), i32> {
    let name = if name.is_null() {
        computed_name.to_atom(ctx)?
    } else {
        name
    };

    let mut flags =
        JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE | JS_PROP_HAS_ENUMERABLE | JS_PROP_THROW;
    if op_flags & OP_DEFINE_METHOD_ENUMERABLE != 0 {
        flags |= JS_PROP_ENUMERABLE
    }

    let mut value = value;
    let mut getter = JSValue::UNDEFINED;
    let mut setter = JSValue::UNDEFINED;
    let func_obj: &mut JSValue;
    match op_flags & (OP_DEFINE_METHOD_GETTER | OP_DEFINE_METHOD_SETTER) {
        OP_DEFINE_METHOD_METHOD => {
            func_obj = &mut value;
            flags |= JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE;
        }
        OP_DEFINE_METHOD_GETTER => {
            getter = value;
            func_obj = &mut getter;
            flags |= JS_PROP_HAS_GET;
        }
        OP_DEFINE_METHOD_SETTER => {
            setter = value;
            func_obj = &mut setter;
            flags |= JS_PROP_HAS_SET;
        }
        _ => unreachable!(),
    }

    js_method_set_properties(ctx, func_obj, name, flags, this_obj).and_then(|_| {
        this_obj
            .define_property(ctx, name, value, getter, setter, flags)
            .map(|_| ())
    })
}

/// `check_ctor_return`: `[ret_val] -> [ret_val, flag]` (delta `+1` on success).
///
/// A derived constructor returning a non-object, non-undefined value throws in
/// the *caller's* realm, so the previous frame's realm is needed.
pub(crate) fn check_ctor_return(
    ctx: &mut ScopedContext,
    ret_val: JSValue,
    arena: *mut VmStack,
    frame_idx: FrameIndex,
) -> Result<bool, i32> {
    const MESSAGE: &str = "derived class constructor must return an object or undefined";
    if ret_val.is_object() {
        return Ok(false);
    }
    if !ret_val.is_undefined() {
        let prev = unsafe { (*arena).frame(frame_idx).prev };
        if !prev.is_null() {
            let prev_func = unsafe { (*arena).frame(prev).func_obj };
            if let Ok(bfun) = BCWrapper::try_from(JSValue::from(prev_func)) {
                let mut caller_ctx_ptr = bfun.payload().bytecode().realm();
                let mut caller_scope = caller_ctx_ptr.new_scope();
                let _ = caller_scope.throw_type_error(MESSAGE);
                return Err(-1);
            }
        }
        return Err(ctx.throw_type_error(MESSAGE));
    }
    Ok(true)
}

/// `check_ctor`: delta `0`.
pub(crate) fn check_ctor(ctx: &mut ScopedContext, new_target: JSValue) -> Result<(), i32> {
    if new_target.is_undefined() {
        Err(ctx.throw_type_error("class constructors must be invoked with 'new'"))
    } else {
        Ok(())
    }
}

/// `init_ctor`: calls the super constructor with the current arguments and
/// returns the `this` to push (delta `+1` on success).
pub(crate) fn init_ctor(
    ctx: &mut ScopedContext,
    func_obj: Object,
    new_target: JSValue,
    args: &[JSValue],
) -> Result<JSValue, i32> {
    if new_target.is_undefined() {
        return Err(ctx.throw_type_error("class constructors must be invoked with 'new'"));
    }
    let super_ctor = JSValue::from(func_obj).prototype(ctx)?;
    super_ctor.call_internal(
        ctx,
        JSValue::UNDEFINED,
        new_target,
        args,
        JS_CALL_FLAG_CONSTRUCTOR,
    )
}

/// `check_brand`: `[obj, brand] -> [obj, brand]` (delta `0`).
pub(crate) fn check_brand(
    ctx: &mut ScopedContext,
    obj: JSValue,
    brand: JSValue,
) -> Result<(), i32> {
    if !obj.check_brand(ctx, brand)? {
        let _ = ctx.throw_type_error("invalid brand on object");
        return Err(-1);
    }
    Ok(())
}

/// `add_brand`: `[func, home_obj] -> []` (delta `-2`, applied before the error
/// is checked).
pub(crate) fn add_brand(
    ctx: &mut ScopedContext,
    mut func_obj: JSValue,
    home_object: JSValue,
) -> Result<(), i32> {
    func_obj.add_brand(ctx, home_object)
}

/// `private_symbol`: `[] -> [symbol]` (delta `+1` on success).
pub(crate) fn private_symbol(ctx: &mut ScopedContext, atom: Atom) -> Result<JSValue, i32> {
    ctx.create_symbol_from_atom(atom, AtomKind::Private)
}

/// `get_private_field`: `[obj, name] -> [value]` (delta `-1`).
pub(crate) fn get_private_field(
    ctx: &mut ScopedContext,
    obj: JSValue,
    name: JSValue,
) -> Result<JSValue, i32> {
    obj.get_private_field(ctx, name)
}

/// `put_private_field`: `[obj, name, value] -> []` (delta `-3`, applied before
/// the error is checked).
pub(crate) fn put_private_field(
    ctx: &mut ScopedContext,
    obj: &mut JSValue,
    name: JSValue,
    value: JSValue,
) -> Result<(), i32> {
    obj.set_private_field(ctx, value, name)
}

/// `define_private_field`: `[obj, name, value] -> [obj]` (delta `-2`, applied
/// before the error is checked).
pub(crate) fn define_private_field(
    ctx: &mut ScopedContext,
    obj: &mut JSValue,
    name: JSValue,
    value: JSValue,
) -> Result<(), i32> {
    obj.define_private_field(ctx, name, value)
}

/// `get_super`: `[obj] -> [obj.prototype]` (delta `0`).
pub(crate) fn get_super(ctx: &mut ScopedContext, obj: JSValue) -> Result<JSValue, i32> {
    obj.prototype(ctx)
}

/// `get_super_value`: `[this, obj, prop] -> [value]` (delta `-2` on success).
pub(crate) fn get_super_value(
    ctx: &mut ScopedContext,
    this_obj: JSValue,
    obj: JSValue,
    prop: JSValue,
) -> Result<JSValue, i32> {
    let atom = prop.to_atom(ctx)?;
    obj.get_property_internal(ctx, atom, this_obj, false)
}

/// `put_super_value`: `[this, obj, prop, val] -> []` (delta `-4`, applied
/// before the error is checked).
pub(crate) fn put_super_value(
    ctx: &mut ScopedContext,
    this_obj: JSValue,
    obj: JSValue,
    prop: JSValue,
    val: JSValue,
) -> Result<(), i32> {
    let atom = prop.to_atom(ctx)?;
    if !obj.is_object() {
        return Err(ctx.throw_type_error_not_an_object());
    }
    debug_assert!(this_obj.is_object());
    obj.set_property_internal(
        ctx,
        atom,
        val,
        this_obj.as_object(),
        ThrowPolicy::StrictOnly,
        false,
    )
    .map(|_| ())
}
