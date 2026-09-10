// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! The `BigInt` builtin: constructor, static `asIntN`/`asUintN`, and the
//! prototype methods `toString` / `toLocaleString` / `valueOf` /
//! `Symbol.toStringTag`. Mirrors the `js_number_*` family.

use std::cmp::Ordering;

use crate::{
    ScopedContext,
    bigint::BigIntRef,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::JS_PROP_CONFIGURABLE,
    value::{HINT_NUMBER, JSValue},
};

/// Strict `ToBigInt(value)`: BigInt → itself, Boolean → 0n/1n, String →
/// StringToBigInt (SyntaxError when invalid), Object → ToPrimitive then
/// recurse, and **Number / Symbol / undefined / null → TypeError**. This is
/// the conversion used by `BigInt.asIntN` / `asUintN` and by the BigInt
/// typed-array / DataView element writers (`BigInt64Array[i] = v`,
/// `DataView.prototype.setBigInt64`).
pub(crate) fn to_big_int(ctx: &mut ScopedContext, val: JSValue) -> JSResult {
    match val.tag() {
        crate::JSValueTag::BigInt => Ok(val),
        crate::JSValueTag::Bool => {
            let v = if val.payload_i32() != 0 { 1 } else { 0 };
            Ok(crate::bigint::from_u64(ctx, v).map(JSValue::make_bigint)?)
        }
        crate::JSValueTag::Int | crate::JSValueTag::Float64 => {
            Err(ctx.throw_type_error("Cannot convert a Number to a BigInt"))
        }
        crate::JSValueTag::String => {
            let s = unsafe { val.as_string_unchecked() };
            let text = s.content().encode_utf8(false);
            match crate::bigint::string_to_bigint_decimal(ctx, &text) {
                Some(b) => Ok(JSValue::make_bigint(b)),
                None => Err(ctx.throw_syntax_error("Cannot convert a string to a BigInt")),
            }
        }
        crate::JSValueTag::Object => {
            let p = val.to_primitive(ctx, HINT_NUMBER)?;
            to_big_int(ctx, p)
        }
        _ => Err(ctx.throw_type_error("Cannot convert undefined, null or Symbol to BigInt")),
    }
}

/// `NumberToBigInt`: converts a Number to its exact integer BigInt value;
/// throws a RangeError when the Number is not an integer or is not finite.
fn number_to_big_int(ctx: &mut ScopedContext, d: f64) -> Result<BigIntRef, i32> {
    if !d.is_finite() || d.fract() != 0.0 {
        return Err(ctx.throw_range_error(
            "The number cannot be converted to a BigInt because it isn't an integer",
        ));
    }
    crate::bigint::from_f64(ctx, d)
}

pub fn js_bigint_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if !cc.this().is_undefined() {
        return Err(ctx.throw_type_error("BigInt is not a constructor"));
    }
    // BigInt(value): ToPrimitive, then Numbers go through NumberToBigInt
    // (integers accepted), everything else through ToBigInt.
    let prim = cc.arg(0).to_primitive(ctx, HINT_NUMBER)?;
    match prim.tag() {
        crate::JSValueTag::Int => Ok(
            crate::bigint::from_i64(ctx, prim.payload_i32() as i64).map(JSValue::make_bigint)?
        ),
        crate::JSValueTag::Float64 => Ok(JSValue::make_bigint(number_to_big_int(ctx, unsafe {
            prim.as_float_unchecked()
        })?)),
        _ => to_big_int(ctx, prim),
    }
}

/// Extract the BigInt primitive from `this` (a BigInt primitive or a BigInt
/// wrapper object).
fn js_thisBigIntValue(ctx: &mut ScopedContext, this_val: JSValue) -> Result<BigIntRef, i32> {
    if let Some(bi) = this_val.as_bigint() {
        Ok(bi)
    } else if let Some(o) = this_val.as_object() {
        o.payload_bigint()
            .copied()
            .ok_or_else(|| ctx.throw_type_error("not a BigInt"))
    } else {
        Err(ctx.throw_type_error("not a BigInt"))
    }
}

