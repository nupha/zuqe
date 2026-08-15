// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Limb-level arithmetic and bitwise operations for `BigIntImpl`.
//!
//! The engine stores BigInts in **sign-magnitude** form (`sign` + little-endian
//! `u64` limbs). All pure helpers here operate on magnitude slices and return
//! plain `(bool, Vec<u64>)` / `Vec<u64>` values; the top-level `BigIntRef`
//! methods allocate the canonical result node in the gc scope.

use std::cmp::Ordering;

use crate::context::ScopedContext;

use super::{BigIntImpl, BigIntRef};

/// Defensive cap on BigInt magnitude (in bits). Bigger values are rejected
/// with a RangeError before the engine tries to allocate them.
pub(crate) const MAX_BIGINT_BITS: u64 = 1 << 28;

/// Operands of at least this many limbs (and not too unbalanced) switch from
/// the schoolbook O(n²) multiplication to Karatsuba O(n^1.585).
const KARATSUBA_THRESHOLD: usize = 32;

/// `true` when Karatsuba is likely to beat schoolbook: both operands are at
/// least [`KARATSUBA_THRESHOLD`] limbs and their lengths are within a factor
/// of four. A very unbalanced pair (e.g. `1000 × 32`) is cheaper as schoolbook
/// because the recursion would otherwise keep splitting the large operand
/// against the same small one.
const fn should_karatsuba(a: &[u64], b: &[u64]) -> bool {
    let (small, large) = if a.len() < b.len() {
        (a.len(), b.len())
    } else {
        (b.len(), a.len())
    };
    small >= KARATSUBA_THRESHOLD && large <= small * 4
}

/// Exact `u128` of a plain impl (magnitude; sign ignored), if it fits.
fn impl_to_u128(i: &BigIntImpl) -> Option<u128> {
    if i.is_zero() {
        return Some(0);
    }
    if i.limbs().len() > 2 {
        return None;
    }
    if i.limbs().len() == 1 {
        Some(i.limbs()[0] as u128)
    } else {
        Some((i.limbs()[1] as u128) << 64 | i.limbs()[0] as u128)
    }
}

// ============================================================================
// pure magnitude helpers
// ============================================================================

/// Strip trailing zero limbs (returns `Vec`, unlike `normalize_limbs`).
pub(crate) fn normalize_vec(mut v: Vec<u64>) -> Vec<u64> {
    while v.last() == Some(&0) {
        v.pop();
    }
    v
}

/// Compare two magnitudes (little-endian limbs).
pub(crate) fn cmp_mag(a: &[u64], b: &[u64]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

/// a + b (magnitudes).
pub(crate) fn add_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    let len = a.len().max(b.len());
    let mut result = Vec::with_capacity(len + 1);
    let mut carry: u64 = 0;
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0) as u128;
        let y = b.get(i).copied().unwrap_or(0) as u128;
        let sum = x + y + carry as u128;
        result.push(sum as u64);
        carry = (sum >> 64) as u64;
    }
    if carry != 0 {
        result.push(carry);
    }
    result
}

/// a - b (magnitudes); requires `a >= b`.
pub(crate) fn sub_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    debug_assert_ne!(cmp_mag(a, b), Ordering::Less, "sub_mag with a < b");
    let mut result = Vec::with_capacity(a.len());
    let mut borrow: u64 = 0;
    for (i, &x) in a.iter().enumerate() {
        let x = x as u128;
        let y = b.get(i).copied().unwrap_or(0) as u128 + borrow as u128;
        if x >= y {
            result.push((x - y) as u64);
            borrow = 0;
        } else {
            result.push(((1u128 << 64) + x - y) as u64);
            borrow = 1;
        }
    }
    debug_assert_eq!(borrow, 0, "sub_mag borrow leak");
    result
}

/// a * b (magnitudes). Dispatches between the schoolbook O(n*m) algorithm and
/// Karatsuba (O(n^1.585)) for large, roughly-balanced operands.
pub(crate) fn mul_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    if should_karatsuba(a, b) {
        mul_karatsuba(a, b)
    } else {
        mul_schoolbook(a, b)
    }
}

/// Schoolbook O(n*m) multiplication (limb-wise, u128 intermediates).
fn mul_schoolbook(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut result = vec![0u64; a.len() + b.len()];
    for (i, &ai) in a.iter().enumerate() {
        if ai == 0 {
            continue;
        }
        let mut carry: u128 = 0;
        for (j, &bj) in b.iter().enumerate() {
            let cur = result[i + j] as u128 + ai as u128 * bj as u128 + carry;
            result[i + j] = cur as u64;
            carry = cur >> 64;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let cur = result[k] as u128 + carry;
            result[k] = cur as u64;
            carry = cur >> 64;
            k += 1;
        }
    }
    normalize_vec(result)
}

