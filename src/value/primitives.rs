use crate::{
    JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING, JS_CLASS_SYMBOL,
    atom::consts::*,
    context::JSContext,
    function::JS_Call,
    object::ObjectDetail,
    runtime::JSRuntime,
    value::{HINT_FORCE_ORDINARY, HINT_NUMBER, HINT_STRING, JSValue},
};

impl From<bool> for JSValue {
    #[inline]
    fn from(value: bool) -> Self {
        JSValue::Bool(value)
    }
}

impl JSValue {
    /// Convert value to primitive (non-object) value
    pub fn coerce_to_primitive(&self, ctx: &JSContext, hint: i32) -> Result<JSValue, i32> {
        if let Self::Object(o) = self {
            let ignore_toPrimitive_method = hint & HINT_FORCE_ORDINARY != 0;
            let hint = hint & !HINT_FORCE_ORDINARY;

            if !ignore_toPrimitive_method {
                // try .toPrimitive() method
                let method = o
                    .get_property(ctx, JS_ATOM_Symbol_toPrimitive)
                    .check_exception()?;

                /* ECMA says *If exoticToPrim is not undefined* but tests in
                test262 use null as a non-callable converter */
                if !method.is_undefined_or_null() {
                    let hint_val = match hint {
                        HINT_STRING => JS_ATOM_string,
                        HINT_NUMBER => JS_ATOM_number,
                        _ => JS_ATOM_default,
                    }
                    .to_value(ctx, true)
                    .check_exception()?;

                    let ret = JS_Call(ctx, &method, self, &[hint_val]).check_exception()?;
                    return if !ret.is_object() {
                        Ok(ret)
                    } else {
                        ctx.JS_ThrowTypeError("toPrimitive");
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
                let method = o.get_property(ctx, prop).check_exception()?;
                if method.is_callable() {
                    let ret = JS_Call(ctx, &method, self, &[]).check_exception()?;
                    if !ret.is_object() {
                        return Ok(ret);
                    }
                }
            }

            ctx.JS_ThrowTypeError("cannot coerce to primitive");
            Err(-1)
        } else {
            Ok(self.clone())
        }
    }

    pub fn coerce_to_bool(&self, ctx: &JSContext) -> Result<bool, i32> {
        match self {
            JSValue::NULL | JSValue::UNDEFINED => Ok(false),
            JSValue::Int(i) => Ok(*i != 0),
            JSValue::Bool(b) => Ok(*b),
            JSValue::Object(o) => Ok(!o.flags().is_htmldda()),
            JSValue::String(s) => Ok(!s.is_empty()),
            JSValue::Float64(d) => Ok(!(*d).is_nan() && *d != 0.0),
            JSValue::EXCEPTION => Err(-1),
            _ => Ok(true),
        }
    }
}

/// Only works for primitive types, otherwise return JS_NULL.
pub unsafe fn JS_GetPrototypePrimitive(ctx: &JSContext, val: &JSValue) -> JSValue {
    ctx.with_mut(|cx| match val {
        JSValue::Int(_) | JSValue::Float64(_) => cx.class_proto[JS_CLASS_NUMBER as usize].clone(),
        JSValue::Bool(_) => cx.class_proto[JS_CLASS_BOOLEAN as usize].clone(),
        JSValue::String(_) => cx.class_proto[JS_CLASS_STRING as usize].clone(),
        JSValue::Symbol(_) => cx.class_proto[JS_CLASS_SYMBOL as usize].clone(),
        _ => JSValue::NULL,
    })
}

pub const fn js_object_data_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {
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
    let v = unsafe {
        val.object_unchecked()
            .0
            .detail
            .borrow()
            .value()
            .unwrap()
            .clone()
    };

    v.gc_mark();
}
