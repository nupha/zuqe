// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    number::{Float64Union, IntOrFloat, math_f16round},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
    value::JSValue,
};

fn js_math_min_max(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_max = cc.magic() != 0;

    if cc.length() == 0 {
        return Ok(JSValue::make_float(if is_max {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }));
    }

    let mut res = cc.arg(0).to_number(ctx)?;

    if cc.length() > 1 {
        for a in &cc.arguments()[1..] {
            match (a.to_number(ctx)?, &mut res) {
                // int vs int
                (IntOrFloat::Int64(n), IntOrFloat::Int64(mx)) => {
                    *mx = if is_max {
                        std::cmp::max(n, *mx)
                    } else {
                        std::cmp::min(n, *mx)
                    }
                }

                // nan and infinity.
                // don't change order
                (IntOrFloat::Float64(d), _) if d.is_nan() => {
                    return Ok(JSValue::NAN);
                }
                (_, IntOrFloat::Float64(d)) if d.is_nan() => {
                    return Ok(JSValue::NAN);
                }
                (IntOrFloat::Float64(f64::INFINITY), _)
                | (_, IntOrFloat::Float64(f64::INFINITY))
                    if is_max =>
                {
                    res = IntOrFloat::Float64(f64::INFINITY);
                }
                (IntOrFloat::Float64(f64::NEG_INFINITY), _)
                | (_, IntOrFloat::Float64(f64::NEG_INFINITY))
                    if !is_max =>
                {
                    res = IntOrFloat::Float64(f64::NEG_INFINITY);
                }

                // int vs float
                (IntOrFloat::Int64(n), IntOrFloat::Float64(mx)) => {
                    let n = n as f64;
                    match n.total_cmp(mx) {
                        std::cmp::Ordering::Greater if is_max => {
                            *mx = n;
                        }
                        std::cmp::Ordering::Less if !is_max => {
                            *mx = n;
                        }
                        _ => {}
                    }
                }
                (IntOrFloat::Float64(d), IntOrFloat::Int64(mx)) => {
                    let mut m = *mx as f64;
                    match d.total_cmp(&m) {
                        std::cmp::Ordering::Greater if is_max => {
                            m = d;
                        }
                        std::cmp::Ordering::Less if !is_max => {
                            m = d;
                        }
                        _ => {}
                    }
                    res = IntOrFloat::Float64(m);
                }

                (IntOrFloat::Float64(d), IntOrFloat::Float64(mx)) => match d.total_cmp(mx) {
                    std::cmp::Ordering::Greater if is_max => {
                        *mx = d;
                    }
                    std::cmp::Ordering::Less if !is_max => {
                        *mx = d;
                    }
                    _ => {}
                },
            }
        }
    }

    Ok(res.into())
}

pub const fn js_math_round(a: f64) -> f64 {
    let mut u = Float64Union { d: a };
    let e = unsafe { (u.uint64 >> 52) as u32 } & 0x7ff;

    unsafe {
        if e < 1023 {
            // abs(a) < 1
            if e == (1023 - 1) && u.uint64 != 0xbfe0000000000000 {
                // abs(a) > 0.5 or a = 0.5: return +/-1.0
                u.uint64 = (u.uint64 & (1 << 63)) | (1023 << 52);
            } else {
                // return +/-0.0
                u.uint64 &= 1 << 63;
            }
        } else if e < 1023 + 52 {
            let s = ((u.uint64 >> 63) & 1) as u32;
            let one = 1 << (52 - (e - 1023));
            let frac_mask = one - 1;
            u.uint64 += (one >> 1) - (s as u64);
            u.uint64 &= !frac_mask; // truncate to an integer 
        }

        // otherwise: abs(a) >= 2^52, or NaN, +/-Infinity: no change
        u.d
    }
}