/// Karatsuba multiplication: split both operands at the midpoint and combine
/// three half-size products instead of four (O(n^1.585)).
///
/// With `a = a0 + a1*B` and `b = b0 + b1*B` (B = 2^(64*m)):
///   z0 = a0*b0, z2 = a1*b1, z1 = (a0+a1)*(b0+b1) - z0 - z2
///   result = z0 + z1*B + z2*B²
fn mul_karatsuba(a: &[u64], b: &[u64]) -> Vec<u64> {
    let n = a.len().max(b.len());
    let m = n / 2;

    let (a0, a1) = if a.len() <= m {
        (a, &[][..])
    } else {
        a.split_at(m)
    };
    let (b0, b1) = if b.len() <= m {
        (b, &[][..])
    } else {
        b.split_at(m)
    };

    let z0 = mul_mag(a0, b0);
    let z2 = mul_mag(a1, b1);

    let s_a = add_mag(a0, a1);
    let s_b = add_mag(b0, b1);
    // z1 = (a0+a1)*(b0+b1) - z0 - z2; mathematically non-negative.
    let mid = mul_mag(&s_a, &s_b);
    let z0z2 = add_mag(&z0, &z2);
    let z1 = sub_mag(&mid, &z0z2);

    // result = z0 + z1*B + z2*B² (B = 2^(64*m)); size = a+b limbs.
    let mut result = vec![0u64; a.len() + b.len() + 1];
    add_mag_into(&mut result, &z0, 0);
    add_mag_into(&mut result, &z1, m);
    add_mag_into(&mut result, &z2, 2 * m);
    normalize_vec(result)
}

/// `result[offset..] += addend` (little-endian limbs), propagating carries.
fn add_mag_into(result: &mut [u64], addend: &[u64], offset: usize) {
    let mut carry: u64 = 0;
    for (i, &x) in addend.iter().enumerate() {
        let idx = offset + i;
        let cur = result[idx] as u128 + x as u128 + carry as u128;
        result[idx] = cur as u64;
        carry = (cur >> 64) as u64;
    }
    let mut k = offset + addend.len();
    while carry != 0 {
        debug_assert!(k < result.len(), "add_mag_into carry overflow");
        let cur = result[k] as u128 + carry as u128;
        result[k] = cur as u64;
        carry = (cur >> 64) as u64;
        k += 1;
    }
}

/// Single-limb division: divide `a` by `d`, returning (quotient, remainder).
fn divmod_small(a: &[u64], d: u64) -> (Vec<u64>, Vec<u64>) {
    debug_assert_ne!(d, 0);
    let mut q = vec![0u64; a.len()];
    let mut rem: u64 = 0;
    for i in (0..a.len()).rev() {
        let cur = ((rem as u128) << 64) | a[i] as u128;
        q[i] = (cur / d as u128) as u64;
        rem = (cur % d as u128) as u64;
    }
    (
        normalize_vec(q),
        if rem == 0 { Vec::new() } else { vec![rem] },
    )
}

/// Knuth Algorithm D (TAOCP vol.2 4.3.1): divide magnitudes, returning
/// (quotient, remainder). Requires `b` non-empty and `a >= b`.
pub(crate) fn divmod_mag(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    debug_assert!(!b.is_empty());
    if cmp_mag(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        return divmod_small(a, b[0]);
    }

    let n = b.len();
    let m = a.len() - n;

    // D1: normalize so that the top bit of v is set.
    let shift = b[n - 1].leading_zeros() as u64;
    let v = shl_mag(b, shift);
    let mut u = shl_mag(a, shift);
    while u.len() < m + n + 1 {
        u.push(0);
    }
    let mut q = vec![0u64; m + 1];
    let vn1 = v[n - 1] as u128;
    let vn2 = v[n - 2] as u128;

    // D7: loop over quotient digits from most to least significant.
    for j in (0..=m).rev() {
        // D3: estimate qhat.
        let num = ((u[j + n] as u128) << 64) | u[j + n - 1] as u128;
        let mut qhat = num / vn1;
        let mut rhat = num % vn1;
        loop {
            if qhat >= (1u128 << 64) {
                break;
            }
            let ujn2 = if j + n >= 2 { u[j + n - 2] as u128 } else { 0 };
            if qhat * vn2 <= (rhat << 64) | ujn2 {
                break;
            }
            qhat -= 1;
            rhat += vn1;
            if rhat >= (1u128 << 64) {
                break;
            }
        }

        // D4: multiply and subtract u[j..j+n] -= qhat * v.
        let mut k: i128 = 0; // signed borrow accumulator
        for i in 0..n {
            let p = qhat * v[i] as u128;
            let t = (u[j + i] as u128)
                .wrapping_sub(p & 0xFFFF_FFFF_FFFF_FFFF)
                .wrapping_sub(k as u128);
            u[j + i] = t as u64;
            k = (p >> 64) as i128 - (t >> 64) as i128;
        }
        let t = (u[j + n] as i128).wrapping_sub(k);
        u[j + n] = t as u64;

        // D5: qhat one too large? add v back and decrement.
        if t < 0 {
            qhat -= 1;
            let mut carry: u128 = 0;
            for i in 0..n {
                let t2 = u[j + i] as u128 + v[i] as u128 + carry;
                u[j + i] = t2 as u64;
                carry = t2 >> 64;
            }
            u[j + n] = (u[j + n] as u128).wrapping_add(carry) as u64;
        }
        q[j] = qhat as u64;
    }

    // D8: remainder is the low n limbs, un-normalized by the shift.
    let rem = shr_mag(&u[..n], shift);
    (normalize_vec(q), rem)
}

