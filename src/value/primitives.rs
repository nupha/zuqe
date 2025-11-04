use crate::{
    JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING, JS_CLASS_SYMBOL,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    object::{JSObject, ObjectPayload},
    runtime::JSRuntime,
    value::{HINT_FORCE_ORDINARY, HINT_NUMBER, HINT_STRING, JSValue},
};

impl<'a> From<bool> for JSValue<'a> {
    #[inline(always)]
    fn from(value: bool) -> Self {
        JSValue::Bool(value)
    }
}

impl<'a> JSObject<'a> {
    /// Coerce object to primitive (non-object) value
    pub fn to_primitive(&self, ctx: &JSContext<'a>, hint: i32) -> JSResult<'a> {
        let ignore_toPrimitive_method = hint & HINT_FORCE_ORDINARY != 0;
        let hint = hint & !HINT_FORCE_ORDINARY;

        let this: JSValue = self.clone().into();

        if !ignore_toPrimitive_method {
            // try .toPrimitive() method
            let method = self.get_property(ctx, &JS_ATOM_Symbol_toPrimitive)?;

            /* ECMA says *If exoticToPrim is not undefined* but tests in
            test262 use null as a non-callable converter */
            if !method.is_undefined_or_null() {
                let hint_val = match hint {
                    HINT_STRING => JS_ATOM_string,
                    HINT_NUMBER => JS_ATOM_number,
                    _ => JS_ATOM_default,
                }
                .force_string(ctx)?;

                let ret = method.call(ctx, &this, &[hint_val.into()])?;
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
            [JS_ATOM_toString, JS_ATOM_valueOf]
        } else {
            [JS_ATOM_valueOf, JS_ATOM_toString]
        } {
            let method = self.get_property(ctx, &prop)?;
            if method.is_function() {
                let ret = method.call(ctx, &this, &[])?;
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
    pub fn to_number(&self, ctx: &JSContext<'a>) -> JSResult<'a> {
        self.to_primitive(ctx, HINT_NUMBER)
    }
}

impl<'a> JSValue<'a> {
    /// Coerce value to primitive (non-object) value
    #[inline]
    pub fn to_primitive(&self, ctx: &JSContext<'a>, hint: i32) -> Result<JSValue<'a>, i32> {
        if let Self::Object(o) = self {
            o.to_primitive(ctx, hint)
        } else {
            Ok(self.clone())
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
pub fn JS_GetPrototypePrimitive<'a>(ctx: &JSContext<'a>, val: &JSValue<'a>) -> JSValue<'a> {
    match val {
        JSValue::Int(_) | JSValue::Float64(_) => ctx.class_proto(JS_CLASS_NUMBER),
        JSValue::Bool(_) => ctx.class_proto(JS_CLASS_BOOLEAN),
        JSValue::String(_) => ctx.class_proto(JS_CLASS_STRING),
        JSValue::Symbol(_) => ctx.class_proto(JS_CLASS_SYMBOL),
        _ => None,
    }
    .map_or(JSValue::NULL, JSValue::Object)
}

pub const fn js_object_data_finalizer(_rt: &mut JSRuntime, _isa: &ObjectPayload, _id: usize) {
    // free object data
    // JS_FreeValueRT(
    //     rt,
    //     val.object_unchecked()
    //         .isa_mut()
    //         .value_replace(rt.as_mut().unwrap(), JSValue::UNDEFINED)
    //         .unwrap(),
    // );
}

pub fn js_object_data_mark(val: JSValue) {
    if let Some(o) = val.as_object() {
        o.payload().value().unwrap().gc_mark();
    }
}
