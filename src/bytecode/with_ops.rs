// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright © 2025-2026 John Ray <996351336@qq.com>

//! `with` / variable-object opcode semantics, shared by the bytecode interpreter
//! (`exec_stackless`) and the JIT helpers (`jit/abi.rs`).
//!
//! The five `with_*` opcodes ask the environment object on top of the operand
//! stack whether it has the name (`HasProperty`, plus the `@@unscopables`
//! exclusion when the object came from a real `with`). If it does, the arm
//! performs the property operation and the bytecode jumps to its label,
//! skipping the ordinary variable access it guards; if not, the object is
//! dropped and execution falls through to that access. The lookup and the
//! property work are shared here; the operand-stack movement stays in the
//! caller (the interpreter updates `cur_sp_offset`, the JIT rewrites the
//! frame's `sp`). Each doc states the window and the delta the caller applies,
//! including on the error path (see `bytecode/ref_ops.rs` for the same shape).

use crate::{
    atom::Atom, context::ScopedContext, error::JS_ThrowReferenceErrorNotDefined,
    object::ThrowPolicy, value::JSValue,
};

use super::operators::js_has_unscopable;

/// Whether `atom` is visible on the environment object: `HasProperty`, then —
/// for a real `with` object — `@@unscopables` (ECMA-262 8.1.1.2.1 `HasBinding`).
/// `Ok(false)` means "not here": the caller drops the object and falls through.
/// `Err` is a pending exception from either lookup (a proxy trap may throw).
pub(crate) fn in_env(
    ctx: &mut ScopedContext,
    env_obj: JSValue,
    atom: Atom,
    is_with: bool,
) -> Result<bool, i32> {
    if !env_obj.has_property(ctx, atom)? {
        return Ok(false);
    }
    if !is_with {
        return Ok(true);
    }
    Ok(!js_has_unscopable(ctx, env_obj, atom)?)
}

/// `with_get_var`: the object's binding (`GetBindingValue`, ECMA-262 9.1.1.2.6).
/// `HasProperty` is re-checked because `@@unscopables` may have run user code
/// that deleted the key; a key that vanished is `undefined` in sloppy mode and
/// a not-defined `ReferenceError` in strict mode. A failed first `has_property`
/// is treated as "missing", exactly as the interpreter always did (the throw
/// below replaces the pending exception).
pub(crate) fn get_var(
    ctx: &mut ScopedContext,
    env_obj: JSValue,
    atom: Atom,
) -> Result<JSValue, i32> {
    if env_obj.has_property(ctx, atom).unwrap_or(false) {
        env_obj.get_property(ctx, atom)
    } else if ctx.rt().is_strict_mode() {
        let _ = JS_ThrowReferenceErrorNotDefined(ctx, atom);
        Err(0)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

/// `with_put_var`: `HasProperty` then `Set` with `ThrowPolicy::StrictOnly`
/// (ECMA-262 9.1.1.2.5 `SetMutableBinding`). The caller consumes the
/// `[value, env_obj]` window even when this fails — the interpreter moves `sp`
/// before it checks the result.
pub(crate) fn put_var(
    ctx: &mut ScopedContext,
    mut env_obj: JSValue,
    atom: Atom,
    val: JSValue,
) -> Result<(), i32> {
    match env_obj.has_property(ctx, atom) {
        Ok(false) | Err(_) if ctx.rt().is_strict_mode() => {
            let _ = JS_ThrowReferenceErrorNotDefined(ctx, atom);
            return Err(0);
        }
        _ => {}
    }
    let prop = atom.to_value(ctx).unwrap_or(JSValue::EXCEPTION);
    env_obj
        .set_property_value(ctx, prop, val, ThrowPolicy::StrictOnly, false)
        .map(|_| ())
}

/// `with_delete_var`: delete the binding and report whether it worked. The
/// caller replaces the object with the boolean.
pub(crate) fn delete_var(
    ctx: &mut ScopedContext,
    env_obj: &mut JSValue,
    atom: Atom,
) -> Result<bool, i32> {
    env_obj.delete_property(ctx, atom)
}

/// `with_make_ref`'s property name. The interpreter does not check this
/// conversion (it stacks `EXCEPTION` and carries on), so neither does the
/// caller — kept verbatim.
pub(crate) fn prop_name(ctx: &mut ScopedContext, atom: Atom) -> JSValue {
    atom.to_value(ctx).unwrap_or(JSValue::EXCEPTION)
}

/// `with_get_ref`: `GetBindingValue` for a call target (ECMA-262 9.1.1.2.6
/// step 2). A key that is gone, or a `Get` that fails, reads as `undefined` —
/// the interpreter does not propagate the error here either.
pub(crate) fn get_ref(ctx: &mut ScopedContext, env_obj: JSValue, atom: Atom) -> JSValue {
    match env_obj.has_property(ctx, atom) {
        Ok(false) | Err(_) => JSValue::UNDEFINED,
        _ => env_obj
            .get_property(ctx, atom)
            .unwrap_or(JSValue::UNDEFINED),
    }
}