fn js_math_hypot(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut r = 0.0;

    if cc.length() > 0 {
        r = cc.arg(0).to_number(ctx)?.to_f64();

        if cc.length() == 1 {
            r = r.abs();
        } else {
            // use the built-in function to minimize precision loss
            for v in &cc.arguments()[1..] {
                let a = v.to_number(ctx)?.to_f64();
                r = r.hypot(a);
            }
        }
    }

    Ok(JSValue::from(r))
}

fn js_math_imul(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_number(ctx)?.to_i32();
    let b = cc.arg(1).to_number(ctx)?.to_i32();
    Ok(JSValue::from(a.wrapping_mul(b)))
}

fn js_math_clz32(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_number(ctx)?.to_u32();
    let r: u32 = if a == 0 { 32 } else { a.leading_zeros() };
    Ok(JSValue::from(r as i32))
}

// xorshift* random number generator by Marsaglia
#[inline]
const fn xorshift64star(pstate: &mut u64) -> u64 {
    let mut x = *pstate;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *pstate = x;

    x.wrapping_mul(0x2545F4914F6CDD1D)
}

pub fn js_math_random(ctx: &mut ScopedContext, _: CallContext) -> JSResult {
    let v = xorshift64star(&mut ctx.random_state);
    // 1.0 <= u.d < 2
    let u = Float64Union {
        uint64: (0x3ff << 52) | (v >> 12),
    };
    Ok(JSValue::make_float(unsafe { u.d } - 1.0))
}

