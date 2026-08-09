// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Conversion helpers between `BigIntRef` and digit strings / fixed-width ints.
//!
//! Phase 1 only needs the literal-parsing path (`parse_str`, used by the `n`
//! suffix in `js_aton`) plus exact narrowing used by the bytecode emitter.
//! Wider conversion surface (`to_string`, `to_f64`, `from_*`) arrives in
//! Phase 2.

use crate::{context::ScopedContext, number::to_digit};

use super::BigIntRef;

/// Parse a BigInt from a digit string, allowing a single leading `-` sign
/// (the sign is stripped and tracked separately, matching the sign-magnitude
/// representation). `radix` must be in `2..=36`.
///
/// The result is allocated in the current gc scope.
pub(crate) fn parse_str(ctx: &mut ScopedContext, txt: &str, radix: u32) -> Result<BigIntRef, i32> {
    debug_assert!((2..=36).contains(&radix));
    debug_assert!(!txt.is_empty());

    let (sign, digits) = match txt.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, txt),
    };

    let radix64 = radix as u64;
    let mut limbs: Vec<u64> = Vec::new();

    for &b in digits.as_bytes() {
        let d = to_digit(b);
        debug_assert!(d < radix as i32, "invalid digit {b} for radix {radix}");
        let mut carry = d as u128;
        for limb in limbs.iter_mut() {
            let v = *limb as u128 * radix64 as u128 + carry;
            *limb = v as u64;
            carry = v >> 64;
        }
        if carry != 0 {
            limbs.push(carry as u64);
        }
    }

    BigIntRef::create(ctx, sign, limbs)
}

/// Parse a string as a BigInt (StringToBigInt): surrounding whitespace is
/// ignored, an empty string is `0n`, and the body must be a signed integer
/// literal — optionally with `0x`/`0o`/`0b` prefixes — with no fractional
/// part, exponent, suffix, or other junk. Parsing is exact (no f64 rounding).
/// Returns `None` when the string is not a valid integer literal.
pub(crate) fn string_to_bigint(ctx: &mut ScopedContext, s: &str) -> Option<BigIntRef> {
    let t = s.trim();
    if t.is_empty() {
        return from_u64(ctx, 0).ok();
    }
    let (neg, rest) = if let Some(r) = t.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = t.strip_prefix('+') {
        (false, r)
    } else {
        (false, t)
    };
    let (radix, digits) =
        if let Some(d) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            (16u32, d)
        } else if let Some(d) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
            (8u32, d)
        } else if let Some(d) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
            (2u32, d)
        } else {
            (10u32, rest)
        };
    if digits.is_empty() || !digits.bytes().all(|b| (to_digit(b) as u32) < radix) {
        return None;
    }
    let signed = if neg {
        format!("-{digits}")
    } else {
        digits.to_string()
    };
    parse_str(ctx, &signed, radix).ok()
}

