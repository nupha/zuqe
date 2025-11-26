use crate::{
    JS_CFUNC_FF_DEF, JS_CFUNC_FFF_DEF, JS_PROP_DOUBLE_DEF,
    context::ContextRef,
    error::JSResult,
    function::{CallContext, FieldDef},
    number::{IntOrFloat, JSFloat64Union},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
    value::JSValue,
};

pub fn js_math_min_max(ctx: ContextRef, cc: CallContext) -> JSResult {
    let is_max = cc.magic() != 0;

    if cc.length() == 0 {
        return Ok(JSValue::Float64(if is_max {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }));
    }

    let mut res = if let JSValue::Int(r1) = cc.arg(0).to_number(&ctx)? {
        IntOrFloat::Int64(r1 as _)
    } else {
        IntOrFloat::Float64(cc.arg(0).to_f64(&ctx)?)
    };

    if cc.length() > 1 {
        for a in &cc.arguments()[1..] {
            match (a.to_number(&ctx)?, &mut res) {
                // int vs int
                (JSValue::Int(n), IntOrFloat::Int64(mx)) => {
                    *mx = if is_max {
                        std::cmp::max(n as i64, *mx)
                    } else {
                        std::cmp::min(n as i64, *mx)
                    }
                }

                // nan and infinity.
                // don't change order
                (JSValue::Float64(d), _) if d.is_nan() => {
                    return Ok(JSValue::Float64(f64::NAN));
                }
                (_, IntOrFloat::Float64(d)) if d.is_nan() => {
                    return Ok(JSValue::Float64(f64::NAN));
                }

                (JSValue::Float64(f64::INFINITY), _) | (_, IntOrFloat::Float64(f64::INFINITY))
                    if is_max =>
                {
                    res = IntOrFloat::Float64(f64::INFINITY);
                }
                (JSValue::Float64(f64::NEG_INFINITY), _)
                | (_, IntOrFloat::Float64(f64::NEG_INFINITY))
                    if !is_max =>
                {
                    res = IntOrFloat::Float64(f64::NEG_INFINITY);
                }

                // int vs float
                (JSValue::Int(n), IntOrFloat::Float64(mx)) => {
                    let n = n as f64;
                    match n.total_cmp(mx) {
                        std::cmp::Ordering::Greater => {
                            if is_max {
                                *mx = n;
                            }
                        }
                        std::cmp::Ordering::Less => {
                            if !is_max {
                                *mx = n;
                            }
                        }
                        _ => {}
                    }
                }
                (JSValue::Float64(d), IntOrFloat::Int64(mx)) => {
                    let mut m = *mx as f64;
                    match d.total_cmp(&m) {
                        std::cmp::Ordering::Greater => {
                            if is_max {
                                m = d;
                            }
                        }
                        std::cmp::Ordering::Less => {
                            if !is_max {
                                m = d;
                            }
                        }
                        _ => {}
                    }
                    res = IntOrFloat::Float64(m);
                }

                (JSValue::Float64(d), IntOrFloat::Float64(mx)) => match d.total_cmp(mx) {
                    std::cmp::Ordering::Greater => {
                        if is_max {
                            *mx = d;
                        }
                    }
                    std::cmp::Ordering::Less => {
                        if !is_max {
                            *mx = d;
                        }
                    }
                    _ => {}
                },

                _ => {
                    unreachable!()
                }
            }
        }
    }

    Ok(res.into())
}

pub const fn js_math_round(a: f64) -> f64 {
    let mut u = JSFloat64Union { d: a };
    let e = unsafe { (u.uint64 >> 52) as u32 } & 0x7ff;

    unsafe {
        if e < 1023 {
            /* abs(a) < 1 */
            if e == (1023 - 1) && u.uint64 != 0xbfe0000000000000 {
                /* abs(a) > 0.5 or a = 0.5: return +/-1.0 */
                u.uint64 = (u.uint64 & (1 << 63)) | (1023 << 52);
            } else {
                /* return +/-0.0 */
                u.uint64 &= 1 << 63;
            }
        } else if e < 1023 + 52 {
            let s = ((u.uint64 >> 63) & 1) as u32;
            let one = 1 << (52 - (e - 1023));
            let frac_mask = one - 1;
            u.uint64 += (one >> 1) - (s as u64);
            u.uint64 &= !frac_mask; /* truncate to an integer */
        }

        /* otherwise: abs(a) >= 2^52, or NaN, +/-Infinity: no change */
        u.d
    }
}

pub fn js_math_hypot(ctx: ContextRef, cc: CallContext) -> JSResult {
    let mut r = 0.0;

    if cc.length() > 0 {
        r = cc.arg(0).to_f64(&ctx)?;

        if cc.length() == 1 {
            r = r.abs();
        } else {
            // use the built-in function to minimize precision loss
            for v in &cc.arguments()[1..] {
                let a = v.to_f64(&ctx)?;
                r = r.hypot(a);
            }
        }
    }

    Ok(JSValue::from(r))
}

pub fn js_math_imul(ctx: ContextRef, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_i32(&ctx)?;
    let b = cc.arg(1).to_i32(&ctx)?;
    Ok(JSValue::Int(a.wrapping_mul(b)))
}