/// Math.sumPrecise (ES2026): exact summation of Number values, correctly
/// rounded once at the end.
///
/// Shewchuk exact-addition partials plus a "biased" overflow partial
/// (conceptually scaled by 2**1024), ported from the proposal's reference
/// implementation; non-finite values follow the spec's state machine.
fn js_math_sumprecise(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if !cc.arg(0).is_object() {
        return Err(ctx.throw_type_error("Math.sumPrecise requires an iterable"));
    }

    // twosum with |x| >= |y|: x + y == hi + lo exactly
    #[inline]
    fn twosum(x: f64, y: f64) -> (f64, f64) {
        let hi = x + y;
        (hi, y - (hi - x))
    }

    const TWO_POW_1023: f64 = f64::from_bits(2046 << 52);
    const MAX_ULP_HALF: f64 = f64::from_bits(1993 << 52); // 2**970

    enum St {
        MinusZero,
        Finite,
        PlusInf,
        MinusInf,
        NotANumber,
    }

    let mut st = St::MinusZero;
    let mut partials: Vec<f64> = Vec::new();
    // overflow counter, conceptually scaled by 2**1024
    let mut overflow: i64 = 0;

    for item in cc.arg(0).iter(ctx)? {
        let val = item?;
        if !val.is_number() {
            return Err(ctx.throw_type_error("Math.sumPrecise requires Number values"));
        }
        let mut x = val.to_number(ctx)?.to_f64();

        match st {
            St::NotANumber => {}
            St::PlusInf => {
                if x == f64::NEG_INFINITY {
                    st = St::NotANumber;
                }
            }
            St::MinusInf => {
                if x == f64::INFINITY {
                    st = St::NotANumber;
                }
            }
            St::MinusZero | St::Finite => {
                if x.is_nan() {
                    st = St::NotANumber;
                } else if x == f64::INFINITY {
                    st = St::PlusInf;
                } else if x == f64::NEG_INFINITY {
                    st = St::MinusInf;
                } else {
                    // any non-minus-zero element flips the state to finite
                    if !(x == 0.0 && x.is_sign_negative()) {
                        st = St::Finite;
                    }
                    // merge x into the non-overlapping partial sums
                    let mut used = 0usize;
                    for m in 0..partials.len() {
                        let mut y = partials[m];
                        if x.abs() < y.abs() {
                            std::mem::swap(&mut x, &mut y);
                        }
                        let (mut hi, lo0) = twosum(x, y);
                        let mut lo = lo0;
                        if hi.abs() == f64::INFINITY {
                            // exact sum exceeds a double: peel off 2**1024
                            // into the biased overflow counter and retry
                            let sign: f64 = if hi == f64::INFINITY { 1.0 } else { -1.0 };
                            overflow += sign as i64;
                            if overflow.abs() >= 1i64 << 53 {
                                return Err(ctx.throw_range_error("Math.sumPrecise overflow"));
                            }
                            x = (x - sign * TWO_POW_1023) - sign * TWO_POW_1023;
                            if x.abs() < y.abs() {
                                std::mem::swap(&mut x, &mut y);
                            }
                            let t = twosum(x, y);
                            hi = t.0;
                            lo = t.1;
                        }
                        if lo != 0.0 {
                            partials[used] = lo;
                            used += 1;
                        }
                        x = hi;
                    }
                    partials.truncate(used);
                    if x != 0.0 {
                        partials.push(x);
                    }
                }
            }
        }
    }

    match st {
        St::NotANumber => Ok(JSValue::make_float(f64::NAN)),
        St::PlusInf => Ok(JSValue::make_float(f64::INFINITY)),
        St::MinusInf => Ok(JSValue::make_float(f64::NEG_INFINITY)),
        St::MinusZero => Ok(JSValue::make_float(-0.0)),
        St::Finite => {
            // combine the partials, rounding once at the end
            let mut n: isize = partials.len() as isize - 1;
            let mut hi;
            let mut lo;

            if overflow != 0 {
                let next = if n >= 0 { partials[n as usize] } else { 0.0 };
                n -= 1;
                if overflow.abs() > 1
                    || (overflow > 0 && next > 0.0)
                    || (overflow < 0 && next < 0.0)
                {
                    return Ok(JSValue::make_float(if overflow > 0 {
                        f64::INFINITY
                    } else {
                        f64::NEG_INFINITY
                    }));
                }
                let t = twosum(overflow as f64 * TWO_POW_1023, next / 2.0);
                hi = t.0;
                lo = t.1 * 2.0;
                if (2.0 * hi).abs() == f64::INFINITY {
                    // rounding at the top of the double range: only a single
                    // opposite-signed tiny remainder can keep it finite
                    if hi > 0.0 {
                        if hi == TWO_POW_1023
                            && lo == -MAX_ULP_HALF
                            && n >= 0
                            && partials[n as usize] < 0.0
                        {
                            return Ok(JSValue::make_float(f64::MAX));
                        }
                        return Ok(JSValue::make_float(f64::INFINITY));
                    } else if hi == -TWO_POW_1023
                        && lo == MAX_ULP_HALF
                        && n >= 0
                        && partials[n as usize] > 0.0
                    {
                        return Ok(JSValue::make_float(-f64::MAX));
                    } else {
                        return Ok(JSValue::make_float(f64::NEG_INFINITY));
                    }
                }
                if lo != 0.0 {
                    partials[(n + 1) as usize] = lo;
                    n += 1;
                    lo = 0.0;
                }
                hi *= 2.0;
            } else {
                hi = 0.0;
                lo = 0.0;
            }

            while n >= 0 {
                let y = partials[n as usize];
                n -= 1;
                let t = twosum(hi, y);
                hi = t.0;
                lo = t.1;
                if lo != 0.0 {
                    break;
                }
            }

            // half-ULP roundoff: peek one more partial to break the tie
            if n >= 0
                && ((lo < 0.0 && partials[n as usize] < 0.0)
                    || (lo > 0.0 && partials[n as usize] > 0.0))
            {
                let y = lo * 2.0;
                let x = hi + y;
                if y == x - hi {
                    hi = x;
                }
            }

            Ok(JSValue::make_float(hi))
        }
    }
}

