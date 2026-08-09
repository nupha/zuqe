// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a2d;
mod convert;
mod d2a;
mod num;

pub(crate) use {a2d::*, convert::*, d2a::*};

#[cfg(feature = "big_int")]
use crate::bigint::BigIntRef;

pub(crate) const MAX_SAFE_INTEGER: i64 = (1_i64 << 53) - 1;

/// Integer beyonds range \[-2^53, 2^53\], precision is not guaranteed when converting to f64.
pub(super) const SAFE_INTEGER_RANGE: std::ops::RangeInclusive<i64> =
    -MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER;

#[inline(always)]
pub(crate) fn is_safe_integer(d: f64) -> bool {
    f64::is_finite(d) && d.floor() == d && d.abs() <= MAX_SAFE_INTEGER as f64
}

/// Bit-level reinterpretation of an `f64` as `u64` / two `u32` lanes.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union Float64Union {
    pub d: f64,
    pub uint64: u64,
    pub uint32: [u32; 2],
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IntOrFloat {
    Int64(i64),
    Float64(f64),
}

impl PartialOrd for IntOrFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (IntOrFloat::Int64(a), IntOrFloat::Int64(b)) => a.partial_cmp(b),
            (IntOrFloat::Float64(a), IntOrFloat::Float64(b)) => a.partial_cmp(b),
            (IntOrFloat::Int64(a), IntOrFloat::Float64(b)) => (*a as f64).partial_cmp(b),
            (IntOrFloat::Float64(a), IntOrFloat::Int64(b)) => a.partial_cmp(&(*b as f64)),
        }
    }
}

impl std::fmt::Display for IntOrFloat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(i) = self.to_safe_i64() {
            write!(f, "{}", i)
        } else {
            let s = f64_stringify(self.to_f64(), 10, None, JS_DTOA_VAR_FORMAT);
            write!(f, "{}", s)
        }
    }
}

impl From<i32> for IntOrFloat {
    #[inline(always)]
    fn from(val: i32) -> Self {
        Self::Int64(val as i64)
    }
}

impl From<i64> for IntOrFloat {
    #[inline(always)]
    fn from(val: i64) -> Self {
        Self::Int64(val)
    }
}

impl From<f64> for IntOrFloat {
    fn from(d: f64) -> Self {
        if d == 0.0 {
            if d.is_sign_negative() {
                Self::Float64(-0.0)
            } else {
                Self::Int64(0)
            }
        } else if d.fract() == 0.0 && is_safe_integer(d) {
            Self::Int64(d as i64)
        } else {
            Self::Float64(d)
        }
    }
}

/// this hint is for BigInt only
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToNumberHintEnum {
    TON_FLAG_NUMBER,
    TON_FLAG_NUMERIC,
}

/// Result of numeric literal parsing (`js_aton`) and `ToNumeric`.
///
/// `Float` holds the legacy `IntOrFloat` value; `BigInt` (feature `big_int`)
/// is produced for `n`-suffixed literals. Without the feature the `BigInt`
/// variant does not exist and the enum degrades to a plain `IntOrFloat`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumValue {
    Float(IntOrFloat),
    #[cfg(feature = "big_int")]
    BigInt(BigIntRef),
}

impl From<IntOrFloat> for NumValue {
    #[inline(always)]
    fn from(v: IntOrFloat) -> Self {
        NumValue::Float(v)
    }
}
