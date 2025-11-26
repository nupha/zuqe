use std::borrow::Cow;

use crate::{
    JS_ALIAS_BASE_DEF, JS_CFUNC_NATIVE_DEF, JS_PROP_DOUBLE_DEF,
    branch_hints::unlikely,
    builtins::global::{MAGIC_NUM_IS_FINITE, MAGIC_NUM_IS_NAN, js_global_isNaN_isFinite},
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef},
    number::{
        ATOD_ACCEPT_PREFIX_AFTER_SIGN, ATOD_INT_ONLY, JS_DTOA_FIXED_FORMAT, JS_DTOA_FORCE_EXP,
        JS_DTOA_FRAC_FORMAT, JS_DTOA_VAR_FORMAT, f64_stringify, is_safe_integer, js_atof,
    },
    object::ObjectPayload,
    value::JSValue,
};

pub fn js_thisNumberValue(ctx: &ContextImpl, this_val: JSValue) -> JSResult {
    if this_val.is_number() {
        Ok(this_val)
    } else {
        // check number object
        this_val
            .as_object()
            .and_then(|p| p.payload().value())
            .filter(|v| v.is_number())
            .cloned()
            .ok_or_else(|| {
                ctx.throw_type_error("not a number");
                -1
            })
    }
}

pub fn js_parseInt(ctx: ContextRef, cc: CallContext) -> JSResult {
    let radix: i32 = cc.arg(1).to_i32(&ctx)?;

    if radix != 0 && !(2..=36).contains(&radix) {
        Ok(JSValue::Float64(f64::NAN))
    } else {
        let str = cc.arg(0).to_string(&ctx)?;
        let s = str.content().to_utf8(false);
        let str = s.trim();

        if unlikely(str.is_empty()) {
            Ok(JSValue::Float64(f64::NAN))
        } else {
            unsafe {
                let val = js_atof(
                    &ctx,
                    str.as_ptr(),
                    std::ptr::null_mut(),
                    radix,
                    ATOD_INT_ONLY | ATOD_ACCEPT_PREFIX_AFTER_SIGN,
                )
                .map_or_else(
                    |_| {
                        // fail to parse number, returns NaN
                        ctx.rt().take_exception();
                        JSValue::Float64(f64::NAN)
                    },
                    JSValue::from,
                );
                Ok(val)
            }
        }
    }
}

pub fn js_parseFloat(ctx: ContextRef, args: CallContext) -> JSResult {
    let str = args.arg(0).to_string(&ctx)?;
    let s = str.content().to_utf8(false);
    let str = s.trim();

    if unlikely(str.is_empty()) {
        Ok(JSValue::Float64(f64::NAN))
    } else {
        unsafe {
            let val = js_atof(&ctx, str.as_ptr(), std::ptr::null_mut(), 10, 0).map_or_else(
                |_| {
                    // fail to parse number, returns NaN
                    ctx.rt().take_exception();
                    JSValue::Float64(f64::NAN)
                },
                JSValue::from,
            );
            Ok(val)
        }
    }
}

pub fn js_number_valueOf(ctx: ContextRef, cc: CallContext) -> JSResult {
    js_thisNumberValue(&ctx, *cc.this())
}

pub fn js_number_toString(ctx: ContextRef, cc: CallContext) -> JSResult {
    let to_locale_string = cc.magic() != 0;
    let val = js_thisNumberValue(&ctx, *cc.this())?;
    let d: f64 = val.to_f64(&ctx)?;

    let radix = if to_locale_string || cc.arg(0).is_undefined() {
        10
    } else {
        let r = cc.arg(0).to_i32_saturating(&ctx)?;
        if !(2..=36).contains(&r) {
            ctx.throw_range_error("radix must be between 2 and 36");
            return Err(-1);
        }
        r
    };

    let s = f64_stringify(d, radix as _, None, JS_DTOA_VAR_FORMAT);
    ctx.create_string_from_utf8(s.into_owned().into_bytes())
        .map(JSValue::String)
}

pub fn js_number_toPrecision(ctx: ContextRef, cc: CallContext) -> JSResult {
    let d = js_thisNumberValue(&ctx, *cc.this())?.to_f64(&ctx)?;

    let a0 = cc.arg(0);
    let s: Cow<str> = if a0.is_undefined() {
        if unlikely(!d.is_finite()) {
            if d.is_nan() {
                "NaN"
            } else if d.is_sign_positive() {
                "Infinity"
            } else {
                "-Infinity"
            }
            .into()
        } else {
            format!("{d}").into()
        }
    } else {
        let f = a0.to_i32_saturating(&ctx)?;

        if unlikely(!d.is_finite()) {
            if d.is_nan() {
                "NaN"
            } else if d.is_sign_positive() {
                "Infinity"
            } else {
                "-Infinity"
            }
            .into()
        } else if unlikely(!(1..=100).contains(&f)) {
            ctx.throw_range_error("invalid number of digits");
            return Err(-1);
        } else {
            f64_stringify(d, 10, Some(f as u32), JS_DTOA_FIXED_FORMAT)
        }
    };

    ctx.create_string_from_str(&s).map(JSValue::String)
}

