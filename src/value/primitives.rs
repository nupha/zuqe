use std::ops::DerefMut;

use crate::{
    atom::consts::*,
    context::JSContext,
    function::JS_CallFree,
    gc::GCMarkFunc,
    runtime::JSRuntime,
    value::{
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_FreeValueRT,
        HINT_FORCE_ORDINARY, HINT_NUMBER, HINT_STRING, JS_TAG_BIG_INT, JS_TAG_BOOL,
        JS_TAG_EXCEPTION, JS_TAG_INT, JS_TAG_IS_FLOAT64, JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_STRING,
        JS_TAG_UNDEFINED, JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG,
        JS_VALUE_GET_TAG,
    },
    JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING, JS_CLASS_SYMBOL, JS_MKVAL,
};

impl From<bool> for JSValue {
    #[inline]
    fn from(value: bool) -> Self {
        if value {
            JSValue::TRUE
        } else {
            JSValue::FALSE
        }
    }
}

impl JSValue {
    /// Coerce `self` value to  primitive value.
    /// value `self` is not consumed
    pub fn coerce_to_primitive(self, ctx: &mut JSContext, hint: i32) -> Result<JSValue, i32> {
        let force_ordinary = hint & HINT_FORCE_ORDINARY != 0;
        let mut hint = hint & !HINT_FORCE_ORDINARY;

        if !force_ordinary {
            let method = self
                .get_property(ctx, JS_ATOM_Symbol_toPrimitive)
                .check_exception()?;

            /* ECMA says *If exoticToPrim is not undefined* but tests in
            test262 use null as a non callable converter */
            if !method.is_undefined_or_null() {
                let mut hint_val = match hint {
                    HINT_STRING => JS_ATOM_string,
                    HINT_NUMBER => JS_ATOM_number,
                    _ => JS_ATOM_default,
                }
                .to_value(ctx, true)
                .check_exception()?
                .into_rc(ctx.rt());

                let ret = unsafe {
                    JS_CallFree(ctx, method, self, 1, hint_val.deref_mut()).check_exception()?
                };

                return if JS_VALUE_GET_TAG(ret) != JS_TAG_OBJECT {
                    Ok(ret)
                } else {
                    unsafe {
                        JS_FreeValue(ctx, ret);
                    }
                    ctx.JS_ThrowTypeError(c"toPrimitive");
                    Err(-1)
                };
            }
        }

        if JS_VALUE_GET_TAG(self) != JS_TAG_OBJECT {
            return Ok(self.dup());
        }

        if hint != HINT_STRING {
            hint = HINT_NUMBER;
        };

        for i in 0..2 {
            let method = self
                .get_property(
                    ctx,
                    if i != hint {
                        JS_ATOM_toString
                    } else {
                        JS_ATOM_valueOf
                    },
                )
                .check_exception()?;

            if method.is_callable(ctx) {
                let ret = unsafe { JS_CallFree(ctx, method, self, 0, std::ptr::null_mut()) }
                    .check_exception()?;
                if JS_VALUE_GET_TAG(ret) != JS_TAG_OBJECT {
                    return Ok(ret);
                } else {
                    unsafe {
                        JS_FreeValue(ctx, ret);
                    }
                }
            } else {
                unsafe {
                    JS_FreeValue(ctx, method);
                }
            }
        }

        ctx.JS_ThrowTypeError(c"toPrimitive");
        Err(-1)
    }
}

#[inline(always)]
pub const fn JS_NewCatchOffset(val: i32) -> JSValue {
    JS_MKVAL!(JSTag::CatchOffset, val)
}

/* Only works for primitive types, otherwise return JS_NULL. */
pub unsafe fn JS_GetPrototypePrimitive(ctx: *const JSContext, val: JSValueConst) -> JSValueConst {
    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::BigFloat | JSTag::BigDecimal | JSTag::BigInt => {
            unimplemented!()
        }
        JSTag::Int | JSTag::Float64 => (*ctx).class_proto[JS_CLASS_NUMBER as usize],
        JSTag::Bool => (*ctx).class_proto[JS_CLASS_BOOLEAN as usize],
        JSTag::String => (*ctx).class_proto[JS_CLASS_STRING as usize],
        JSTag::Symbol => (*ctx).class_proto[JS_CLASS_SYMBOL as usize],
        JSTag::Object | JSTag::Null | JSTag::Undefined => JSValue::NULL,
        _ => JSValue::NULL,
    }
}

/// returns Err if val is an exception.
/// `val` is consumed
pub unsafe fn JS_ToBoolFree(ctx: *mut JSContext, val: JSValue) -> Result<bool, i32> {
    match JS_VALUE_GET_TAG(val) {
        JS_TAG_NULL | JS_TAG_UNDEFINED => Ok(false),
        JS_TAG_INT | JS_TAG_BOOL => Ok(JS_VALUE_GET_INT(val) != 0),
        JS_TAG_EXCEPTION => Err(-1),
        JS_TAG_STRING => {
            let ret = !val.string().is_empty();
            JS_FreeValue(ctx, val);
            Ok(ret)
        }
        JS_TAG_BIG_INT => {
            // let p = JS_VALUE_GET_PTR(val) as *mut JSBigFloat;
            // let ret;
            // ret = (*p).num.expn != BF_EXP_ZERO && (*p).num.expn != BF_EXP_NAN;
            // JS_FreeValue(ctx, val);
            // ret as i32
            unimplemented!()
        }
        JS_TAG_OBJECT => {
            let ret = !val.object_unchecked().detailed.is_htmldda();
            JS_FreeValue(ctx, val);
            Ok(ret)
        }
        tag => {
            if JS_TAG_IS_FLOAT64(tag) {
                let d = JS_VALUE_GET_FLOAT64(val);
                Ok(!d.is_nan() && d != 0.0)
            } else {
                JS_FreeValue(ctx, val);
                Ok(true)
            }
        }
    }
}

pub unsafe fn JS_ToBool(ctx: *mut JSContext, val: JSValueConst) -> Result<bool, i32> {
    JS_ToBoolFree(ctx, JS_DupValue(ctx, val))
}

pub unsafe fn js_object_data_finalizer(rt: *mut JSRuntime, val: JSValue) {
    // free object data
    JS_FreeValueRT(
        rt,
        val.object_unchecked()
            .isa_mut()
            .value_replace(rt.as_mut().unwrap(), JSValue::UNDEFINED)
            .unwrap(),
    );
}

pub unsafe fn js_object_data_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
    (*rt).gc_mark_value(val.object_unchecked().isa().value().unwrap(), mark_func);
}
