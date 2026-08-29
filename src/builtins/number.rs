// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{borrow::Cow, ffi::c_void};

use crate::{
    ClassDef, JS_CLASS_NUMBER,
    builtins::global::{MAGIC_NUM_IS_FINITE, MAGIC_NUM_IS_NAN, js_global_isNaN_isFinite},
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    number::{
        ATOD_ACCEPT_PREFIX_AFTER_SIGN, ATOD_INT_ONLY, JS_DTOA_FIXED_FORMAT, JS_DTOA_FORCE_EXP,
        JS_DTOA_FRAC_FORMAT, JS_DTOA_VAR_FORMAT, NumValue, f64_stringify, is_safe_integer, js_aton,
    },
    utils::unlikely,
    value::JSValue,
};

pub fn js_number_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = if cc.length() == 0 {
        JSValue::from(0)
    } else {
        // ToNumeric: `Number(1n)` converts the BigInt to a Number (with
        // precision loss), it does not throw.
        match cc.arg(0).to_numeric(ctx)? {
            NumValue::Float(f) => JSValue::from(f),
            #[cfg(feature = "big_int")]
            NumValue::BigInt(b) => JSValue::from(crate::bigint::to_f64(&b)),
        }
    };

    let new_target = cc.this();
    if !new_target.is_undefined() {
        ctx.ordinary_create_from_constructor_class(
            new_target,
            JS_CLASS_NUMBER,
            Box::into_raw(Box::new(val)) as *mut c_void,
        )
        .map(JSValue::from)
        .map_err(|_| -1)
    } else {
        Ok(val)
    }
}

fn js_thisNumberValue(ctx: &mut ScopedContext, this_val: JSValue) -> JSResult {
    if this_val.is_number() {
        Ok(this_val)
    } else {
        // check number object
        let val = this_val
            .as_object()
            .and_then(|p| p.payload_value().cloned());

        val.filter(|v| v.is_number())
            .ok_or_else(|| ctx.throw_type_error("not a number"))
    }
}

pub(super) fn js_parseInt(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let radix = cc.arg(1).to_number(ctx)?.to_i32();

    if radix != 0 && !(2..=36).contains(&radix) {
        Ok(JSValue::NAN)
    } else {
        let str = cc.arg(0).to_string(ctx)?;
        let s = str.content().encode_utf8(false);
        let str = s.trim();

        if unlikely(str.is_empty()) {
            Ok(JSValue::NAN)
        } else {
            let val: JSValue = js_aton(
                ctx,
                str.as_bytes(),
                radix,
                ATOD_INT_ONLY | ATOD_ACCEPT_PREFIX_AFTER_SIGN,
            )
            .map_or_else(
                |_| {
                    // fail to parse number, returns NaN
                    ctx.clear_exception();
                    JSValue::NAN
                },
                |(n, _)| match n {
                    NumValue::Float(v) => JSValue::from(v),
                    // parseInt never passes ATOD_ACCEPT_SUFFIX, so a BigInt can
                    // never be returned; keep the value-preserving conversion.
                    #[cfg(feature = "big_int")]
                    NumValue::BigInt(b) => JSValue::make_bigint(b),
                },
            );

            Ok(val)
        }
    }
}

pub(super) fn js_parseFloat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let str = cc.arg(0).to_string(ctx)?;
    let s = str.content().encode_utf8(false);
    let str = s.trim();

    if unlikely(str.is_empty()) {
        Ok(JSValue::NAN)
    } else {
        let val = js_aton(ctx, str.as_bytes(), 10, 0).map_or_else(
            |_| {
                // fail to parse number, returns NaN
                ctx.clear_exception();
                JSValue::NAN
            },
            |(n, _)| match n {
                NumValue::Float(v) => JSValue::from(v),
                // parseFloat never passes ATOD_ACCEPT_SUFFIX; keep the
                // value-preserving conversion for exhaustiveness.
                #[cfg(feature = "big_int")]
                NumValue::BigInt(b) => JSValue::make_bigint(b),
            },
        );
        Ok(val)
    }
}

fn js_number_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    js_thisNumberValue(ctx, cc.this())
}

fn js_number_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let to_locale_string = cc.magic() != 0;
    let val = js_thisNumberValue(ctx, cc.this())?;
    let d: f64 = val.to_number(ctx)?.to_f64();

    let radix = if to_locale_string || cc.arg(0).is_undefined() {
        10
    } else {
        let r = cc.arg(0).to_number(ctx)?.to_i32_saturating();
        if !(2..=36).contains(&r) {
            return Err(ctx.throw_range_error("radix must be between 2 and 36"));
        }
        r
    };

    let s = f64_stringify(d, radix as _, None, JS_DTOA_VAR_FORMAT);
    ctx.create_string_from_utf8(s.into_owned().into_bytes())
        .map(JSValue::make_string)
}