pub const js_math_funcs: &[FieldDef] = &[
    FieldDef::FUNC("min", 2, js_math_min_max, 0),
    FieldDef::FUNC("max", 2, js_math_min_max, 1),
    //
    FieldDef::CALL_FF("abs", 1, f64::abs, 0),
    FieldDef::CALL_FF("floor", 1, f64::floor, 0),
    FieldDef::CALL_FF("ceil", 1, f64::ceil, 0),
    FieldDef::CALL_FF("round", 1, js_math_round, 0),
    FieldDef::CALL_FF("sqrt", 1, f64::sqrt, 0),
    FieldDef::CALL_FF("acos", 1, f64::acos, 0),
    FieldDef::CALL_FF("asin", 1, f64::asin, 0),
    FieldDef::CALL_FF("atan", 1, f64::atan, 0),
    FieldDef::CALL_FFF("atan2", 2, f64::atan2, 0),
    FieldDef::CALL_FF("cos", 1, f64::cos, 0),
    FieldDef::CALL_FF("exp", 1, f64::exp, 0),
    FieldDef::CALL_FF("log", 1, f64::ln, 0),
    FieldDef::CALL_FFF(
        "pow",
        2,
        |a: f64, b: f64| -> f64 {
            if !b.is_finite() && a.abs() == 1.0 {
                f64::NAN // not compatible with IEEE 754
            } else {
                a.powf(b)
            }
        },
        0,
    ),
    FieldDef::CALL_FF("sin", 1, f64::sin, 0),
    FieldDef::CALL_FF("tan", 1, f64::tan, 0),
    // ES6
    FieldDef::CALL_FF("trunc", 1, f64::trunc, 0),
    FieldDef::CALL_FF(
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
        0,
    ),
    FieldDef::CALL_FF("cosh", 1, f64::cosh, 0),
    FieldDef::CALL_FF("sinh", 1, f64::sinh, 0),
    FieldDef::CALL_FF("tanh", 1, f64::tanh, 0),
    FieldDef::CALL_FF("acosh", 1, f64::acosh, 0),
    FieldDef::CALL_FF("asinh", 1, f64::asinh, 0),
    FieldDef::CALL_FF("atanh", 1, f64::atanh, 0),
    FieldDef::CALL_FF("expm1", 1, f64::exp_m1, 0),
    FieldDef::CALL_FF("log1p", 1, f64::ln_1p, 0),
    FieldDef::CALL_FF("log2", 1, f64::log2, 0),
    FieldDef::CALL_FF("log10", 1, f64::log10, 0),
    FieldDef::CALL_FF("cbrt", 1, f64::cbrt, 0),
    FieldDef::FUNC("hypot", 2, js_math_hypot, 0),
    FieldDef::FUNC("random", 0, js_math_random, 0),
    FieldDef::FUNC("sumPrecise", 1, js_math_sumprecise, 0),
    FieldDef::CALL_FF("fround", 1, |a: f64| -> f64 { (a as f32) as f64 }, 0),
    FieldDef::CALL_FF("f16round", 1, |a: f64| -> f64 { math_f16round(a) }, 0),
    FieldDef::FUNC("imul", 2, js_math_imul, 0),
    FieldDef::FUNC("clz32", 1, js_math_clz32, 0),
    FieldDef::FLOAT64("E", std::f64::consts::E, 0),
    FieldDef::FLOAT64("LN10", std::f64::consts::LN_10, 0),
    FieldDef::FLOAT64("LN2", std::f64::consts::LN_2, 0),
    FieldDef::FLOAT64("LOG2E", std::f64::consts::LOG2_E, 0),
    FieldDef::FLOAT64("LOG10E", std::f64::consts::LOG10_E, 0),
    FieldDef::FLOAT64("PI", std::f64::consts::PI, 0),
    FieldDef::FLOAT64("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2, 0),
    FieldDef::FLOAT64("SQRT2", std::f64::consts::SQRT_2, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Math", JS_PROP_CONFIGURABLE),
];

pub const js_math_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "Math",
    js_math_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