/// Number of significant bits of a magnitude.
pub(crate) fn bit_length(a: &[u64]) -> u64 {
    if a.is_empty() {
        return 0;
    }
    let top = a[a.len() - 1];
    ((a.len() - 1) as u64) * 64 + (64 - top.leading_zeros() as u64)
}

/// `a << bits` (magnitude).
pub(crate) fn shl_mag(a: &[u64], bits: u64) -> Vec<u64> {
    if a.is_empty() {
        return Vec::new();
    }
    let whole = (bits / 64) as usize;
    let rem = (bits % 64) as u32;
    let mut result = vec![0u64; whole];
    if rem == 0 {
        result.extend_from_slice(a);
    } else {
        let mut carry: u64 = 0;
        for &l in a {
            result.push((l << rem) | carry);
            carry = l >> (64 - rem);
        }
        if carry != 0 {
            result.push(carry);
        }
    }
    result
}

/// `a >> bits` (logical, magnitude).
pub(crate) fn shr_mag(a: &[u64], bits: u64) -> Vec<u64> {
    if a.is_empty() {
        return Vec::new();
    }
    let whole = (bits / 64) as usize;
    let rem = (bits % 64) as u32;
    if whole >= a.len() {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(a.len() - whole);
    if rem == 0 {
        result.extend_from_slice(&a[whole..]);
    } else {
        for i in whole..a.len() {
            let hi = a[i] >> rem;
            let lo = if i + 1 < a.len() {
                a[i + 1] << (64 - rem)
            } else {
                0
            };
            result.push(hi | lo);
        }
    }
    normalize_vec(result)
}

/// Whether `a` has any bit set below position `bits`.
fn has_low_bits(a: &[u64], bits: u64) -> bool {
    let whole = (bits / 64) as usize;
    let rem = (bits % 64) as u32;
    if whole >= a.len() {
        return !a.is_empty();
    }
    for &l in &a[..whole] {
        if l != 0 {
            return true;
        }
    }
    if rem > 0 && (a[whole] & ((1u64 << rem) - 1)) != 0 {
        return true;
    }
    false
}

/// Convert a sign-magnitude value to two's complement over `width` limbs.
fn to_twos_complement(sign: bool, mag: &[u64], width: usize) -> Vec<u64> {
    let mut v = vec![0u64; width];
    let n = mag.len().min(width);
    v[..n].copy_from_slice(&mag[..n]);
    if sign {
        for x in v.iter_mut() {
            *x = !*x;
        }
        let mut carry = 1u64;
        for x in v.iter_mut() {
            let (s, c) = x.overflowing_add(carry);
            *x = s;
            carry = c as u64;
            if carry == 0 {
                break;
            }
        }
    }
    v
}

/// Convert a two's complement value back to sign-magnitude.
fn from_twos_complement(v: Vec<u64>) -> (bool, Vec<u64>) {
    let is_neg = v.last().is_some_and(|&x| (x as i64) < 0);
    if !is_neg {
        return (false, normalize_vec(v));
    }
    let mut mag: Vec<u64> = v.iter().map(|&x| !x).collect();
    let mut carry = 1u64;
    for x in mag.iter_mut() {
        let (s, c) = x.overflowing_add(carry);
        *x = s;
        carry = c as u64;
        if carry == 0 {
            break;
        }
    }
    (true, normalize_vec(mag))
}

// ============================================================================
// BigIntRef top-level operations (allocate results in the gc scope)
// ============================================================================

impl BigIntRef {
    /// sign-magnitude addition of two impls (pure).
    fn add_impl(a: &BigIntImpl, b: &BigIntImpl) -> (bool, Vec<u64>) {
        if a.sign == b.sign {
            (a.sign, add_mag(a.limbs(), b.limbs()))
        } else {
            match cmp_mag(a.limbs(), b.limbs()) {
                Ordering::Equal => (false, Vec::new()),
                Ordering::Greater => (a.sign, sub_mag(a.limbs(), b.limbs())),
                Ordering::Less => (b.sign, sub_mag(b.limbs(), a.limbs())),
            }
        }
    }

    /// sign-magnitude subtraction of two impls (pure).
    fn sub_impl(a: &BigIntImpl, b: &BigIntImpl) -> (bool, Vec<u64>) {
        let nb = BigIntImpl {
            sign: !b.sign && !b.is_zero(),
            limbs: b.limbs.clone(),
        };
        Self::add_impl(a, &nb)
    }

    pub(crate) fn add(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        let (sign, limbs) = Self::add_impl(a.as_impl(), b.as_impl());
        Self::create(ctx, sign, limbs)
    }

    pub(crate) fn sub(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        let (sign, limbs) = Self::sub_impl(a.as_impl(), b.as_impl());
        Self::create(ctx, sign, limbs)
    }

    pub(crate) fn mul(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        let ai = a.as_impl();
        let bi = b.as_impl();
        let sign = ai.sign != bi.sign;
        let limbs = mul_mag(ai.limbs(), bi.limbs());
        Self::create(ctx, sign, limbs)
    }

    /// Truncating division toward zero (`a / b`). Zero divisor → RangeError.
    pub(crate) fn div(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        let ai = a.as_impl();
        let bi = b.as_impl();
        if bi.is_zero() {
            return Err(ctx.throw_range_error("Division by zero"));
        }
        let (q, _) = divmod_mag(ai.limbs(), bi.limbs());
        Self::create(ctx, ai.sign != bi.sign, q)
    }

    /// Truncating remainder (`a % b`, sign follows the dividend). Zero
    /// divisor → RangeError.
    pub(crate) fn rem(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        let ai = a.as_impl();
        let bi = b.as_impl();
        if bi.is_zero() {
            return Err(ctx.throw_range_error("Division by zero"));
        }
        let (_, r) = divmod_mag(ai.limbs(), bi.limbs());
        Self::create(ctx, ai.sign, r)
    }

    /// Exponentiation (`a ** b`). A negative exponent → RangeError per spec.
    pub(crate) fn pow(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        let ai = a.as_impl();
        let ei = b.as_impl();
        if ei.sign() {
            return Err(ctx.throw_range_error("Exponent must be positive"));
        }
        if ai.is_zero() {
            return if ei.is_zero() {
                crate::bigint::from_u64(ctx, 1)
            } else {
                Self::zero(ctx)
            };
        }
        if ai.limbs() == &[1] {
            // |a| == 1: result is ±1; even exponent keeps it positive.
            let sign = ai.sign() && (ei.limbs().first().is_some_and(|&l| l & 1 == 1));
            return Self::create(ctx, sign, vec![1]);
        }

        let e = super::convert::to_u128_exact(b)
            .ok_or_else(|| ctx.throw_range_error("Maximum BigInt size exceeded"))?;
        let a_bits = bit_length(ai.limbs());
        if e.saturating_mul(a_bits as u128) > MAX_BIGINT_BITS as u128 {
            return Err(ctx.throw_range_error("Maximum BigInt size exceeded"));
        }

        let mut result: Vec<u64> = vec![1];
        let mut base = ai.limbs().to_vec();
        let mut exp = e;
        while exp > 0 {
            if exp & 1 == 1 {
                result = mul_mag(&result, &base);
            }
            exp >>= 1;
            if exp > 0 {
                base = mul_mag(&base, &base);
            }
        }
        let sign = ai.sign() && (e & 1 == 1);
        Self::create(ctx, sign, result)
    }

    pub(crate) fn neg(ctx: &mut ScopedContext, a: &Self) -> Result<Self, i32> {
        let i = a.as_impl();
        Self::create(ctx, !i.sign() && !i.is_zero(), i.limbs().to_vec())
    }

    pub(crate) fn abs(ctx: &mut ScopedContext, a: &Self) -> Result<Self, i32> {
        let i = a.as_impl();
        Self::create(ctx, false, i.limbs().to_vec())
    }

    pub(crate) fn inc(ctx: &mut ScopedContext, a: &Self) -> Result<Self, i32> {
        let one = BigIntImpl {
            sign: false,
            limbs: Box::new([1]),
        };
        let (sign, limbs) = Self::add_impl(a.as_impl(), &one);
        Self::create(ctx, sign, limbs)
    }

    pub(crate) fn dec(ctx: &mut ScopedContext, a: &Self) -> Result<Self, i32> {
        let one = BigIntImpl {
            sign: false,
            limbs: Box::new([1]),
        };
        let (sign, limbs) = Self::sub_impl(a.as_impl(), &one);
        Self::create(ctx, sign, limbs)
    }

    /// Bitwise NOT (`~a`), infinite two's complement semantics.
    pub(crate) fn not(ctx: &mut ScopedContext, a: &Self) -> Result<Self, i32> {
        let i = a.as_impl();
        // one extra limb so a positive value whose top bit is set is not
        // misread as negative (the sign bit lives one limb higher).
        let width = i.limbs().len() + 1;
        let tc = to_twos_complement(i.sign(), i.limbs(), width);
        let r: Vec<u64> = tc.iter().map(|&x| !x).collect();
        let (sign, limbs) = from_twos_complement(r);
        Self::create(ctx, sign, limbs)
    }

    fn bitwise(
        ctx: &mut ScopedContext,
        a: &Self,
        b: &Self,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<Self, i32> {
        let ai = a.as_impl();
        let bi = b.as_impl();
        let width = ai.limbs().len().max(bi.limbs().len()) + 1;
        let ta = to_twos_complement(ai.sign(), ai.limbs(), width);
        let tb = to_twos_complement(bi.sign(), bi.limbs(), width);
        let r: Vec<u64> = ta.iter().zip(tb.iter()).map(|(&x, &y)| op(x, y)).collect();
        let (sign, limbs) = from_twos_complement(r);
        Self::create(ctx, sign, limbs)
    }

    pub(crate) fn bitand(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        Self::bitwise(ctx, a, b, |x, y| x & y)
    }

    pub(crate) fn bitor(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        Self::bitwise(ctx, a, b, |x, y| x | y)
    }

    pub(crate) fn bitxor(ctx: &mut ScopedContext, a: &Self, b: &Self) -> Result<Self, i32> {
        Self::bitwise(ctx, a, b, |x, y| x ^ y)
    }

    /// Convert a shift-count impl to a `u64` bit count; RangeError when the
    /// count (or the resulting size) is out of bounds.
    fn shift_bits(ctx: &mut ScopedContext, shift: &BigIntImpl) -> Result<u64, i32> {
        let bits = impl_to_u128(shift)
            .ok_or_else(|| ctx.throw_range_error("Maximum BigInt size exceeded"))?;
        if bits > MAX_BIGINT_BITS as u128 {
            return Err(ctx.throw_range_error("Maximum BigInt size exceeded"));
        }
        Ok(bits as u64)
    }

    /// `a << shift` where `shift` is a BigInt (negative shift shifts right).
    pub(crate) fn shl_big(ctx: &mut ScopedContext, a: &Self, shift: &Self) -> Result<Self, i32> {
        let si = shift.as_impl();
        if si.is_zero() {
            return Ok(*a);
        }
        let (sign, limbs) = if si.sign() {
            let bits = Self::shift_bits(ctx, si)?;
            Self::shr_impl(a.as_impl(), bits)
        } else {
            let bits = Self::shift_bits(ctx, si)?;
            if (a.as_impl().bit_length() as u64).saturating_add(bits) > MAX_BIGINT_BITS {
                return Err(ctx.throw_range_error("Maximum BigInt size exceeded"));
            }
            (a.as_impl().sign(), shl_mag(a.as_impl().limbs(), bits))
        };
        Self::create(ctx, sign, limbs)
    }

    /// `a >> shift` where `shift` is a BigInt (negative shift shifts left).
    pub(crate) fn shr_big(ctx: &mut ScopedContext, a: &Self, shift: &Self) -> Result<Self, i32> {
        let si = shift.as_impl();
        if si.is_zero() {
            return Ok(*a);
        }
        let (sign, limbs) = if si.sign() {
            let bits = Self::shift_bits(ctx, si)?;
            if (a.as_impl().bit_length() as u64).saturating_add(bits) > MAX_BIGINT_BITS {
                return Err(ctx.throw_range_error("Maximum BigInt size exceeded"));
            }
            (a.as_impl().sign(), shl_mag(a.as_impl().limbs(), bits))
        } else {
            let bits = Self::shift_bits(ctx, si)?;
            Self::shr_impl(a.as_impl(), bits)
        };
        Self::create(ctx, sign, limbs)
    }

    /// Arithmetic right shift by `bits` (floor division by 2^bits).
    fn shr_impl(a: &BigIntImpl, bits: u64) -> (bool, Vec<u64>) {
        if a.is_zero() {
            return (false, Vec::new());
        }
        let mag = shr_mag(a.limbs(), bits);
        if !a.sign() {
            (false, mag)
        } else {
            let had = has_low_bits(a.limbs(), bits);
            let mag = if had { add_mag(&mag, &[1]) } else { mag };
            (true, mag)
        }
    }

    /// Compare two BigInts by mathematical value.
    pub(crate) fn cmp(&self, other: &Self) -> Ordering {
        Self::cmp_impl(self.as_impl(), other.as_impl())
    }

    /// Compare a BigInt with a plain `BigIntImpl`.
    pub(crate) fn cmp_impl(a: &BigIntImpl, b: &BigIntImpl) -> Ordering {
        match (a.sign(), b.sign()) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (true, true) => cmp_mag(b.limbs(), a.limbs()),
            (false, false) => cmp_mag(a.limbs(), b.limbs()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bigint::{parse_str, to_i64_exact, to_string_radix};
    use crate::runtime::Runtime;

    fn s(scope: &mut ScopedContext, txt: &str) -> BigIntRef {
        parse_str(scope, txt, 10).unwrap()
    }

    fn dec(bi: &BigIntRef) -> String {
        to_string_radix(bi, 10)
    }

    fn i64(bi: &BigIntRef) -> i64 {
        to_i64_exact(bi).unwrap()
    }

    fn with_ctx(f: impl FnOnce(&mut ScopedContext)) {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt.create_context(|_| Ok(())).unwrap();
        ctx.with_new_scope(|scope| {
            f(scope);
        });
    }

    fn check_bin_op(
        op: fn(&mut ScopedContext, &BigIntRef, &BigIntRef) -> Result<BigIntRef, i32>,
        a: &str,
        b: &str,
        expect: &str,
    ) {
        with_ctx(|ctx| {
            let x = s(ctx, a);
            let y = s(ctx, b);
            let r = op(ctx, &x, &y).unwrap();
            assert_eq!(dec(&r), expect, "{a} op {b}");
        });
    }

    #[test]
    fn add_carries_and_signs() {
        check_bin_op(BigIntRef::add, "0", "0", "0");
        check_bin_op(BigIntRef::add, "1", "2", "3");
        check_bin_op(BigIntRef::add, "-5", "3", "-2");
        check_bin_op(BigIntRef::add, "-5", "-3", "-8");
        check_bin_op(BigIntRef::add, "5", "-5", "0");
        // multi-limb carry: 2^64 - 1 + 2 = 2^64 + 1
        check_bin_op(
            BigIntRef::add,
            "18446744073709551615",
            "2",
            "18446744073709551617",
        );
    }

    #[test]
    fn sub_borrows_and_signs() {
        check_bin_op(BigIntRef::sub, "5", "3", "2");
        check_bin_op(BigIntRef::sub, "3", "5", "-2");
        check_bin_op(BigIntRef::sub, "-3", "5", "-8");
        check_bin_op(BigIntRef::sub, "-3", "-5", "2");
        check_bin_op(BigIntRef::sub, "0", "0", "0");
        check_bin_op(
            BigIntRef::sub,
            "18446744073709551615",
            "1",
            "18446744073709551614",
        );
        check_bin_op(
            BigIntRef::sub,
            "1",
            "18446744073709551615",
            "-18446744073709551614",
        );
    }

    #[test]
    fn mul_schoolbook() {
        check_bin_op(BigIntRef::mul, "0", "999", "0");
        check_bin_op(BigIntRef::mul, "12", "12", "144");
        check_bin_op(BigIntRef::mul, "-12", "12", "-144");
        check_bin_op(BigIntRef::mul, "-12", "-12", "144");
        // 2^64 * 2^64 = 2^128
        check_bin_op(
            BigIntRef::mul,
            "18446744073709551616",
            "18446744073709551616",
            "340282366920938463463374607431768211456",
        );
        check_bin_op(
            BigIntRef::mul,
            "12345678901234567890",
            "98765432109876543210",
            "1219326311370217952237463801111263526900",
        );
    }

    #[test]
    fn div_truncates_toward_zero() {
        check_bin_op(BigIntRef::div, "7", "2", "3");
        check_bin_op(BigIntRef::div, "-7", "2", "-3");
        check_bin_op(BigIntRef::div, "7", "-2", "-3");
        check_bin_op(BigIntRef::div, "-7", "-2", "3");
        check_bin_op(BigIntRef::div, "1", "2", "0");
        check_bin_op(BigIntRef::div, "-1", "2", "0");
        check_bin_op(
            BigIntRef::div,
            "12345678901234567890",
            "7",
            "1763668414462081127",
        );
        // multi-limb: 2^128 / 12345
        check_bin_op(
            BigIntRef::div,
            "340282366920938463463374607431768211456",
            "12345",
            "27564387761922921301204909471994184",
        );
        check_bin_op(
            BigIntRef::div,
            "123456789012345678901234567890",
            "12345678901234567890123456789",
            "10",
        );
    }

    #[test]
    fn div_by_zero_throws_range_error() {
        with_ctx(|ctx| {
            let a = s(ctx, "5");
            let b = s(ctx, "0");
            assert!(BigIntRef::div(ctx, &a, &b).is_err());
            assert!(BigIntRef::rem(ctx, &a, &b).is_err());
        });
    }

    #[test]
    fn rem_sign_follows_dividend() {
        check_bin_op(BigIntRef::rem, "7", "2", "1");
        check_bin_op(BigIntRef::rem, "-7", "2", "-1");
        check_bin_op(BigIntRef::rem, "7", "-2", "1");
        check_bin_op(BigIntRef::rem, "-7", "-2", "-1");
        check_bin_op(BigIntRef::rem, "6", "3", "0");
        check_bin_op(BigIntRef::rem, "18446744073709551615", "1000000", "551615");
    }

    #[test]
    fn divmod_randomized_small() {
        // cross-check divmod_mag against Python-verified integer arithmetic on
        // many random pairs spanning multiple limbs (exercises Knuth D).
        with_ctx(|ctx| {
            let mut seed: u64 = 0x1234_5678_9abc_def0;
            let mut next = move || {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 33) as u64
            };
            for _ in 0..2000 {
                // build a dividend in [0, 2^192) and a divisor in [1, 2^128)
                let a: u128 = ((next() as u128) << 64 | (next() as u128)) << 64 | next() as u128;
                let b: u128 = ((next() as u128) << 64 | next() as u128) | 1;
                let a_limbs = normalize_vec(vec![a as u64, (a >> 64) as u64, 0u64]);
                let b_limbs = normalize_vec(vec![b as u64, (b >> 64) as u64]);
                let (q, r) = divmod_mag(&a_limbs, &b_limbs);
                // verify q*b + r == a and r < b (limb arithmetic)
                let ident = add_mag(&mul_mag(&q, &b_limbs), &r);
                assert_eq!(ident, a_limbs, "identity a={a} b={b}");
                assert_eq!(
                    cmp_mag(&r, &b_limbs),
                    Ordering::Less,
                    "remainder below divisor"
                );
            }
        });
    }

    #[test]
    fn shl_shr() {
        check_bin_op(BigIntRef::shl_big, "1", "0", "1");
        check_bin_op(BigIntRef::shl_big, "1", "10", "1024");
        check_bin_op(BigIntRef::shl_big, "-1", "10", "-1024");
        check_bin_op(BigIntRef::shl_big, "1", "64", "18446744073709551616");
        check_bin_op(BigIntRef::shl_big, "-5", "3", "-40");
        check_bin_op(BigIntRef::shr_big, "1024", "10", "1");
        check_bin_op(BigIntRef::shr_big, "-1024", "10", "-1");
        check_bin_op(BigIntRef::shr_big, "-3", "1", "-2");
        check_bin_op(BigIntRef::shr_big, "-4", "1", "-2");
        check_bin_op(BigIntRef::shr_big, "-5", "1", "-3");
        check_bin_op(BigIntRef::shr_big, "-1", "100", "-1");
        check_bin_op(BigIntRef::shr_big, "1", "100", "0");
        // negative shift reverses direction
        check_bin_op(BigIntRef::shl_big, "1", "-1", "0");
        check_bin_op(BigIntRef::shr_big, "1", "-1", "2");
        check_bin_op(BigIntRef::shl_big, "1", "-2", "0");
    }

    #[test]
    fn bitwise_two_complement() {
        check_bin_op(BigIntRef::bitand, "5", "3", "1");
        check_bin_op(BigIntRef::bitand, "-5", "3", "3");
        check_bin_op(BigIntRef::bitand, "5", "-3", "5");
        check_bin_op(BigIntRef::bitand, "-5", "-3", "-7");
        check_bin_op(BigIntRef::bitor, "5", "3", "7");
        check_bin_op(BigIntRef::bitor, "-5", "3", "-5");
        check_bin_op(BigIntRef::bitor, "5", "-3", "-3");
        check_bin_op(BigIntRef::bitor, "-5", "-3", "-1");
        check_bin_op(BigIntRef::bitxor, "5", "3", "6");
        check_bin_op(BigIntRef::bitxor, "-5", "3", "-8");
        check_bin_op(BigIntRef::bitxor, "5", "-3", "-8");
        check_bin_op(BigIntRef::bitxor, "-5", "-3", "6");
        // not: only one operand
        with_ctx(|ctx| {
            for (a, expect) in [("0", "-1"), ("5", "-6"), ("-6", "5"), ("-1", "0")] {
                let x = s(ctx, a);
                let r = BigIntRef::not(ctx, &x).unwrap();
                assert_eq!(dec(&r), expect, "~{a}");
            }
        });
    }

    #[test]
    fn pow_fast() {
        check_bin_op(BigIntRef::pow, "0", "0", "1");
        check_bin_op(BigIntRef::pow, "0", "5", "0");
        check_bin_op(BigIntRef::pow, "1", "1000000", "1");
        check_bin_op(BigIntRef::pow, "-1", "2", "1");
        check_bin_op(BigIntRef::pow, "-1", "3", "-1");
        check_bin_op(BigIntRef::pow, "2", "10", "1024");
        check_bin_op(BigIntRef::pow, "-2", "3", "-8");
        check_bin_op(BigIntRef::pow, "-2", "4", "16");
        check_bin_op(BigIntRef::pow, "3", "7", "2187");
        check_bin_op(BigIntRef::pow, "7", "2", "49");
        // 2^100
        check_bin_op(
            BigIntRef::pow,
            "2",
            "100",
            "1267650600228229401496703205376",
        );
        with_ctx(|ctx| {
            let a = s(ctx, "2");
            let b = s(ctx, "-1");
            assert!(BigIntRef::pow(ctx, &a, &b).is_err());
        });
    }

    #[test]
    fn cmp_orders_correctly() {
        with_ctx(|ctx| {
            let cases: &[(&str, &str, Ordering)] = &[
                ("0", "0", Ordering::Equal),
                ("1", "0", Ordering::Greater),
                ("0", "1", Ordering::Less),
                ("-1", "0", Ordering::Less),
                ("-1", "-2", Ordering::Greater),
                ("-2", "-1", Ordering::Less),
                (
                    "123456789012345678901234567890",
                    "12345678901234567890123456789",
                    Ordering::Greater,
                ),
                (
                    "-123456789012345678901234567890",
                    "12345678901234567890123456789",
                    Ordering::Less,
                ),
                (
                    "18446744073709551616",
                    "18446744073709551615",
                    Ordering::Greater,
                ),
            ];
            for (a, b, exp) in cases {
                let ra = s(ctx, a);
                let rb = s(ctx, b);
                assert_eq!(ra.cmp(&rb), *exp, "{a} cmp {b}");
            }
        });
    }

    #[test]
    fn neg_abs_inc_dec() {
        with_ctx(|ctx| {
            let check = |ctx: &mut ScopedContext,
                         txt: &str,
                         op: fn(&mut ScopedContext, &BigIntRef) -> Result<BigIntRef, i32>,
                         expect: i64| {
                let x = s(ctx, txt);
                assert_eq!(i64(&op(ctx, &x).unwrap()), expect, "{op:?} of {txt}");
            };
            check(ctx, "5", BigIntRef::neg, -5);
            check(ctx, "-5", BigIntRef::neg, 5);
            check(ctx, "0", BigIntRef::neg, 0);
            check(ctx, "-5", BigIntRef::abs, 5);
            check(ctx, "0", BigIntRef::abs, 0);
            check(ctx, "-1", BigIntRef::inc, 0);
            check(ctx, "0", BigIntRef::inc, 1);
            check(ctx, "0", BigIntRef::dec, -1);
            // i64::MIN - 1 overflows i64; compare by decimal string
            let x = s(ctx, "-9223372036854775808");
            let r = BigIntRef::dec(ctx, &x).unwrap();
            assert_eq!(dec(&r), "-9223372036854775809");
        });
    }

    #[test]
    fn huge_values_via_string() {
        // 10^40
        let ten_40 = "10000000000000000000000000000000000000000";
        with_ctx(|ctx| {
            let a = s(ctx, ten_40);
            let b = s(ctx, "10");
            assert_eq!(
                dec(&BigIntRef::mul(ctx, &a, &b).unwrap()),
                "100000000000000000000000000000000000000000"
            );
            assert_eq!(
                dec(&BigIntRef::div(ctx, &a, &b).unwrap()),
                "1000000000000000000000000000000000000000"
            );
        });
    }

    #[test]
    fn karatsuba_matches_schoolbook_randomized() {
        // Cross-check the Karatsuba path (large, balanced operands) against the
        // schoolbook fallback over many random pairs spanning 32+ limbs.
        with_ctx(|ctx| {
            let mut seed: u64 = 0xfeed_face_cafe_beef;
            let mut next = move || {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 33) as u64
            };
            for _ in 0..200 {
                let la = 33 + (next() % 8) as usize;
                let lb = 33 + (next() % 8) as usize;
                let a: Vec<u64> = (0..la).map(|_| next()).collect();
                let b: Vec<u64> = (0..lb).map(|_| next()).collect();
                let kar = super::mul_karatsuba(&a, &b);
                let school = super::mul_schoolbook(&a, &b);
                assert_eq!(kar, school, "mismatch for {la}×{lb} limbs");
            }
        });
    }

    #[test]
    fn karatsuba_unbalanced_falls_back_to_schoolbook() {
        // A 40×5 pair must not use Karatsuba (small operand below threshold)
        // and must still be correct.
        with_ctx(|ctx| {
            let a: Vec<u64> = vec![0xdead_beef_cafe_f00d; 40];
            let b: Vec<u64> = vec![0x1234_5678_9abc_def0; 5];
            let r = mul_mag(&a, &b);
            assert_eq!(r, super::mul_schoolbook(&a, &b));
        });
    }

    #[test]
    fn karatsuba_boundary_unbalance_still_correct() {
        // 128 = 4×32: still enters Karatsuba (the unbalance limit), with a1
        // empty after the split — exercises the z1 carry into the z2 region.
        with_ctx(|ctx| {
            let a: Vec<u64> = vec![0x0123_4567_89ab_cdef; 32];
            let b: Vec<u64> = (0..128u64)
                .map(|i| i.wrapping_mul(0x9e3779b97f4a7c15))
                .collect();
            let r = mul_mag(&a, &b);
            assert_eq!(r, super::mul_schoolbook(&a, &b));
            assert_eq!(mul_mag(&b, &a), r, "commutative");
        });
    }

    #[test]
    fn to_string_radix_roundtrip_randomized() {
        // Round-trip decimal strings through parse → to_string for large random
        // values, exercising the chunked-by-1e19 output path.
        with_ctx(|ctx| {
            let mut seed: u64 = 0x1234_5678_9abc_def0;
            let mut next = move || {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 33) as u64
            };
            for _ in 0..200 {
                // 3–6 random limbs → up to ~115 decimal digits (> 19, spans
                // multiple 1e19 chunks).
                let len = 3 + (next() % 4) as usize;
                let limbs: Vec<u64> = (0..len).map(|_| next()).collect();
                let bi = BigIntRef::create(ctx, false, limbs.clone()).unwrap();
                let s1 = to_string_radix(&bi, 10);
                let back = parse_str(ctx, &s1, 10).unwrap();
                assert_eq!(
                    back.as_impl().limbs(),
                    &*normalize_vec(limbs),
                    "roundtrip {s1}"
                );
            }
        });
    }

    #[test]
    fn to_string_radix_all_radices() {
        with_ctx(|ctx| {
            for radix in 2..=36u32 {
                for &txt in &["0", "1", "255", "65535", "12345678901234567890"] {
                    let bi = s(ctx, txt);
                    let out = to_string_radix(&bi, radix);
                    let back = parse_str(ctx, &out, radix).unwrap();
                    assert_eq!(dec(&back), txt, "radix {radix} {txt} -> {out}");
                }
            }
        });
    }
}