pub fn js_number_toExponential(ctx: ContextRef, cc: CallContext) -> JSResult {
    let val = js_thisNumberValue(&ctx, *cc.this())?;
    let d: f64 = val.to_f64(&ctx)?;

    let a0 = cc.arg(0);
    let f = a0.to_i32_saturating(&ctx)?;

    let s = if unlikely(!d.is_finite()) {
        if d.is_nan() {
            "NaN"
        } else if d.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into()
    } else if unlikely(a0.is_undefined()) {
        f64_stringify(d, 10, None, JS_DTOA_FORCE_EXP)
    } else if unlikely(!(0..=100).contains(&f)) {
        ctx.throw_range_error("invalid number of digits");
        return Err(-1);
    } else {
        f64_stringify(
            d,
            10,
            Some(f as u32),
            JS_DTOA_FIXED_FORMAT | JS_DTOA_FORCE_EXP,
        )
    };

    ctx.create_string_from_str(&s).map(JSValue::String)
}

pub fn js_number_toFixed(ctx: ContextRef, cc: CallContext) -> JSResult {
    let d = js_thisNumberValue(&ctx, *cc.this())?.to_f64(&ctx)?;
    let f = cc.arg(0).to_i32_saturating(&ctx)?;

    let s = if unlikely(!(0..=100).contains(&f)) {
        ctx.throw_range_error("invalid number of digits");
        return Err(-1);
    } else if unlikely(d.abs() >= 1e21) {
        format!("{d:e}")
    } else {
        f64_stringify(d, 10, Some(f as u32), JS_DTOA_FRAC_FORMAT).into_owned()
    };

    ctx.create_string_from_utf8(s.into_bytes())
        .map(JSValue::String)
}

pub static js_number_proto_funcs: [FieldDef; 6] = [
    JS_CFUNC_NATIVE_DEF!("toFixed", 1, js_number_toFixed, 0),
    JS_CFUNC_NATIVE_DEF!("toPrecision", 1, js_number_toPrecision, 0),
    JS_CFUNC_NATIVE_DEF!("toExponential", 1, js_number_toExponential, 0),
    JS_CFUNC_NATIVE_DEF!("toString", 1, js_number_toString, 0),
    JS_CFUNC_NATIVE_DEF!("toLocaleString", 0, js_number_toString, 1),
    JS_CFUNC_NATIVE_DEF!("valueOf", 0, js_number_valueOf, 0),
];

pub fn js_number_isNaN_isFinite(ctx: ContextRef, args: CallContext) -> JSResult {
    if !args.arg(0).is_number() {
        Ok(JSValue::Bool(false))
    } else {
        js_global_isNaN_isFinite(ctx, args)
    }
}

pub fn js_number_isInteger(ctx: ContextRef, args: CallContext) -> JSResult {
    let b = args.arg(0).is_integral(&ctx)?;
    Ok(b.into())
}

pub fn js_number_isSafeInteger(ctx: ContextRef, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    if !val.is_number() {
        return Ok(JSValue::Bool(false));
    }

    let d: f64 = val.to_f64(&ctx)?;
    Ok(is_safe_integer(d).into())
}

pub static js_number_funcs: [FieldDef; 14] = [
    // global ParseInt and parseFloat should be defined already or delayed
    JS_ALIAS_BASE_DEF!("parseInt", "parseInt", 0),
    JS_ALIAS_BASE_DEF!("parseFloat", "parseFloat", 0),
    JS_CFUNC_NATIVE_DEF!("isNaN", 1, js_number_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    JS_CFUNC_NATIVE_DEF!(
        "isFinite",
        1,
        js_number_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _
    ),
    JS_CFUNC_NATIVE_DEF!("isInteger", 1, js_number_isInteger, 0),
    JS_CFUNC_NATIVE_DEF!("isSafeInteger", 1, js_number_isSafeInteger, 0),
    JS_PROP_DOUBLE_DEF!("MAX_VALUE", 1.797_693_134_862_315_7e308, 0),
    JS_PROP_DOUBLE_DEF!("MIN_VALUE", 5e-324, 0),
    JS_PROP_DOUBLE_DEF!("NaN", f64::NAN, 0),
    JS_PROP_DOUBLE_DEF!("NEGATIVE_INFINITY", -f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!("POSITIVE_INFINITY", f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!("EPSILON", 2.220446049250313e-16, 0), /* ES6 */
    JS_PROP_DOUBLE_DEF!("MAX_SAFE_INTEGER", 9007199254740991.0, 0), /* ES6 */
    JS_PROP_DOUBLE_DEF!("MIN_SAFE_INTEGER", -9007199254740991.0, 0), /* ES6 */
];

pub fn js_number_constructor(ctx: ContextRef, cc: CallContext) -> JSResult {
    let val = if cc.length() == 0 {
        JSValue::Int(0)
    } else {
        cc.arg(0).to_numeric(&ctx)?
    };

    let new_target = *cc.this();
    if !new_target.is_undefined() {
        ctx.create_object_from_ctor(new_target, ObjectPayload::NUMBER(val))
            .map(JSValue::Object)
            .map_err(|_| -1)
    } else {
        Ok(val)
    }
}
