use crate::{
    class::js_create_from_ctor,
    context::JSContext,
    error::{JSResult, JS_ThrowRangeError},
    function::{CallArgs, JSCFunctionListEntry},
    js_core::global::{js_global_isNaN_isFinite, MAGIC_NUM_IS_FINITE, MAGIC_NUM_IS_NAN},
    utils::skip_spaces,
    value::{
        is_safe_integer, js_dtoa, object::JS_SetObjectData, JSValue, JSValueConst, JS_IsException,
        JS_IsNumber, JS_IsUndefined, JS_ToFloat64, JS_ToInt32Sat, JS_ToNumeric, JS_ToStringFree,
        JS_DTOA_FIXED_FORMAT, JS_DTOA_FORCE_EXP, JS_DTOA_FRAC_FORMAT, JS_TAG_BIG_INT,
        JS_TAG_OBJECT, JS_VALUE_GET_TAG,
    },
    JS_ALIAS_BASE_DEF, JS_CFUNC_NATIVE_DEF, JS_CLASS_NUMBER, JS_PROP_DOUBLE_DEF,
};

use super::{f64_stringify, js_atof, JS_ToInt32, ATOD_ACCEPT_PREFIX_AFTER_SIGN, ATOD_INT_ONLY};

pub fn js_thisNumberValue(ctx: &mut JSContext, this_val: JSValue) -> JSResult {
    if JS_IsNumber(this_val) {
        return Ok(this_val.dup());
    } else if JS_VALUE_GET_TAG(this_val) == JS_TAG_OBJECT {
        let p = unsafe { this_val.object() };
        if p.class_id() == JS_CLASS_NUMBER {
            let d = unsafe { p.u.object_data };
            if JS_IsNumber(d) {
                return Ok(d.dup());
            }
        }
    }

    ctx.JS_ThrowTypeError(c"not a number");
    Err(-1)
}

pub fn js_parseInt(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let radix: i32 = unsafe { JS_ToInt32(ctx, args.get_raw(1))? };

    let ret = if radix != 0 && (radix < 2 || radix > 36) {
        JSValue::NAN
    } else {
        let str = args.get_raw(0).to_utf8_string(ctx)?;
        let mut p = str.as_ptr();

        unsafe {
            let skip_spaces_result = skip_spaces(p);
            p = p.add(skip_spaces_result);
            js_atof(
                ctx,
                p,
                std::ptr::null_mut(),
                radix,
                ATOD_INT_ONLY | ATOD_ACCEPT_PREFIX_AFTER_SIGN,
            )
            .check_exception()?
        }
    };

    Ok(ret)
}

pub fn js_parseFloat(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let str = args.get_raw(0).to_utf8_string(ctx)?;
    let mut p = str.as_ptr();

    let skip_spaces_result = unsafe { skip_spaces(p) };
    unsafe {
        p = p.add(skip_spaces_result);
        js_atof(ctx, p, std::ptr::null_mut(), 10, 0).check_exception()
    }
}

pub fn js_number_valueOf(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    js_thisNumberValue(ctx, args.this_raw())
}

pub unsafe fn js_get_radix(ctx: *mut JSContext, val: JSValueConst) -> i32 {
    let mut radix: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut radix, val) != 0 {
        return -1;
    }
    if !(2..=36).contains(&radix) {
        JS_ThrowRangeError(ctx, c"radix must be between 2 and 36");
        return -1;
    }
    radix
}

pub fn js_number_toString(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let val = js_thisNumberValue(ctx, args.this_raw())?.into_rc(ctx.rt());

    let base = if args.magic != 0 || args.get_raw(0).is_undefined() {
        10
    } else {
        let b = unsafe { js_get_radix(ctx, args.get_raw(0)) };
        if b < 0 {
            return Err(-1);
        }
        b
    };

    let mut d: f64 = 0.0;
    if unsafe { JS_ToFloat64(ctx, &mut d, *val) } != 0 {
        Err(-1)
    } else {
        ctx.create_string_from_utf8(
            f64_stringify(d, base as _, None, 0)
                .into_owned()
                .into_bytes(),
        )
        .check_exception()
    }
}