pub fn js_math_clz32(ctx: ContextRef, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_u32(&ctx)?;
    let r: u32 = if a == 0 { 32 } else { a.leading_zeros() };
    Ok(JSValue::Int(r as i32))
}

/* xorshift* random number generator by Marsaglia */
#[inline]
const fn xorshift64star(pstate: &mut u64) -> u64 {
    let mut x = *pstate;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *pstate = x;

    x.wrapping_mul(0x2545F4914F6CDD1D)
}

pub fn js_math_random(mut ctx: ContextRef, _: CallContext) -> JSResult {
    let v = xorshift64star(&mut ctx.random_state);
    /* 1.0 <= u.d < 2 */
    let u = JSFloat64Union {
        uint64: (0x3ff << 52) | (v >> 12),
    };
    Ok(JSValue::Float64(unsafe { u.d } - 1.0))
}

pub static js_math_funcs: [FieldDef; 44] = [
    FieldDef::FUNCTION("min", 2, js_math_min_max, 0),
    FieldDef::FUNCTION("max", 2, js_math_min_max, 1),
    //
    JS_CFUNC_FF_DEF!("abs", 1, f64::abs, 0),
    JS_CFUNC_FF_DEF!("floor", 1, f64::floor, 0),
    JS_CFUNC_FF_DEF!("ceil", 1, f64::ceil, 0),
    JS_CFUNC_FF_DEF!("round", 1, js_math_round, 0),
    JS_CFUNC_FF_DEF!("sqrt", 1, f64::sqrt, 0),
    JS_CFUNC_FF_DEF!("acos", 1, f64::acos, 0),
    JS_CFUNC_FF_DEF!("asin", 1, f64::asin, 0),
    JS_CFUNC_FF_DEF!("atan", 1, f64::atan, 0),
    JS_CFUNC_FFF_DEF!("atan2", 2, f64::atan2, 0),
    JS_CFUNC_FF_DEF!("cos", 1, f64::cos, 0),
    JS_CFUNC_FF_DEF!("exp", 1, f64::exp, 0),
    JS_CFUNC_FF_DEF!("log", 1, f64::ln, 0),
    JS_CFUNC_FFF_DEF!(
        "pow",
        2,
        |a: f64, b: f64| -> f64 {
            if !b.is_finite() && a.abs() == 1.0 {
                f64::NAN // not compatible with IEEE 754
            } else {
                a.powf(b)
            }
        },
        0
    ),
    JS_CFUNC_FF_DEF!("sin", 1, f64::sin, 0),
    JS_CFUNC_FF_DEF!("tan", 1, f64::tan, 0),
    // ES6
    JS_CFUNC_FF_DEF!("trunc", 1, f64::trunc, 0),
    JS_CFUNC_FF_DEF!(
        "sign",
        1,
        |a: f64| -> f64 {
            if a.is_nan() || a == 0.0 {
                a
            } else if a < 0.0 {
                -1.0
            } else {
                1.0
            }
        },
        0
    ),
    JS_CFUNC_FF_DEF!("cosh", 1, f64::cosh, 0),
    JS_CFUNC_FF_DEF!("sinh", 1, f64::sinh, 0),
    JS_CFUNC_FF_DEF!("tanh", 1, f64::tanh, 0),
    JS_CFUNC_FF_DEF!("acosh", 1, f64::acosh, 0),
    JS_CFUNC_FF_DEF!("asinh", 1, f64::asinh, 0),
    JS_CFUNC_FF_DEF!("atanh", 1, f64::atanh, 0),
    JS_CFUNC_FF_DEF!("expm1", 1, f64::exp_m1, 0),
    JS_CFUNC_FF_DEF!("log1p", 1, f64::ln_1p, 0),
    JS_CFUNC_FF_DEF!("log2", 1, f64::log2, 0),
    JS_CFUNC_FF_DEF!("log10", 1, f64::log10, 0),
    JS_CFUNC_FF_DEF!("cbrt", 1, f64::cbrt, 0),
    FieldDef::FUNCTION("hypot", 2, js_math_hypot, 0),
    FieldDef::FUNCTION("random", 0, js_math_random, 0),
    JS_CFUNC_FF_DEF!("fround", 1, |a: f64| -> f64 { (a as f32) as f64 }, 0),
    FieldDef::FUNCTION("imul", 2, js_math_imul, 0),
    FieldDef::FUNCTION("clz32", 1, js_math_clz32, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Math", JS_PROP_CONFIGURABLE),
    JS_PROP_DOUBLE_DEF!("E", std::f64::consts::E, 0),
    JS_PROP_DOUBLE_DEF!("LN10", std::f64::consts::LN_10, 0),
    JS_PROP_DOUBLE_DEF!("LN2", std::f64::consts::LN_2, 0),
    JS_PROP_DOUBLE_DEF!("LOG2E", std::f64::consts::LOG2_E, 0),
    JS_PROP_DOUBLE_DEF!("LOG10E", std::f64::consts::LOG10_E, 0),
    JS_PROP_DOUBLE_DEF!("PI", std::f64::consts::PI, 0),
    JS_PROP_DOUBLE_DEF!("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2, 0),
    JS_PROP_DOUBLE_DEF!("SQRT2", std::f64::consts::SQRT_2, 0),
];

pub static js_math_obj: [FieldDef; 1] = [FieldDef::OBJECT(
    "Math",
    &js_math_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
