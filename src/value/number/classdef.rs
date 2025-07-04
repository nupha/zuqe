use crate::{
    JS_ALIAS_BASE_DEF, JS_CFUNC_NATIVE_DEF, JS_PROP_DOUBLE_DEF,
    context::JSContext,
    error::{JS_ThrowRangeError, JSResult},
    function::{CallContext, NativeFieldDef},
    js_core::global::{MAGIC_NUM_IS_FINITE, MAGIC_NUM_IS_NAN, js_global_isNaN_isFinite},
    object::JSObjectIsa,
    utils::skip_spaces,
    value::{
        JS_DTOA_FORCE_EXP, JS_ToFloat64, JS_ToInt32Sat, JSValue, JSValueConst, is_safe_integer,
    },
};

use super::{ATOD_ACCEPT_PREFIX_AFTER_SIGN, ATOD_INT_ONLY, JS_ToInt32, f64_stringify, js_atof};

pub fn js_thisNumberValue(ctx: &JSContext, this_val: JSValue) -> JSResult {
    if this_val.is_number() {
        Ok(this_val)
    } else {
        this_val
            .object()
            .and_then(|p| p.0.isa.borrow().value().cloned())
            .and_then(|x| if x.is_number() { Some(x) } else { None })
            .ok_or_else(|| {
                ctx.JS_ThrowTypeError("not a number");
                -1
            })
    }
}