pub fn js_number_toFixed(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let val = js_thisNumberValue(ctx, args.this_raw())?.into_rc(ctx.rt());

    let mut d: f64 = 0.0;
    if unsafe { JS_ToFloat64(ctx, &mut d, *val) } != 0 {
        return Err(-1);
    }

    let mut f: i32 = 0;
    if unsafe { JS_ToInt32Sat(ctx, &mut f, args.get_raw(0)) } != 0 {
        return Err(-1);
    } else if f < 0 || f > 100 {
        unsafe { JS_ThrowRangeError(ctx, c"invalid number of digits") };
        return Err(-1);
    }

    if d.abs() >= 1e21 {
        unsafe { JS_ToStringFree(ctx, JSValue::from_f64_as_is(d)).check_exception() }
    } else {
        unsafe { js_dtoa(ctx, d, 10, f, JS_DTOA_FRAC_FORMAT).check_exception() }
    }
}

pub fn js_number_toExponential(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let val = js_thisNumberValue(ctx, args.this_raw())?.into_rc(ctx.rt());

    let mut d: f64 = 0.0;
    if unsafe { JS_ToFloat64(ctx, &mut d, *val) } != 0 {
        return Err(-1);
    }

    let s = if args.get_raw(0).is_undefined() {
        f64_stringify(d, 10, None, JS_DTOA_FORCE_EXP)
    } else {
        let mut f: i32 = 0;
        if unsafe { JS_ToInt32Sat(ctx, &mut f, args.get_raw(0)) } != 0 {
            return Err(-1);
        }
        if !(0..=20).contains(&f) {
            unsafe {
                JS_ThrowRangeError(ctx, c"invalid number of digits");
            }
            return Err(-1);
        }
        f += 1;
        f64_stringify(d, 10, Some(f as _), JS_DTOA_FORCE_EXP)
    };

    ctx.create_string_from_utf8(s.into_owned().into_bytes())
        .check_exception()
}

pub fn js_number_toPrecision(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let val = js_thisNumberValue(ctx, args.this_raw())?.into_rc(ctx.rt());

    let mut d: f64 = 0.0;
    if unsafe { JS_ToFloat64(ctx, &mut d, *val) } != 0 {
        return Err(-1);
    }

    let s = if !d.is_finite() {
        if d.is_nan() {
            "NaN"
        } else if d.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_string()
    } else {
        let a = args.get_raw(0);
        if a.is_undefined() {
            f64_stringify(d, 10, None, 0).into_owned() // as-is
        } else {
            let mut p: i32 = 0;

            if unsafe { JS_ToInt32Sat(ctx, &mut p, a) } != 0 {
                return Err(-1);
            } else if !(1..=100).contains(&p) {
                unsafe {
                    JS_ThrowRangeError(ctx, c"invalid number of digits");
                }
                return Err(-1);
            }

            let scientific = format!("{:.1$e}", d, p as usize - 1);
            let i = scientific.find('e').unwrap();
            // let mentissa = &scientific[..i];
            let exponent = scientific[i + 1..].parse::<i32>().unwrap();

            let use_scientific = if exponent < 0 {
                exponent.abs() > 6 // < 1e-6, use scientific notation
            } else {
                exponent >= p
            };

            if use_scientific {
                scientific
            } else {
                let n = p - 1 - exponent;
                format!("{:.1$}", d, if n >= 0 { n } else { p - 1 } as usize)
            }
        }
    };

    ctx.create_string_from_utf8(s.into_bytes())
        .check_exception()

    // if (*argv.offset(0)).is_undefined() {
    //     // as-is
    //     return JS_ToStringFree(ctx, JSValue::from_f64_as_is(d));
    // }

    // if JS_ToInt32Sat(ctx, &mut p, *argv.offset(0)) != 0 {
    //     return JSValue::EXCEPTION;
    // }
    // if !d.is_finite() {
    //     return JS_ToStringFree(ctx, JSValue::from_f64_as_is(d));
    // }
    // if !(1..=100).contains(&p) {
    //     return JS_ThrowRangeError(ctx, c"invalid number of digits");
    // }

    // js_dtoa(ctx, d, 10, p, JS_DTOA_FIXED_FORMAT)
}

