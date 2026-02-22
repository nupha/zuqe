// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING,
    atom::Atom,
    context::ContextImpl,
    error::JSResult,
    object::Object,
    value::{HINT_FORCE_ORDINARY, HINT_NUMBER, HINT_STRING, JSValue},
};

impl From<bool> for JSValue {
    #[inline(always)]
    fn from(value: bool) -> Self {
        JSValue::Bool(value)
    }
}

impl Object {
    /// Coerce object to primitive (non-object) value
    pub fn to_primitive(&self, ctx: &ContextImpl, hint: i32) -> JSResult {
        let ignore_toPrimitive_method = hint & HINT_FORCE_ORDINARY != 0;
        let hint = hint & !HINT_FORCE_ORDINARY;

        let this: JSValue = (*self).into();

        if !ignore_toPrimitive_method {
            // try [Symbol.toPrimitive] method
            #[cfg(feature = "builtin_symbol")]
            {
                let method = self.get_property(ctx, Atom::CONST_Symbol_toPrimitive)?;

                /* ECMA says *If exoticToPrim is not undefined* but tests in
                test262 use null as a non-callable converter */
                if !method.is_undefined_or_null() {
                    let hint_val = match hint {
                        HINT_STRING => Atom::CONST_string,
                        HINT_NUMBER => Atom::CONST_number,
                        _ => Atom::CONST_default,
                    }
                    .force_string(ctx)?;

                    let ret = method.call(ctx, this, &[hint_val.into()])?;
                    return if !ret.is_object() {
                        Ok(ret)
                    } else {
                        ctx.throw_type_error("toPrimitive");
                        Err(-1)
                    };
                }
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
            if method.is_function() {
                let ret = method.call(ctx, this, &[])?;
                if !ret.is_object() {
                    return Ok(ret);
                }
            }
        }

        ctx.throw_type_error("cannot coerce to primitive");
        Err(-1)
    }

    /// Coerce object to number (Int or Float64)
    #[inline(always)]
    pub fn to_number(&self, ctx: &ContextImpl) -> JSResult {
        self.to_primitive(ctx, HINT_NUMBER)
    }
}

impl JSValue {
    /// Coerce value to primitive value. Primitive is non-object value.
    pub fn to_primitive(&self, ctx: &ContextImpl, hint: i32) -> Result<JSValue, i32> {
        if let Self::Object(o) = self {
            o.to_primitive(ctx, hint)
        } else {
            Ok(*self)
        }
    }

    /// coerce value to bool
    pub fn to_bool(&self) -> Result<bool, i32> {
        match self {
            JSValue::NULL | JSValue::UNDEFINED => Ok(false),
            JSValue::Int(i) => Ok(*i != 0),
            JSValue::Bool(b) => Ok(*b),
            JSValue::Object(o) => Ok(!o.flags().is_htmldda()),
            JSValue::String(s) => Ok(!s.is_empty()),
            JSValue::Float64(d) => Ok(!d.is_nan() && *d != 0.0),
            JSValue::EXCEPTION => Err(-1),
            _ => Ok(true),
        }
    }
}

/// For primitive types (number, bool, string, symbol), return prototype of associated type.
/// Only works for primitive types, otherwise returns Null.
pub fn JS_GetPrototypePrimitive(ctx: &ContextImpl, val: &JSValue) -> JSValue {
    match val {
        JSValue::Int(_) | JSValue::Float64(_) => ctx.class_proto(JS_CLASS_NUMBER),
        JSValue::Bool(_) => ctx.class_proto(JS_CLASS_BOOLEAN),
        JSValue::String(_) => ctx.class_proto(JS_CLASS_STRING),
        #[cfg(feature = "builtin_symbol")]
        JSValue::Symbol(_) => ctx.class_proto(crate::JS_CLASS_SYMBOL),
        _ => None,
    }
    .map_or(JSValue::NULL, JSValue::Object)
}