fn js_number_toPrecision(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let d = js_thisNumberValue(ctx, cc.this())?.to_number(ctx)?.to_f64();

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
        let f = a0.to_number(ctx)?.to_i32_saturating();

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
            return Err(ctx.throw_range_error("invalid number of digits"));
        } else {
            f64_stringify(d, 10, Some(f as u32), JS_DTOA_FIXED_FORMAT)
        }
    };

    ctx.create_string_from_str(&s).map(JSValue::make_string)
}

fn js_number_toExponential(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = js_thisNumberValue(ctx, cc.this())?;
    let d: f64 = val.to_number(ctx)?.to_f64();

    let a0 = cc.arg(0);
    let f = a0.to_number(ctx)?.to_i32_saturating();

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
        return Err(ctx.throw_range_error("invalid number of digits"));
    } else {
        f64_stringify(
            d,
            10,
            Some(f as u32),
            JS_DTOA_FIXED_FORMAT | JS_DTOA_FORCE_EXP,
        )
    };

    ctx.create_string_from_str(&s).map(JSValue::make_string)
}

fn js_number_toFixed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let d = js_thisNumberValue(ctx, cc.this())?.to_number(ctx)?.to_f64();
    let f = cc.arg(0).to_number(ctx)?.to_i32_saturating();

    let s = if unlikely(!(0..=100).contains(&f)) {
        return Err(ctx.throw_range_error("invalid number of digits"));
    } else if unlikely(d.abs() >= 1e21) {
        // ES spec step 9: when x >= 10^21, toFixed returns ToString(x),
        // which uses variable-format exponential notation (positive
        // exponents carry a '+' sign, e.g. "1e+21"). Delegating to
        // f64_stringify keeps it consistent with String(x).
        f64_stringify(d, 10, None, JS_DTOA_VAR_FORMAT).into_owned()
    } else {
        f64_stringify(d, 10, Some(f as u32), JS_DTOA_FRAC_FORMAT).into_owned()
    };

    ctx.create_string_from_utf8(s.into_bytes())
        .map(JSValue::make_string)
}

#[cfg(feature = "intl402")]
fn js_number_toLocaleString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = js_thisNumberValue(ctx, cc.this())?;
    let data =
        crate::builtins::intl::number_format::build_number_format_data(ctx, cc.arg(0), cc.arg(1))?;
    let s = crate::builtins::intl::number_format::format_value(ctx, &data, val)?;
    ctx.create_string_from_utf8(s.into_bytes())
        .map(JSValue::make_string)
}

pub const js_number_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toFixed", 1, js_number_toFixed, 0),
    FieldDef::FUNC("toPrecision", 1, js_number_toPrecision, 0),
    FieldDef::FUNC("toExponential", 1, js_number_toExponential, 0),
    FieldDef::FUNC("toString", 1, js_number_toString, 0),
    FieldDef::FUNC("valueOf", 0, js_number_valueOf, 0),
    #[cfg(feature = "intl402")]
    FieldDef::FUNC("toLocaleString", 0, js_number_toLocaleString, 0),
];

fn js_number_isNaN_isFinite(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if !cc.arg(0).is_number() {
        Ok(JSValue::FALSE)
    } else {
        js_global_isNaN_isFinite(ctx, cc)
    }
}

fn js_number_isInteger(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let b = cc.arg(0).is_integral(ctx)?;
    Ok(b.into())
}

fn js_number_isSafeInteger(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    if !val.is_number() {
        return Ok(JSValue::FALSE);
    }

    let d: f64 = val.to_number(ctx)?.to_f64();
    Ok(is_safe_integer(d).into())
}

pub const js_number_funcs: &[FieldDef] = &[
    // global ParseInt and parseFloat should be defined already or delayed
    FieldDef::ALIAS_BASE("parseInt", "parseInt", 0),
    FieldDef::ALIAS_BASE("parseFloat", "parseFloat", 0),
    FieldDef::FUNC("isNaN", 1, js_number_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    FieldDef::FUNC(
        "isFinite",
        1,
        js_number_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _,
    ),
    FieldDef::FUNC("isInteger", 1, js_number_isInteger, 0),
    FieldDef::FUNC("isSafeInteger", 1, js_number_isSafeInteger, 0),
    FieldDef::FLOAT64("MAX_VALUE", 1.797_693_134_862_315_7e308, 0),
    FieldDef::FLOAT64("MIN_VALUE", 5e-324, 0),
    FieldDef::FLOAT64("NaN", f64::NAN, 0),
    FieldDef::FLOAT64("NEGATIVE_INFINITY", -f64::INFINITY, 0),
    FieldDef::FLOAT64("POSITIVE_INFINITY", f64::INFINITY, 0),
    FieldDef::FLOAT64("EPSILON", 2.220446049250313e-16, 0), // ES6
    FieldDef::FLOAT64("MAX_SAFE_INTEGER", 9007199254740991.0, 0), // ES6
    FieldDef::FLOAT64("MIN_SAFE_INTEGER", -9007199254740991.0, 0), // ES6
];

pub(crate) static CLASS_DEF_NUMBER: ClassDef = ClassDef::builtin(
    crate::Atom::CONST_Number,
    js_number_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_number_constructor, 1, true)
            .entries(js_number_funcs)
            .expose_to_global(),
    ),
);