pub fn js_parseInt(ctx: &JSContext, args: CallContext) -> JSResult {
    let radix: i32 = unsafe { JS_ToInt32(ctx, args.arg(1))? };

    let ret = if radix != 0 && !(2..=36).contains(&radix) {
        JSValue::Float64(f64::NAN)
    } else {
        let str = args.arg(0).coerce_to_utf8(ctx)?;
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

pub fn js_parseFloat(ctx: &JSContext, args: CallContext) -> JSResult {
    let str = args.arg(0).coerce_to_utf8(ctx)?;
    let mut p = str.as_ptr();

    let skip_spaces_result = unsafe { skip_spaces(p) };
    unsafe {
        p = p.add(skip_spaces_result);
        js_atof(ctx, p, std::ptr::null_mut(), 10, 0).check_exception()
    }
}

pub fn js_number_valueOf(ctx: &JSContext, args: CallContext) -> JSResult {
    js_thisNumberValue(ctx, args.this_raw())
}

pub unsafe fn js_get_radix(ctx: &JSContext, val: &JSValueConst) -> i32 {
    let mut radix: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut radix, val) != 0 {
        return -1;
    }
    if !(2..=36).contains(&radix) {
        JS_ThrowRangeError(ctx, "radix must be between 2 and 36");
        return -1;
    }
    radix
}

const FMT_TO_FIXED: i32 = 1;
const FMT_TO_PRECISION: i32 = 2;
const FMT_TO_EXPONENTIAL: i32 = 3;
const FMT_TO_STRING: i32 = 4;
const FMT_TO_LOCALE_STRING: i32 = 5;

// pub fn js_number_toString(ctx: &JSContext, args: CallContext) -> JSResult {
//     let val = js_thisNumberValue(ctx, args.this_raw())?.into_rc(ctx.rt());

//     let base = if args.magic != 0 || args.get_raw(0).is_undefined() {
//         10
//     } else {
//         let b = unsafe { js_get_radix(ctx, args.get_raw(0)) };
//         if b < 0 {
//             return Err(-1);
//         }
//         b
//     };

//     let mut d: f64 = 0.0;
//     if unsafe { JS_ToFloat64(ctx, &mut d, *val) } != 0 {
//         Err(-1)
//     } else {
//         ctx.create_string_from_utf8(
//             f64_stringify(d, base as _, None, 0)
//                 .into_owned()
//                 .into_bytes(),
//         )
//         .check_exception()
//     }
// }

// pub fn js_number_toExponential(ctx: &JSContext, args: CallContext) -> JSResult {
//     let val = js_thisNumberValue(ctx, args.this_raw())?.into_rc(ctx.rt());

//     let mut d: f64 = 0.0;
//     if unsafe { JS_ToFloat64(ctx, &mut d, *val) } != 0 {
//         return Err(-1);
//     }

//     let s = if args.get_raw(0).is_undefined() {
//         f64_stringify(d, 10, None, JS_DTOA_FORCE_EXP)
//     } else {
//         let mut f: i32 = 0;
//         if unsafe { JS_ToInt32Sat(ctx, &mut f, args.get_raw(0)) } != 0 {
//             return Err(-1);
//         }
//         if !(0..=20).contains(&f) {
//             unsafe {
//                 JS_ThrowRangeError(ctx, c"invalid number of digits");
//             }
//             return Err(-1);
//         }
//         f += 1;
//         f64_stringify(d, 10, Some(f as _), JS_DTOA_FORCE_EXP)
//     };

//     ctx.create_string_from_utf8(s.into_owned().into_bytes())
//         .check_exception()
// }

pub fn js_number_fmt(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = js_thisNumberValue(ctx, cc.this_raw())?;

    let mut d: f64 = 0.0;
    if unsafe { JS_ToFloat64(ctx, &mut d, &val) } != 0 {
        return Err(-1);
    }

    let a0 = cc.arg(0);
    let a = if a0.is_undefined() {
        None
    } else {
        let mut f: i32 = 0;
        if unsafe { JS_ToInt32Sat(ctx, &mut f, a0) } != 0 {
            return Err(-1);
        }
        Some(f)
    };

    let s: String = if !d.is_finite() {
        if d.is_nan() {
            "NaN"
        } else if d.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_string()
    } else {
        match cc.magic() {
            m @ (FMT_TO_STRING | FMT_TO_LOCALE_STRING) => {
                let base = if m == FMT_TO_LOCALE_STRING {
                    10
                } else {
                    a.unwrap_or(10)
                };
                if !(2..=36).contains(&base) {
                    unsafe {
                        JS_ThrowRangeError(ctx, "radix must be between 2 and 36");
                    }
                    return Err(-1);
                }

                let mut d: f64 = 0.0;
                if unsafe { JS_ToFloat64(ctx, &mut d, &val) } != 0 {
                    return Err(-1);
                }

                f64_stringify(d, base as _, None, 0).into_owned()
            }

            FMT_TO_FIXED => {
                let f = a.unwrap_or(0);
                if !(0..=20).contains(&f) {
                    JS_ThrowRangeError(ctx, "invalid number of digits");
                    return Err(-1);
                }
                let scale = 10_f64.powf(f as f64);
                let n = d * scale;
                d = n.round() / scale;

                if d.abs() >= 1e21 {
                    f64_stringify(d, 10, Some(f as u32 + 1), JS_DTOA_FORCE_EXP).into_owned()
                } else {
                    format!("{:.1$}", d, f as usize)
                }
            }

            FMT_TO_PRECISION => f64_stringify(
                d,
                10,
                if let Some(p) = a {
                    if !(1..=100).contains(&p) {
                        unsafe {
                            JS_ThrowRangeError(ctx, "invalid number of digits");
                        }
                        return Err(-1);
                    }
                    Some(p as u32)
                } else {
                    None
                },
                0,
            )
            .into_owned(),

            FMT_TO_EXPONENTIAL => if let Some(f) = a {
                if !(0..=20).contains(&f) {
                    unsafe {
                        JS_ThrowRangeError(ctx, "invalid number of digits");
                    }
                    return Err(-1);
                }
                f64_stringify(d, 10, Some(f as u32 + 1), JS_DTOA_FORCE_EXP)
            } else {
                f64_stringify(d, 10, None, JS_DTOA_FORCE_EXP)
            }
            .into_owned(),

            _ => {
                unreachable!();
            }
        }
    };

    ctx.create_string_from_utf8(s.into_bytes())
        .check_exception()
}

pub const js_number_proto_funcs: [NativeFieldDef; 6] = [
    JS_CFUNC_NATIVE_DEF!("toFixed", 1, js_number_fmt, FMT_TO_FIXED as i16),
    JS_CFUNC_NATIVE_DEF!("toPrecision", 1, js_number_fmt, FMT_TO_PRECISION as i16),
    JS_CFUNC_NATIVE_DEF!("toExponential", 1, js_number_fmt, FMT_TO_EXPONENTIAL as i16),
    JS_CFUNC_NATIVE_DEF!("toString", 1, js_number_fmt, FMT_TO_STRING as i16),
    JS_CFUNC_NATIVE_DEF!(
        "toLocaleString",
        0,
        js_number_fmt,
        FMT_TO_LOCALE_STRING as i16
    ),
    JS_CFUNC_NATIVE_DEF!("valueOf", 0, js_number_valueOf, 0),
];

pub fn js_number_isNaN_isFinite(ctx: &JSContext, args: CallContext) -> JSResult {
    if !args.arg(0).is_number() {
        Ok(JSValue::Bool(false))
    } else {
        js_global_isNaN_isFinite(ctx, args)
    }
}

pub fn js_number_isInteger(ctx: &JSContext, args: CallContext) -> JSResult {
    let b = args.arg(0).is_integer(ctx)?;
    Ok(b.into())
}

pub fn js_number_isSafeInteger(ctx: &JSContext, args: CallContext) -> JSResult {
    let val = args.arg(0);
    if !val.is_number() {
        return Ok(JSValue::Bool(false));
    }

    let mut d: f64 = 0.0;
    if unsafe { JS_ToFloat64(ctx, &mut d, val) } != 0 {
        return Err(-1);
    }

    Ok(is_safe_integer(d).into())
}

pub const js_number_funcs: [NativeFieldDef; 14] = [
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

pub fn js_number_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = if cc.length() == 0 {
        JSValue::Int(0)
    } else {
        cc.arg(0).coerce_to_numeric(ctx)?
    };

    let new_target = cc.this();
    if !new_target.is_undefined() {
        ctx.create_instance_from_ctor(new_target, JSObjectIsa::NUMBER(val))
            .map_err(|_| -1)
    } else {
        Ok(val)
    }
}