/// StringToBigInt per spec: surrounding whitespace is ignored, an empty string
/// is `0n`. A **signed** body must be a decimal integer literal (no radix
/// prefix); an **unsigned** body may be a decimal integer or a `0x`/`0o`/`0b`
/// prefixed literal. No fractional part, exponent, suffix, or other junk.
/// Exact (no f64 rounding). Returns `None` when invalid. Used by `==` and the
/// BigInt constructor.
pub(crate) fn string_to_bigint_decimal(ctx: &mut ScopedContext, s: &str) -> Option<BigIntRef> {
    let t = s.trim();
    if t.is_empty() {
        return from_u64(ctx, 0).ok();
    }
    if t.starts_with('-') || t.starts_with('+') {
        let (neg, digits) = if let Some(r) = t.strip_prefix('-') {
            (true, r)
        } else {
            (false, &t[1..])
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let signed = if neg {
            format!("-{digits}")
        } else {
            digits.to_string()
        };
        parse_str(ctx, &signed, 10).ok()
    } else {
        string_to_bigint(ctx, t)
    }
}

/// Exact conversion to `i64` if the magnitude fits, otherwise `None`.
pub(crate) fn to_i64_exact(bi: &BigIntRef) -> Option<i64> {
    let i = bi.as_impl();
    if i.is_zero() {
        return Some(0);
    }
    if i.limbs.len() > 1 {
        return None;
    }
    let v = i.limbs[0];
    if !i.sign {
        // magnitude in [1, i64::MAX]
        if v <= i64::MAX as u64 {
            Some(v as i64)
        } else {
            None
        }
    } else {
        // magnitude in [1, 2^63]
        if v <= 1u64 << 63 {
            if v == 1u64 << 63 {
                Some(i64::MIN)
            } else {
                Some(-(v as i64))
            }
        } else {
            None
        }
    }
}

/// Render a BigInt as a digit string in the given radix (2..=36), with a
/// leading `-` for negatives. Zero is `"0"`. Used for ToString and for
/// ToPropertyKey (BigInt property names are the decimal string, no `n`).
pub(crate) fn to_string_radix(bi: &BigIntRef, radix: u32) -> String {
    debug_assert!((2..=36).contains(&radix));
    let i = bi.as_impl();
    if i.is_zero() {
        return "0".to_string();
    }

    // Repeatedly divide the little-endian limb array by `radix`, collecting
    // remainders (least significant first).
    let mut limbs: Vec<u64> = i.limbs().to_vec();
    let mut digits: Vec<u8> = Vec::with_capacity(limbs.len() * 19 + 1);
    loop {
        let mut rem: u128 = 0;
        let mut q_limbs: Vec<u64> = Vec::with_capacity(limbs.len());
        let mut all_zero = true;
        // iterate most significant limb first
        for &limb in limbs.iter().rev() {
            let cur = (rem << 64) | limb as u128;
            let q = (cur / radix as u128) as u64;
            rem = cur % radix as u128;
            if q != 0 {
                all_zero = false;
            }
            q_limbs.push(q);
        }
        digits.push(rem as u8);
        if all_zero {
            break;
        }
        q_limbs.reverse(); // back to little-endian
        limbs = q_limbs;
    }

    let mut s = String::with_capacity(digits.len() + 1);
    if i.sign() {
        s.push('-');
    }
    for &d in digits.iter().rev() {
        s.push(char::from_digit(d as u32, radix).expect("digit in range"));
    }
    s
}

/// Exact conversion to `u128` if the magnitude fits, otherwise `None`.
pub(crate) fn to_u128_exact(bi: &BigIntRef) -> Option<u128> {
    let i = bi.as_impl();
    if i.is_zero() {
        return Some(0);
    }
    if i.limbs.len() > 2 {
        return None;
    }
    if i.limbs.len() == 1 {
        Some(i.limbs[0] as u128)
    } else {
        Some((i.limbs[1] as u128) << 64 | i.limbs[0] as u128)
    }
}

/// Convert a BigInt to a `f64`. Precision loss for magnitudes beyond 2^53 is
/// allowed by the spec (BigInt→Number coercion). Huge values saturate to ±Inf.
pub(crate) fn to_f64(bi: &BigIntRef) -> f64 {
    let i = bi.as_impl();
    if i.is_zero() {
        return 0.0;
    }
    // fold from the most significant limb; use the exact power so large
    // magnitudes overflow to ±Inf instead of losing the sign.
    let mut d = 0.0f64;
    for &l in i.limbs().iter().rev() {
        d = d * 18446744073709551616.0 + l as f64;
    }
    if i.sign() {
        -d
    } else {
        d
    }
}

/// Create a BigInt from an `i64` in the current gc scope.
pub(crate) fn from_i64(ctx: &mut ScopedContext, v: i64) -> Result<BigIntRef, i32> {
    if v >= 0 {
        BigIntRef::create(ctx, false, vec![v as u64])
    } else if v == i64::MIN {
        BigIntRef::create(ctx, true, vec![1u64 << 63])
    } else {
        BigIntRef::create(ctx, true, vec![(-v) as u64])
    }
}

/// Create a BigInt from a `u64` in the current gc scope.
pub(crate) fn from_u64(ctx: &mut ScopedContext, v: u64) -> Result<BigIntRef, i32> {
    BigIntRef::create(ctx, false, vec![v])
}

/// Create a BigInt from a `f64`. Only finite integers are accepted; a
/// non-integer (or non-finite) value throws a RangeError (ToBigInt semantics).
pub(crate) fn from_f64(ctx: &mut ScopedContext, d: f64) -> Result<BigIntRef, i32> {
    if !d.is_finite() || d.fract() != 0.0 {
        return Err(ctx.throw_range_error("the number cannot be converted to a BigInt"));
    }
    let neg = d.is_sign_negative();
    let mag = if d == 0.0 {
        Vec::new()
    } else {
        f64_integer_to_limbs(d.abs())
    };
    BigIntRef::create(ctx, neg, mag)
}

/// Convert a finite, non-negative integral `f64` to normalized little-endian
/// limbs (empty for zero). `d` must satisfy `d.fract() == 0.0`.
pub(crate) fn f64_integer_to_limbs(d: f64) -> Vec<u64> {
    debug_assert!(d.is_finite() && d >= 0.0 && d.fract() == 0.0);
    if d == 0.0 {
        return Vec::new();
    }
    let bits = d.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32 - 1023 - 52; // value = mantissa * 2^exp
    let mantissa = (bits & 0x000f_ffff_ffff_ffff) | (1 << 52);
    debug_assert!(exp >= 0, "non-integer f64 passed to f64_integer_to_limbs");
    let mut limbs = vec![0u64; (exp / 64) as usize];
    let rem = (exp % 64) as u32;
    if rem == 0 {
        limbs.push(mantissa);
    } else {
        limbs.push(mantissa << rem);
        let high = mantissa >> (64 - rem);
        if high != 0 {
            limbs.push(high);
        }
    }
    super::ops::normalize_vec(limbs)
}
