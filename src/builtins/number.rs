use std::borrow::Cow;

use crate::{
    builtins::global::{MAGIC_NUM_IS_FINITE, MAGIC_NUM_IS_NAN, js_global_isNaN_isFinite},
    context::ContextImpl,
    error::JSResult,
    function::{CallContext, FieldDef},
    number::{
        ATOD_ACCEPT_PREFIX_AFTER_SIGN, ATOD_INT_ONLY, JS_DTOA_FIXED_FORMAT, JS_DTOA_FORCE_EXP,
        JS_DTOA_FRAC_FORMAT, JS_DTOA_VAR_FORMAT, f64_stringify, is_safe_integer, js_atof,
    },
    object::ObjectPayload,
    utils::unlikely,
    value::JSValue,
};

fn js_thisNumberValue(ctx: &ContextImpl, this_val: JSValue) -> JSResult {
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

pub(super) fn js_parseInt(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let radix: i32 = cc.arg(1).to_i32(ctx)?;

    if radix != 0 && !(2..=36).contains(&radix) {
        Ok(JSValue::Float64(f64::NAN))
    } else {
        let str = cc.arg(0).to_string(ctx)?;
        let s = str.content().to_utf8(false);
        let str = s.trim();

        if unlikely(str.is_empty()) {
            Ok(JSValue::Float64(f64::NAN))
        } else {
            unsafe {
                let val = js_atof(
                    ctx,
                    str.as_ptr(),
                    std::ptr::null_mut(),
                    radix,
                    ATOD_INT_ONLY | ATOD_ACCEPT_PREFIX_AFTER_SIGN,
                )
                .map_or_else(
                    |_| {
                        // fail to parse number, returns NaN
                        ctx.rt_ptr().take_exception();
                        JSValue::Float64(f64::NAN)
                    },
                    JSValue::from,
                );
                Ok(val)
            }
        }
    }
}

pub(super) fn js_parseFloat(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let str = cc.arg(0).to_string(ctx)?;
    let s = str.content().to_utf8(false);
    let str = s.trim();

    if unlikely(str.is_empty()) {
        Ok(JSValue::Float64(f64::NAN))
    } else {
        unsafe {
            let val = js_atof(ctx, str.as_ptr(), std::ptr::null_mut(), 10, 0).map_or_else(
                |_| {
                    // fail to parse number, returns NaN
                    ctx.rt_ptr().take_exception();
                    JSValue::Float64(f64::NAN)
                },
                JSValue::from,
            );
            Ok(val)
        }
    }
}

fn js_number_valueOf(cc: CallContext) -> JSResult {
    js_thisNumberValue(cc.ctx(), *cc.this())
}

fn js_number_toString(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let to_locale_string = cc.magic() != 0;
    let val = js_thisNumberValue(ctx, *cc.this())?;
    let d: f64 = val.to_f64(ctx)?;

    let radix = if to_locale_string || cc.arg(0).is_undefined() {
        10
    } else {
        let r = cc.arg(0).to_i32_saturating(ctx)?;
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

fn js_number_toPrecision(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let d = js_thisNumberValue(ctx, *cc.this())?.to_f64(ctx)?;

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
        let f = a0.to_i32_saturating(ctx)?;

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

fn js_number_toExponential(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let val = js_thisNumberValue(ctx, *cc.this())?;
    let d: f64 = val.to_f64(ctx)?;

    let a0 = cc.arg(0);
    let f = a0.to_i32_saturating(ctx)?;

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

fn js_number_toFixed(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let d = js_thisNumberValue(ctx, *cc.this())?.to_f64(ctx)?;
    let f = cc.arg(0).to_i32_saturating(ctx)?;

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
    FieldDef::CALL("toFixed", 1, js_number_toFixed, 0),
    FieldDef::CALL("toPrecision", 1, js_number_toPrecision, 0),
    FieldDef::CALL("toExponential", 1, js_number_toExponential, 0),
    FieldDef::CALL("toString", 1, js_number_toString, 0),
    FieldDef::CALL("toLocaleString", 0, js_number_toString, 1),
    FieldDef::CALL("valueOf", 0, js_number_valueOf, 0),
];

fn js_number_isNaN_isFinite(cc: CallContext) -> JSResult {
    if !cc.arg(0).is_number() {
        Ok(JSValue::Bool(false))
    } else {
        js_global_isNaN_isFinite(cc)
    }
}

fn js_number_isInteger(cc: CallContext) -> JSResult {
    let b = cc.arg(0).is_integral(cc.ctx())?;
    Ok(b.into())
}

fn js_number_isSafeInteger(cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    if !val.is_number() {
        return Ok(JSValue::Bool(false));
    }

    let d: f64 = val.to_f64(cc.ctx())?;
    Ok(is_safe_integer(d).into())
}

pub static js_number_funcs: [FieldDef; 14] = [
    // global ParseInt and parseFloat should be defined already or delayed
    FieldDef::ALIAS_BASE("parseInt", "parseInt", 0),
    FieldDef::ALIAS_BASE("parseFloat", "parseFloat", 0),
    FieldDef::CALL("isNaN", 1, js_number_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    FieldDef::CALL(
        "isFinite",
        1,
        js_number_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _,
    ),
    FieldDef::CALL("isInteger", 1, js_number_isInteger, 0),
    FieldDef::CALL("isSafeInteger", 1, js_number_isSafeInteger, 0),
    FieldDef::FLOAT64("MAX_VALUE", 1.797_693_134_862_315_7e308, 0),
    FieldDef::FLOAT64("MIN_VALUE", 5e-324, 0),
    FieldDef::FLOAT64("NaN", f64::NAN, 0),
    FieldDef::FLOAT64("NEGATIVE_INFINITY", -f64::INFINITY, 0),
    FieldDef::FLOAT64("POSITIVE_INFINITY", f64::INFINITY, 0),
    FieldDef::FLOAT64("EPSILON", 2.220446049250313e-16, 0), /* ES6 */
    FieldDef::FLOAT64("MAX_SAFE_INTEGER", 9007199254740991.0, 0), /* ES6 */
    FieldDef::FLOAT64("MIN_SAFE_INTEGER", -9007199254740991.0, 0), /* ES6 */
];

pub fn js_number_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let val = if cc.length() == 0 {
        JSValue::Int(0)
    } else {
        cc.arg(0).to_numeric(ctx)?
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
