// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use super::JSFloat64Union;

/// convert f64 to i32 according to the ECMAScript rules (not rust rules).
pub(crate) const fn f64_to_i32(d: f64) -> i32 {
    let u = JSFloat64Union { d };

    // we avoid doing fmod(x, 2^32)
    let e = unsafe { (u.uint64 >> 52) & 0x7ff } as i32;
    if e <= (1023 + 30) {
        // fast case
        d as i32
    } else if e <= (1023 + 30 + 53) {
        // remainder modulo 2^32
        let mut v = unsafe { (u.uint64 & ((1u64 << 52) - 1)) | (1u64 << 52) };
        v <<= (e - 1023 - 52 + 32) as u32;

        let mut ret = (v >> 32) as i32;
        // take the sign into account
        if unsafe { u.uint64 >> 63 } != 0 {
            ret = ret.wrapping_neg();
        }
        ret
    } else {
        0 // also handles NaN and +inf
    }
}

/// `Math.f16round`: round a Number to the nearest IEEE 754-2019 binary16
/// value (round-to-nearest-even), then return it as a Number.
pub(crate) fn math_f16round(x: f64) -> f64 {
    f16bits_to_f64(f64_to_f16bits(x))
}

/// Convert the bits of an `f64` to the 16-bit pattern of the nearest
/// half-precision float (round-to-nearest-even).
const fn f64_to_f16bits(x: f64) -> u16 {
    let bits = x.to_bits();
    let sign = (((bits >> 63) as u16) << 15) & 0x8000;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let mant = bits & 0x000f_ffff_ffff_ffff;

    if exp == 0x7ff {
        // inf or nan
        if mant == 0 {
            0x7c00 | sign
        } else {
            0x7e00 | sign
        }
    } else if exp == 0 {
        // zero or subnormal f64 -> flush to +/-zero
        sign
    } else {
        let unbiased = exp - 1023;
        let half_bias = unbiased + 15;
        if half_bias >= 0x1f {
            // overflow -> inf
            0x7c00 | sign
        } else if half_bias <= 0 {
            // subnormal half or underflow: h_mant = sig * 2^(unbiased - 28),
            // sig = 1<<52 | mant. This naturally yields 0 (underflow), a
            // subnormal value, or 0x400 (which rounds up to the smallest normal).
            let sig = 0x0010_0000_0000_0000u64 | mant;
            let k = (43 - half_bias) as u32;
            let raw = (sig >> k) as u32;
            let (guard, sticky) = if k >= 64 {
                // sig is only 53 bits; everything falls below the guard position.
                (0u32, sig != 0)
            } else {
                let guard = ((sig >> (k - 1)) & 1) as u32;
                let sticky = (sig & ((1u64 << (k - 1)) - 1)) != 0;
                (guard, sticky)
            };
            let mut h = raw;
            if guard != 0 && (sticky || (h & 1) != 0) {
                h += 1;
            }
            if h >= 0x400 {
                // rounds up to the smallest normal (2^-14)
                sign | 0x0400
            } else {
                sign | h as u16
            }
        } else {
            // normal half
            let m_top = (mant >> 42) as u32;
            let guard = ((mant >> 41) & 1) as u32;
            let sticky = (mant & ((1u64 << 41) - 1)) != 0;
            let mut m = m_top;
            if guard != 0 && (sticky || (m & 1) != 0) {
                m += 1;
            }
            if m >= 0x400 {
                // implicit bit carries into exponent
                let e2 = half_bias + 1;
                if e2 >= 0x1f {
                    return 0x7c00 | sign;
                }
                return sign | (e2 as u16) << 10;
            }
            sign | (half_bias as u16) << 10 | m as u16
        }
    }
}

/// Reconstruct an `f64` from a half-precision bit pattern.
fn f16bits_to_f64(h: u16) -> f64 {
    let sign = (h & 0x8000) != 0;
    let exp = (h >> 10) & 0x1f;
    let mant = (h & 0x3ff) as u64;
    let v = if exp == 0 {
        if mant == 0 {
            0.0
        } else {
            // subnormal: mant * 2^-24
            (mant as f64) * 2f64.powi(-24)
        }
    } else if exp == 0x1f {
        if mant == 0 { f64::INFINITY } else { f64::NAN }
    } else {
        // normal: (1 + mant/2^10) * 2^(exp-15)
        ((1024 + mant) as f64) * 2f64.powi((exp as i32) - 25)
    };
    if sign { -v } else { v }
}