pub const js_number_proto_funcs: [JSCFunctionListEntry; 6] = [
    JS_CFUNC_NATIVE_DEF!(c"toExponential", 1, js_number_toExponential, 0),
    JS_CFUNC_NATIVE_DEF!(c"toFixed", 1, js_number_toFixed, 0),
    JS_CFUNC_NATIVE_DEF!(c"toPrecision", 1, js_number_toPrecision, 0),
    JS_CFUNC_NATIVE_DEF!(c"toString", 1, js_number_toString, 0),
    JS_CFUNC_NATIVE_DEF!(c"toLocaleString", 0, js_number_toString, 1),
    JS_CFUNC_NATIVE_DEF!(c"valueOf", 0, js_number_valueOf, 0),
];

pub fn js_number_isNaN_isFinite(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    if !JS_IsNumber(args.get_raw(0)) {
        Ok(JSValue::FALSE)
    } else {
        js_global_isNaN_isFinite(ctx, args)
    }
}

pub fn js_number_isInteger(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let b = args.get_raw(0).is_integer(ctx)?;
    Ok(b.into())
}

pub fn js_number_isSafeInteger(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let val = args.get_raw(0);
    if !JS_IsNumber(val) {
        return Ok(JSValue::FALSE);
    }

    let mut d: f64 = 0.0;
    if unsafe { JS_ToFloat64(ctx, &mut d, val) } != 0 {
        return Err(-1);
    }

    Ok(is_safe_integer(d).into())
}

pub const js_number_funcs: [JSCFunctionListEntry; 14] = [
    // global ParseInt and parseFloat should be defined already or delayed
    JS_ALIAS_BASE_DEF!(c"parseInt", c"parseInt", 0),
    JS_ALIAS_BASE_DEF!(c"parseFloat", c"parseFloat", 0),
    JS_CFUNC_NATIVE_DEF!(c"isNaN", 1, js_number_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    JS_CFUNC_NATIVE_DEF!(
        c"isFinite",
        1,
        js_number_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _
    ),
    JS_CFUNC_NATIVE_DEF!(c"isInteger", 1, js_number_isInteger, 0),
    JS_CFUNC_NATIVE_DEF!(c"isSafeInteger", 1, js_number_isSafeInteger, 0),
    JS_PROP_DOUBLE_DEF!(c"MAX_VALUE", 1.797_693_134_862_315_7e308, 0),
    JS_PROP_DOUBLE_DEF!(c"MIN_VALUE", 5e-324, 0),
    JS_PROP_DOUBLE_DEF!(c"NaN", f64::NAN, 0),
    JS_PROP_DOUBLE_DEF!(c"NEGATIVE_INFINITY", -f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!(c"POSITIVE_INFINITY", f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!(c"EPSILON", 2.220446049250313e-16, 0), /* ES6 */
    JS_PROP_DOUBLE_DEF!(c"MAX_SAFE_INTEGER", 9007199254740991.0, 0), /* ES6 */
    JS_PROP_DOUBLE_DEF!(c"MIN_SAFE_INTEGER", -9007199254740991.0, 0), /* ES6 */
];

pub unsafe fn js_number_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val: JSValue;
    if argc == 0 {
        val = JSValue::ZERO;
    } else {
        val = JS_ToNumeric(ctx, *argv.offset(0));
        if JS_IsException(val) {
            return val;
        }
        if JS_VALUE_GET_TAG(val) == JS_TAG_BIG_INT {
            (*ctx).JS_ThrowTypeError(c"BigInt is not supported");
            return JSValue::EXCEPTION;
        }
    }

    if !JS_IsUndefined(new_target) {
        let obj = js_create_from_ctor(ctx, new_target, JS_CLASS_NUMBER);
        if !JS_IsException(obj) {
            JS_SetObjectData(ctx, obj, val);
        }
        obj
    } else {
        val
    }
}
