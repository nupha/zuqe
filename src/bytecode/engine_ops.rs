// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Semantics of the opcodes the *engine* emits rather than user JavaScript,
//! shared by the bytecode interpreter (`exec_stackless`) and the JIT helpers
//! (`jit/abi.rs`).
//!
//! Same split as `class_ops`/`ref_ops`/`with_ops`: one implementation, two
//! engines, so the error messages and the allocation paths cannot drift. The
//! operand-stack movement stays in the caller — each function below states the
//! opcode's window and the net `sp` delta the caller applies.
//!
//! * `throw` / `throw_error` — raising (todo 2.6, the last opcodes that used
//!   to exit the native frame on every execution).
//! * `regexp` — build the `RegExp` object for a pattern the parser already
//!   precompiled.
//! * `push_bigint_i32` — reconstruct an `i32`-range BigInt literal.

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::{
        JS_THROW_ERROR_DELETE_SUPER, JS_THROW_ERROR_ITERATOR_THROW, JS_THROW_VAR_REDECL,
        JS_THROW_VAR_RO, JS_THROW_VAR_UNINITIALIZED, JS_ThrowReferenceErrorUninitialized,
        JS_ThrowSyntaxErrorVarRedeclaration,
    },
    value::JSValue,
};

/// `throw_error <atom> <type>`: raise the engine-generated error `type_` names.
/// `0 -> 0`; the caller exits the frame because this always raises.
pub(crate) fn throw_error(ctx: &mut ScopedContext, atom: Atom, type_: u8) {
    match type_ {
        JS_THROW_VAR_RO => {
            let _ = ctx.throw_type_error_read_only(atom);
        }
        JS_THROW_VAR_REDECL => {
            let _ = JS_ThrowSyntaxErrorVarRedeclaration(ctx, atom);
        }
        JS_THROW_VAR_UNINITIALIZED => {
            let _ = JS_ThrowReferenceErrorUninitialized(ctx, atom);
        }
        JS_THROW_ERROR_DELETE_SUPER => {
            let _ = ctx.throw_reference_error("unsupported reference to 'super'");
        }
        JS_THROW_ERROR_ITERATOR_THROW => {
            let _ = ctx.throw_type_error("iterator does not have a throw method");
        }
        _ => {
            let _ = ctx.throw_internal_error(&format!("invalid throw var type {type_}"));
        }
    }
}

/// `regexp`: build the `RegExp` object for precompiled pattern `re_id` (the
/// parser put it in `rt().regexp_precompiled` and pushed its index).
///
/// The caller reads the index from the top of the stack, replaces the slot
/// below it with the result and drops one (`[id, ^] -> [regexp, ^]`).
pub(crate) fn make_regexp(ctx: &mut ScopedContext, re_id: i32) -> Result<JSValue, i32> {
    #[cfg(feature = "builtin_regexp")]
    {
        // The interpreter indexes the table directly; an out-of-range id can
        // only come from corrupt bytecode, so this bounds check turns a panic
        // into the engine's own internal error without changing any valid
        // bytecode's behavior.
        let Some(re) = ctx.rt().regexp_precompiled.get(re_id as usize).cloned() else {
            return Err(ctx.throw_internal_error("invalid precompiled regexp id"));
        };
        crate::builtins::regexp::RegExp::from(ctx, JSValue::UNDEFINED, re).map(Into::into)
    }
    #[cfg(not(feature = "builtin_regexp"))]
    {
        let _ = re_id;
        Err(ctx.throw_internal_error("regex not supported"))
    }
}

/// `push_bigint_i32 <value>`: the BigInt for an `i32`-range literal, rebuilt
/// from the immediate (no constant-pool lookup, no parser-side allocation).
/// The caller pushes the result (`0 -> 1`).
pub(crate) fn bigint_from_i32(ctx: &mut ScopedContext, value: i32) -> Result<JSValue, i32> {
    #[cfg(feature = "big_int")]
    {
        crate::bigint::BigIntRef::create(ctx, value < 0, vec![value.unsigned_abs() as u64])
            .map(JSValue::make_bigint)
    }
    #[cfg(not(feature = "big_int"))]
    {
        let _ = value;
        // `big_int` disabled: the emitter never produces this opcode, so
        // reaching it is a bug (the interpreter's arm says the same).
        unreachable!("PUSH_BIGINT_I32 not supported")
    }
}
