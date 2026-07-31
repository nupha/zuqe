// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Small helpers shared across the builtin implementations.

use crate::{
    builtins::js_get_this,
    context::ScopedContext,
    function::{CallContext, FieldDef},
};

/// The standard `[Symbol.species]` getter entry (returns `this`), shared by
/// every constructor function table exposing the species pattern.
pub(crate) const SPECIES_FIELD: FieldDef =
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0);

/// Optional start/end style index argument: `undefined` (or absent) yields
/// `default`, otherwise `ToNumber` + clamp into `[0, len]` with negative
/// values offset by `len` (the spec's relative-index conversion).
pub(crate) fn arg_rel_index(
    ctx: &mut ScopedContext,
    cc: &CallContext,
    nth: usize,
    len: i64,
    default: i64,
) -> Result<i64, i32> {
    let v = cc.arg(nth);
    if v.is_undefined() {
        Ok(default)
    } else {
        Ok(v.to_number(ctx)?.to_i64_clamp(0, len, len))
    }
}

/// `i32` flavor of [arg_rel_index] for the string / typed-array methods that
/// operate on 32-bit lengths.
pub(crate) fn arg_rel_index32(
    ctx: &mut ScopedContext,
    cc: &CallContext,
    nth: usize,
    len: i32,
    default: i32,
) -> Result<i32, i32> {
    let v = cc.arg(nth);
    if v.is_undefined() {
        Ok(default)
    } else {
        Ok(v.to_number(ctx)?.to_i32_clamp(0, len, len))
    }
}
