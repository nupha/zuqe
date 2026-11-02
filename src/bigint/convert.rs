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
///
/// Instead of dividing by `radix` one digit at a time, we divide the limb
/// array by a large single-limb chunk `radix^k` (k = 19 for radix 10, so the
/// chunk is `10^19`), pulling `k` digits out of each division pass. That cuts
/// the number of O(n) limb passes by ~k× compared with per-digit division.
pub(crate) fn to_string_radix(bi: &BigIntRef, radix: u32) -> String {
    debug_assert!((2..=36).contains(&radix));
    let i = bi.as_impl();
    if i.is_zero() {
        return "0".to_string();
    }

    // Largest radix^k that fits a u64 (single-limb divisor) and its digit count.
    let (chunk, chunk_digits) = largest_power_fit_u64(radix);

    // Repeatedly divide the little-endian limb array by `chunk`, collecting
    // remainders (least significant first); each remainder is k digits.
    let mut limbs: Vec<u64> = i.limbs().to_vec();
    let mut chunks: Vec<u64> = Vec::with_capacity(limbs.len() + 2);
    loop {
        let mut rem: u64 = 0;
        let mut all_zero = true;
        for l in limbs.iter_mut().rev() {
            let cur = ((rem as u128) << 64) | *l as u128;
            *l = (cur / chunk as u128) as u64;
            rem = (cur % chunk as u128) as u64;
            if *l != 0 {
                all_zero = false;
            }
        }
        chunks.push(rem);
        if all_zero {
            break;
        }
    }

    let mut s = String::with_capacity(chunks.len() * chunk_digits + 1);
    if i.sign() {
        s.push('-');
    }
    // chunks are little-endian; the most significant chunk is the last one.
    for (idx, &c) in chunks.iter().rev().enumerate() {
        let d = u64_to_digits(c, radix);
        if idx != 0 {
            // zero-pad every chunk except the most significant one.
            for _ in d.len()..chunk_digits {
                s.push('0');
            }
        }
        s.push_str(&d);
    }
    s
}

/// The largest `radix^k` that still fits in a `u64`, plus the digit count `k`.
const fn largest_power_fit_u64(radix: u32) -> (u64, usize) {
    let mut power: u64 = 1;
    let mut digits = 0usize;
    while let Some(p) = power.checked_mul(radix as u64) {
        power = p;
        digits += 1;
    }
    (power, digits)
}

