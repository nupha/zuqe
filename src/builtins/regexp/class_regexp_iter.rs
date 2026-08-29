// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_REGEXP_STRING_ITERATOR, ScopedContext,
    atom::Atom,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    object::JS_PROP_CONFIGURABLE,
    value::JSValue,
};

use super::{RegExpStringIterPayload, get_last_index, regexp_exec};

fn js_regexp_string_iterator_next(
    ctx: &mut ScopedContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let this = cc.this();
    let o = this
        .as_object()
        .filter(|o| o.class_id() == JS_CLASS_REGEXP_STRING_ITERATOR)
        .ok_or_else(|| ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP_STRING_ITERATOR))?;
    let it: &RegExpStringIterPayload = o
        .payload_regexp_string_iterator()
        .ok_or_else(|| ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP_STRING_ITERATOR))?;

    if it.done() {
        *state = IteratorState::Done;
        return Ok(JSValue::UNDEFINED);
    }

    // initialize state
    *state = IteratorState::NotDone;

    if let Some(mat) = regexp_exec(ctx, it.regexp().into(), it.string())? {
        if it.is_global() {
            let str = mat
                .get_property_i64(ctx, 0)
                .and_then(|x| x.to_string(ctx))?;

            if str.is_empty() {
                // if matched is empty, advance index to next char
                let last_index = get_last_index(ctx, it.regexp().into()).inspect_err(|_| {
                    *state = IteratorState::NotDone;
                })?;

                let next_index = it
                    .string()
                    .index_of_next_i64(last_index, it.is_unicode())
                    .unwrap_or(last_index + 1);

                it.regexp()
                    .set_property(ctx, Atom::CONST_lastIndex, JSValue::from(next_index))
                    .inspect_err(|_| {
                        *state = IteratorState::NotDone;
                    })?;
            }
        } else {
            it.set_done(true);
        }

        Ok(mat.into())
    } else {
        it.set_done(true);
        *state = IteratorState::Done;
        Ok(JSValue::UNDEFINED)
    }
}

pub const js_regexp_string_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 0, js_regexp_string_iterator_next, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "RegExp String Iterator",
        JS_PROP_CONFIGURABLE,
    ),
];
