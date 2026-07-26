// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    number::IntOrFloat,
    object::{Object, ObjectImpl},
    value::{HINT_FORCE_ORDINARY, HINT_NUMBER, HINT_STRING, JSValue},
};

impl ObjectImpl {
    /// Coerce object to primitive (non-object) value
    pub fn to_primitive(&self, ctx: &mut ScopedContext, hint: i32) -> JSResult {
        let ignore_toPrimitive_method = hint & HINT_FORCE_ORDINARY != 0;
        let hint = hint & !HINT_FORCE_ORDINARY;

        let this = JSValue::from(Object::from_ref(self));

        if !ignore_toPrimitive_method {
            // try [Symbol.toPrimitive] method
            let method = self.get_property(ctx, Atom::CONST_Symbol_toPrimitive)?;

            // ECMA says *If exoticToPrim is not undefined* but tests in
            // test262 use null as a non-callable converter
            if !method.is_undefined_or_null() {
                let hint_val = match hint {
                    HINT_STRING => Atom::CONST_string,
                    HINT_NUMBER => Atom::CONST_number,
                    _ => Atom::CONST_default,
                }
                .force_string(ctx)?;

                let ret = method.call(ctx, this, &[JSValue::make_string(hint_val)])?;
                return if !ret.is_object() {
                    Ok(ret)
                } else {
                    ctx.throw_type_error("toPrimitive");
                    Err(-1)
                };
            }
        }

        // Call method when hint is:
        // - HINT_STRING: try toString() first, then valueOf()
        // - HINT_NUMBER/HINT_NONE: try valueOf() first, then toString()
        for prop in if hint == HINT_STRING {
            [Atom::CONST_toString, Atom::CONST_valueOf]
        } else {
            [Atom::CONST_valueOf, Atom::CONST_toString]
        } {
            let method = self.get_property(ctx, prop)?;

            if method.is_callable() {
                let ret = method.call(ctx, this, &[])?;
                if !ret.is_object() {
                    return Ok(ret);
                }
            }
        }

        ctx.throw_type_error("cannot coerce to primitive");
        Err(-1)
    }

    /// Coerce object to number
    pub fn to_number(&self, ctx: &mut ScopedContext) -> Result<IntOrFloat, i32> {
        self.to_primitive(ctx, HINT_NUMBER)
            .and_then(|v| v.to_number(ctx))
    }
}

impl JSValue {
    /// Coerce value to primitive value. Primitive is non-object value.
    pub fn to_primitive(&self, ctx: &mut ScopedContext, hint: i32) -> Result<JSValue, i32> {
        self.as_object()
            .map_or(Ok(*self), |o| o.to_primitive(ctx, hint))
    }
}