/// Format a `u64` as base-`radix` digits (no leading zeros).
fn u64_to_digits(mut v: u64, radix: u32) -> String {
    debug_assert!((2..=36).contains(&radix));
    if v == 0 {
        return "0".to_string();
    }
    let mut buf = [0u8; 64];
    let mut n = 0usize;
    while v != 0 {
        let d = (v % radix as u64) as u32;
        buf[n] = char::from_digit(d, radix).unwrap() as u8;
        v /= radix as u64;
        n += 1;
    }
    // reverse
    let mut s = String::with_capacity(n);
    for i in (0..n).rev() {
        s.push(buf[i] as char);
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

/// Convert a BigInt to a `f64` (`Number(bigint)`).
///
/// Round to nearest, ties to even, **once**: the significand is the top 53
/// bits plus one round bit and a sticky bit taken from everything below it.
///
/// Folding the limbs through `d * 2^64 + limb` instead rounds at every limb
/// (double rounding) and picks the wrong neighbour near a binade boundary —
/// e.g. `2n**65n - 2n**12n + 2n**11n - 1n` is 2047/4096 of the way up from
/// `2**65 - 2**12` (round down) while the folded form lands on `2**65`.
///
/// A value that rounds up to `2**1024` overflows to ±Infinity, exactly like
/// `Number.MAX_VALUE`'s upper neighbour.
pub(crate) fn to_f64(bi: &BigIntRef) -> f64 {
    let i = bi.as_impl();
    if i.is_zero() {
        return 0.0;
    }
    let limbs = i.limbs();
    let bits = i.bit_length();

    let d = if bits <= 53 {
        // The whole magnitude fits the significand: exact.
        debug_assert_eq!(limbs.len(), 1, "a <= 53-bit value has one limb");
        limbs[0] as f64
    } else if bits > 1024 {
        // Beyond the largest finite binade: no rounding can recover it.
        f64::INFINITY
    } else {
        // Drop every bit below `shift`, keeping 53 of them.
        let shift = bits - 53;
        let mut m: u64 = 0;
        for k in 0..53 {
            if bigint_bit_at(limbs, shift + k) {
                m |= 1 << k;
            }
        }
        let round = bigint_bit_at(limbs, shift - 1);
        if round && (bigint_any_bit_below(limbs, shift - 1) || m & 1 == 1) {
            // Ties go to the even significand; otherwise round up.
            m += 1;
        }
        // `m` may carry into 2^53 (the next binade); scaling by the exact power
        // of two keeps that right, and overflows to +Inf at 2^1024.
        m as f64 * 2f64.powi(shift as i32)
    };

    if i.sign() { -d } else { d }
}

/// Bit `idx` (0 = least significant) of a little-endian limb array.
#[inline]
fn bigint_bit_at(limbs: &[u64], idx: usize) -> bool {
    let limb = idx / 64;
    limb < limbs.len() && (limbs[limb] >> (idx % 64)) & 1 == 1
}

/// Whether any bit below `idx` is set (the sticky bit of a rounding).
#[inline]
fn bigint_any_bit_below(limbs: &[u64], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    let full = idx / 64;
    if limbs[..full.min(limbs.len())].iter().any(|&l| l != 0) {
        return true;
    }
    match limbs.get(full) {
        // Bits [0, idx % 64) of the limb that holds the cut.
        Some(&l) if idx % 64 != 0 => l & ((1u64 << (idx % 64)) - 1) != 0,
        _ => false,
    }
}

/// `ToBigInt64` (BigInt → 64-bit two's complement, wrapping mod 2^64).
///
/// Used by `BigInt64Array` element assignment and `DataView.prototype
/// .setBigInt64` (after ToBigInt).
pub(crate) fn to_bigint64(bi: &BigIntRef) -> i64 {
    to_biguint64(bi) as i64
}

/// `ToBigUint64` (BigInt → 64-bit, wrapping mod 2^64).
///
/// Used by `BigUint64Array` element assignment and `DataView.prototype
/// .setBigUint64` (after ToBigInt).
pub(crate) fn to_biguint64(bi: &BigIntRef) -> u64 {
    let i = bi.as_impl();
    let lo = i.limbs().first().copied().unwrap_or(0);
    if i.sign() { lo.wrapping_neg() } else { lo }
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
    if exp < 0 {
        // value < 2^52: the integer is contained in the top bits of the
        // mantissa (the low `-exp` mantissa bits are fraction bits).
        let v = mantissa >> (-exp);
        super::ops::normalize_vec(vec![v])
    } else {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;

    fn with_ctx(f: impl FnOnce(&mut ScopedContext)) {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt.create_context(|_| Ok(())).unwrap();
        ctx.with_new_scope(|scope| {
            f(scope);
        });
    }

    fn num(scope: &mut ScopedContext, decimal: &str) -> f64 {
        let bi = parse_str(scope, decimal, 10).unwrap();
        to_f64(&bi)
    }

    /// `Number(bigint)` rounds once, to nearest with ties to even. Folding the
    /// limbs (`d * 2^64 + limb`) rounds at every limb instead, which lands on
    /// the wrong neighbour near a binade boundary: this value is 2047/4096 of
    /// the way up from `2^65 - 2^12` and must round **down** to it.
    #[test]
    fn to_f64_rounds_once_near_a_binade_boundary() {
        with_ctx(|scope| {
            // 2^65 - 2^12 + 2^11 - 1
            assert_eq!(
                num(scope, "36893488147419101183"),
                // 2^65 - 2^12, exactly representable (the binade below 2^65
                // has a 2^12 spacing).
                36893488147419099136.0
            );
        });
    }

    /// Ties go to the even significand, in both directions.
    #[test]
    fn to_f64_breaks_ties_to_even() {
        with_ctx(|scope| {
            // 2^53 + 1: between 2^53 (even) and 2^53 + 2 → down.
            assert_eq!(num(scope, "9007199254740993"), 9007199254740992.0);
            // 2^53 + 3: between 2^53 + 2 and 2^53 + 4 → up to the even one.
            assert_eq!(num(scope, "9007199254740995"), 9007199254740996.0);
        });
    }

    /// Overflow: the largest finite value survives, one ulp above the top
    /// binade's midpoint becomes Infinity.
    #[test]
    fn to_f64_saturates_at_the_largest_finite_value() {
        with_ctx(|scope| {
            // 2^1024 - 2^971 == f64::MAX
            assert_eq!(
                num(
                    scope,
                    "179769313486231570814527423731704356798070567525844996598917476803157260780028538760589558632766878171540458953514382464234321326889464182768467546703537516986049910576551282076245490090389328944075868508455133942304583236903222948165808559332123348274797826204144723168738177180919299881250404026184124858368"
                ),
                f64::MAX
            );
            // 2^1024 - 2^970 - 1: just below the midpoint → MAX_VALUE.
            assert_eq!(
                num(
                    scope,
                    "179769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497791"
                ),
                f64::MAX
            );
            // 2^1024 - 2^970: the midpoint, ties to the even 2^1024 → Infinity.
            assert_eq!(
                num(
                    scope,
                    "179769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497792"
                ),
                f64::INFINITY
            );
            // 2^1024 is past the largest finite value.
            assert_eq!(
                num(
                    scope,
                    "179769313486231590772930519078902473361797697894230657273430081157732675805500963132708477322407536021120113879871393357658789768814416622492847430639474124377767893424865485276302219601246094119453082952085005768838150682342462881473913110540827237163350510684586298239947245938479716304835356329624224137216"
                ),
                f64::INFINITY
            );
        });
    }
}
