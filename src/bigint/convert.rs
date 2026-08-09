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