fn js_bigint_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let to_locale_string = cc.magic() != 0;
    let bi = js_thisBigIntValue(ctx, cc.this())?;

    let radix = if to_locale_string || cc.arg(0).is_undefined() {
        10
    } else {
        let r = cc.arg(0).to_number(ctx)?.to_i32_saturating();
        if !(2..=36).contains(&r) {
            return Err(ctx.throw_range_error("radix must be between 2 and 36"));
        }
        r as u32
    };

    let s = crate::bigint::to_string_radix(&bi, radix);
    ctx.create_string_from_utf8(s.into_bytes())
        .map(JSValue::make_string)
}

#[cfg(feature = "intl402")]
fn js_bigint_toLocaleString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = js_thisBigIntValue(ctx, cc.this())?;
    let data =
        crate::builtins::intl::number_format::build_number_format_data(ctx, cc.arg(0), cc.arg(1))?;
    let s =
        crate::builtins::intl::number_format::format_value(ctx, &data, JSValue::make_bigint(val))?;
    ctx.create_string_from_utf8(s.into_bytes())
        .map(JSValue::make_string)
}

fn js_bigint_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    Ok(JSValue::make_bigint(js_thisBigIntValue(ctx, cc.this())?))
}

/// `2^bits` as a BigInt (RangeError when too large to be practical).
fn pow2(ctx: &mut ScopedContext, bits: u64) -> Result<BigIntRef, i32> {
    if bits > crate::bigint::MAX_BIGINT_BITS {
        return Err(ctx.throw_range_error("Maximum BigInt size exceeded"));
    }
    BigIntRef::create(ctx, false, crate::bigint::shl_mag(&[1], bits))
}

/// Euclidean `a mod 2^bits` (result in `[0, 2^bits)`).
fn mod_2pow(ctx: &mut ScopedContext, a: &BigIntRef, bits: u64) -> Result<BigIntRef, i32> {
    let ai = a.as_impl();
    if !ai.sign() && bits > ai.bit_length() as u64 {
        return Ok(*a); // positive value already in range
    }
    let modulus = pow2(ctx, bits)?;
    let (_, r) = crate::bigint::divmod_mag(ai.limbs(), modulus.as_impl().limbs());
    let r = BigIntRef::create(ctx, false, r)?;
    if ai.sign() {
        if r.is_zero() {
            Ok(r)
        } else {
            BigIntRef::sub(ctx, &modulus, &r)
        }
    } else {
        Ok(r)
    }
}

fn js_bigint_asIntN(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let bits = cc.arg(0).to_index(ctx)? as u64;
    let bi = to_big_int(ctx, cc.arg(1))?.as_bigint().unwrap();
    let m = mod_2pow(ctx, &bi, bits)?;
    if bits == 0 {
        return Ok(JSValue::make_bigint(m)); // 0 mod 1 = 0
    }
    // Signed interpretation: values at or above 2^(bits-1) wrap negative.
    let half = pow2(ctx, bits - 1)?;
    if m.cmp(&half) != Ordering::Less {
        let modulus = pow2(ctx, bits)?;
        Ok(BigIntRef::sub(ctx, &m, &modulus).map(JSValue::make_bigint)?)
    } else {
        Ok(JSValue::make_bigint(m))
    }
}

fn js_bigint_asUintN(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let bits = cc.arg(0).to_index(ctx)? as u64;
    let bi = to_big_int(ctx, cc.arg(1))?.as_bigint().unwrap();
    Ok(JSValue::make_bigint(mod_2pow(ctx, &bi, bits)?))
}

pub const js_bigint_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_bigint_toString, 0),
    #[cfg(feature = "intl402")]
    FieldDef::FUNC("toLocaleString", 0, js_bigint_toLocaleString, 0),
    FieldDef::FUNC("valueOf", 0, js_bigint_valueOf, 0),
    FieldDef::STR("[Symbol.toStringTag]", "BigInt", JS_PROP_CONFIGURABLE),
];

pub const js_bigint_funcs: &[FieldDef] = &[
    FieldDef::FUNC("asIntN", 2, js_bigint_asIntN, 0),
    FieldDef::FUNC("asUintN", 2, js_bigint_asUintN, 0),
];
