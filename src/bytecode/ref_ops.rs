// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Ref-cell opcode semantics, shared by the bytecode interpreter
//! (`exec_stackless`) and the JIT helpers (`jit/abi.rs`).
//!
//! A "ref cell" is a plain object carrying one `JS_PROP_VARREF` property whose
//! value is a [`VarRef`] onto a local/argument/closure slot (or a global var
//! object/property pair). Destructuring assignment, `for (a of xs)` /
//! `for (a in src)` targets and `with` bindings all read/write through one.
//!
//! Each function is the interpreter arm's work verbatim; the operand-stack
//! movement stays in the caller (the interpreter updates `cur_sp_offset`, the
//! JIT rewrites the frame's `sp`). Every doc comment states the opcode's stack
//! window and the net `sp` delta the caller applies — including on the error
//! path, which mirrors `SP_GO!` running before the `is_err` check.

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::JS_ThrowReferenceErrorNotDefined,
    function::VarRef,
    object::{JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty, ThrowPolicy},
    value::JSValue,
    vm_stack::{FrameIndex, VmStack},
};

/// Which slot space [`make_ref`]'s index names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefKind {
    Loc,
    Arg,
    /// A closure variable, resolved from the current function's captured refs.
    VarRefRef,
}

/// `make_loc_ref` / `make_arg_ref` / `make_var_ref_ref`: create a ref object
/// holding a `VarRef` onto slot `idx`, then push the object and its property
/// name. `0 -> 2`, no `sp` move on the error path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_ref(
    ctx: &mut ScopedContext,
    kind: RefKind,
    atom: Atom,
    idx: u16,
    arg_base_ptr: *mut JSValue,
    var_base_ptr: *mut JSValue,
    arena: &mut VmStack,
    cur_frame_idx: FrameIndex,
    arg_count: u16,
    var_count: u16,
    captured_vars: &mut [VarRef],
) -> Result<(JSValue, JSValue), i32> {
    let var_ref = match kind {
        RefKind::VarRefRef => captured_vars[idx as usize],
        RefKind::Loc | RefKind::Arg => super::exec_stackless::get_var_ref(
            ctx,
            idx,
            kind == RefKind::Arg,
            arg_base_ptr,
            var_base_ptr,
            arena,
            cur_frame_idx,
            arg_count,
            var_count,
        )?,
    };
    let mut obj = ctx.create_base_object_with_proto(None)?;
    obj.add_own_property(
        ctx,
        atom,
        JS_PROP_WRITABLE | JS_PROP_VARREF,
        JSProperty::VarRef(var_ref),
    )?;
    Ok((JSValue::from(obj), atom.to_value(ctx)?))
}

/// `make_var_ref`: resolve a global binding to `(base, prop)`. `0 -> 2`.
pub(crate) fn make_global_ref(
    ctx: &mut ScopedContext,
    atom: Atom,
) -> Result<(JSValue, JSValue), i32> {
    match ctx.build_global_varref(atom)? {
        Some(pair) => Ok(pair),
        None => Err(0),
    }
}

/// `get_ref_value`: `[obj, prop] -> [obj, value]` (`+1`). Raises the
/// not-defined `ReferenceError` itself (undefined base, or a strict
/// missing-property read); `HasProperty` runs before `Get` so a with-object
/// proxy sees the ECMA-262 9.1.1.2.6 trap order.
pub(crate) fn get_ref_value(
    ctx: &mut ScopedContext,
    obj: JSValue,
    prop: JSValue,
) -> Result<JSValue, i32> {
    if obj.is_undefined() {
        if let Ok(atom) = prop.to_atom(ctx) {
            let _ = JS_ThrowReferenceErrorNotDefined(ctx, atom);
        }
        return Err(0);
    }
    let obj_has_prop = prop
        .to_atom(ctx)
        .and_then(|atom| obj.has_property(ctx, atom));
    if let Ok(false) = obj_has_prop {
        if ctx.rt().is_strict_mode() {
            if let Ok(atom) = prop.to_atom(ctx) {
                let _ = JS_ThrowReferenceErrorNotDefined(ctx, atom);
            }
            return Err(0);
        }
        return Ok(JSValue::UNDEFINED);
    }
    if obj_has_prop.is_ok() {
        return obj.get_property_value(ctx, prop);
    }
    Err(0)
}

/// `put_ref_value`: `[obj, prop, val] -> []` (`-3`). Strict writes to an
/// undefined base or a missing property raise the not-defined
/// `ReferenceError`; `HasProperty` runs before `Set` (ECMA-262 9.1.1.2.5).
pub(crate) fn put_ref_value(
    ctx: &mut ScopedContext,
    mut obj: JSValue,
    prop: JSValue,
    val: JSValue,
) -> Result<(), i32> {
    let strict = ctx.rt().is_strict_mode();
    let mut no_add = false;
    if obj.is_undefined() {
        if strict {
            if let Ok(atom) = prop.to_atom(ctx) {
                let _ = JS_ThrowReferenceErrorNotDefined(ctx, atom);
            }
            return Err(0);
        }
        obj = JSValue::from(ctx.global_obj());
    } else if strict {
        no_add = true;
    }
    let Ok(atom) = prop.to_atom(ctx) else {
        return Err(0);
    };
    match obj.has_property(ctx, atom) {
        Ok(false) if strict => {
            let _ = JS_ThrowReferenceErrorNotDefined(ctx, atom);
            return Err(0);
        }
        Ok(_) => {}
        // A throwing `has` trap must propagate as-is.
        Err(_) => return Err(0),
    }
    obj.set_property_value(ctx, prop, val, ThrowPolicy::StrictOnly, no_add)
        .map(|_| ())
}
