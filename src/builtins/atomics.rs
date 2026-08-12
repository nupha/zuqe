// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JSResult,
    context::ScopedContext,
    function::{CallContext, FieldDef},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
};

/// Message thrown by every `Atomics` method until the real implementation lands.
const ATOMICS_UNSUPPORTED: &str = "Atomic is not supported at the moment";

#[cold]
fn unsupported(ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    Err(ctx.throw_type_error(ATOMICS_UNSUPPORTED))
}

pub const js_atomics_funcs: &[FieldDef] = &[
    FieldDef::FUNC("load", 2, unsupported, 0),
    FieldDef::FUNC("store", 3, unsupported, 0),
    FieldDef::FUNC("exchange", 3, unsupported, 0),
    FieldDef::FUNC("compareExchange", 4, unsupported, 0),
    FieldDef::FUNC("add", 3, unsupported, 0),
    FieldDef::FUNC("sub", 3, unsupported, 0),
    FieldDef::FUNC("and", 3, unsupported, 0),
    FieldDef::FUNC("or", 3, unsupported, 0),
    FieldDef::FUNC("xor", 3, unsupported, 0),
    FieldDef::FUNC("notify", 3, unsupported, 0),
    FieldDef::FUNC("wait", 4, unsupported, 0),
    FieldDef::FUNC("waitAsync", 4, unsupported, 0),
    FieldDef::FUNC("isLockFree", 1, unsupported, 0),
];

/// Lazy-initialized `Atomics` namespace object, registered as a global.
pub const js_atomics_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "Atomics",
    js_atomics_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
