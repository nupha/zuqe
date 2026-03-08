// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ops::RangeInclusive;

use crate::{context::ScopedContext, error::JSResult, value::JSValue};

pub const MAX_SAFE_INTEGER: i64 = (1_i64 << 53) - 1;
pub const SAFE_INTEGER_RANGE: RangeInclusive<i64> = -MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER;
pub const JS_FLOAT64_NAN: f64 = f64::NAN;

#[inline(always)]
pub(crate) fn is_safe_integer(d: f64) -> bool {
    f64::is_finite(d) && d.floor() == d && d.abs() <= MAX_SAFE_INTEGER as f64
}

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum IntOrFloat {
    Int64(i64),
    Float64(f64),
}

impl From<f64> for IntOrFloat {
    #[inline]
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

impl From<i64> for IntOrFloat {
    #[inline(always)]
    fn from(val: i64) -> Self {
        Self::Int64(val)
    }
}

impl From<IntOrFloat> for JSValue {
    fn from(value: IntOrFloat) -> Self {
        match value {
            IntOrFloat::Int64(i) => JSValue::from(i),
            IntOrFloat::Float64(f) => JSValue::from(f),
        }
    }
}

impl IntOrFloat {
    pub fn to_i64(self) -> Option<i64> {
        match self {
            Self::Int64(val) => Some(val),
            Self::Float64(val) => {
                if is_safe_integer(val) {
                    Some(val as i64)
                } else {
                    None
                }
            }
        }
    }

    pub fn to_f64(self) -> Option<f64> {
        match self {
            IntOrFloat::Int64(val) => {
                if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&val) {
                    Some(val as f64)
                } else {
                    None
                }
            }
            IntOrFloat::Float64(val) => Some(val),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union JSFloat64Union {
    pub d: f64,
    pub uint64: u64,
    pub uint32: [u32; 2],
}

impl From<i32> for JSValue {
    #[inline(always)]
    fn from(val: i32) -> Self {
        Self::Int(val)
    }
}

impl From<u32> for JSValue {
    #[inline(always)]
    fn from(val: u32) -> Self {
        if val <= 0x7fffffff {
            Self::Int(val as i32)
        } else {
            Self::from_f64_as_is(val as f64)
        }
    }
}

impl From<i64> for JSValue {
    /// convert an i64 to JSValue.
    ///
    /// if value beyonds \[-2^53, 2^53\] range, precision is not guaranteed.
    fn from(val: i64) -> Self {
        let v32 = val as i32;
        if val == v32 as i64 {
            Self::Int(v32)
        } else {
            #[cfg(debug_assertions)]
            {
                const PRECISION_RANGE: RangeInclusive<i64> = -(1_i64 << 53)..=1_i64 << 53;
                if !PRECISION_RANGE.contains(&val) {
                    println!("integer {val} will lose precision");
                }
            }

            Self::from_f64_as_is(val as f64)
        }
    }
}

impl From<f64> for JSValue {
    fn from(d: f64) -> Self {
        if d == 0.0 && d.is_sign_negative() {
            // -0
            Self::from_f64_as_is(d)
        } else if d.fract() == 0.0 {
            // integral
            let n = d as i64;
            if (i32::MIN as i64..=i32::MAX as i64).contains(&n) {
                Self::Int(n as i32)
            } else {
                Self::from_f64_as_is(d)
            }
        } else {
            Self::from_f64_as_is(d)
        }
    }
}

impl JSValue {
    /// create Float64 value as-is
    #[cfg(not(feature = "nan_boxing"))]
    #[inline(always)]
    const fn from_f64_as_is(d: f64) -> JSValue {
        JSValue::Float64(d)
    }

    /// create f64 value as-is
    #[cfg(feature = "nan_boxing")]
    #[inline]
    pub(crate) const fn from_f64_as_is(d: f64) -> JSValue {
        let u = Float64Union { d };

        // Normalize NaN
        if (u.u64 & 0x7fffffffffffffff) > 0x7ff0000000000000 {
            JSValue::NAN
        } else {
            JSValue::from_u64(u.u64 - ((super::value::JS_FLOAT64_TAG_ADDEND as u64) << 32))
        }
    }

    /// test if the value is an intergral number.
    /// returns Err if test has exception.
    pub fn is_integral(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        if !self.is_number() {
            Ok(false)
        } else {
            let d = self.to_f64(ctx)?;
            Ok(d.is_finite() && d.fract() == 0.0)
        }
    }

    /// Convert value to number value.
    /// returns JSValue::Int, JSValue::Float64 value, or Err.
    ///
    /// Because BigInt is not supported, JSToNumberHintEnum does not effect
    fn to_num_internal(self, ctx: &mut ScopedContext, _flag: JSToNumberHintEnum) -> JSResult {
        let mut val = self;

        loop {
            match val {
                Self::Int(_) | Self::Float64(_) => {
                    break Ok(val);
                }
                Self::Bool(b) => break Ok(JSValue::Int(b as i32)),
                Self::NULL => {
                    break Ok(JSValue::Int(0));
                }
                Self::UNDEFINED => {
                    break Ok(JSValue::Float64(f64::NAN));
                }
                Self::String(s) => {
                    break Ok(s.to_number(ctx));
                }
                Self::Object(o) => {
                    val = o.to_number(ctx)?;
                    continue;
                }
                #[cfg(feature = "builtin_symbol")]
                Self::Symbol(_) => {
                    ctx.throw_type_error("cannot convert symbol to number");
                    break Err(-1);
                }
                Self::EXCEPTION => {
                    #[cfg(debug_assertions)]
                    unreachable!();
                    #[cfg(not(debug_assertions))]
                    break Err(-1);
                }
                _ => {
                    break Ok(JSValue::Float64(f64::NAN));
                }
            }
        }
    }

    /// Convert value to number Int or Float64 (could be NaN).
    #[inline(always)]
    pub fn to_number(&self, ctx: &mut ScopedContext) -> Result<JSValue, i32> {
        self.to_num_internal(ctx, JSToNumberHintEnum::TON_FLAG_NUMBER)
    }

    /// Convert value to Int, Float64 (could be NaN) value, or Err.
    /// Because BigInt is not supported, this method is same as coerce_to_number()
    #[inline(always)]
    pub fn to_numeric(&self, ctx: &mut ScopedContext) -> Result<JSValue, i32> {
        self.to_num_internal(ctx, JSToNumberHintEnum::TON_FLAG_NUMERIC)
    }

    pub fn coerce_to_u8_clamp(&self, ctx: &mut ScopedContext) -> Result<u8, i32> {
        let mut val = *self;

        let res: i32 = loop {
            match val {
                JSValue::NULL | JSValue::UNDEFINED => {
                    break 0;
                }
                JSValue::Int(i) => {
                    break i.clamp(0, 255);
                }
                JSValue::Bool(b) => {
                    break b as i32;
                }
                JSValue::Float64(d) => {
                    break if d.is_nan() || d < 0.0 {
                        0
                    } else if d > 255.0 {
                        255
                    } else {
                        f64::round_ties_even(d) as i32
                    };
                }
                JSValue::EXCEPTION => {
                    return Err(-1);
                }
                _ => {
                    val = val.to_number(ctx)?;
                }
            }
        };

        Ok(res as u8)
    }

    /// coerce value to i32.
    pub fn to_i32(&self, ctx: &mut ScopedContext) -> Result<i32, i32> {
        let mut val = *self;

        let ret: i32 = loop {
            match val {
                Self::UNDEFINED | Self::NULL => {
                    break 0;
                }
                Self::Int(i) => {
                    break i;
                }
                Self::Bool(b) => {
                    break b as i32;
                }
                Self::Float64(d) => {
                    break js_f64_to_i32(d);
                }
                Self::EXCEPTION => {
                    return Err(-1);
                }
                _ => {
                    val = val.to_number(ctx)?;
                }
            }
        };

        Ok(ret)
    }

    // Note: the integer value is saturated to 32 bits
    pub fn to_i32_saturating(&self, ctx: &mut ScopedContext) -> Result<i32, i32> {
        let mut val = *self;

        let ret: i32 = loop {
            match val {
                JSValue::NULL | JSValue::UNDEFINED => {
                    break 0;
                }
                JSValue::Int(i) => {
                    break i;
                }
                JSValue::Bool(b) => {
                    break b as i32;
                }
                JSValue::Float64(d) => {
                    break if d.is_nan() {
                        0
                    } else if d < i32::MIN as f64 {
                        i32::MIN
                    } else if d > i32::MAX as f64 {
                        i32::MAX
                    } else {
                        d as i32
                    };
                }

                JSValue::EXCEPTION => {
                    #[cfg(debug_assertions)]
                    unreachable!();
                    #[cfg(not(debug_assertions))]
                    return Err(-1);
                }
                _ => {
                    val = val.to_number(ctx)?;
                }
            }
        };

        Ok(ret)
    }

    pub fn to_i32_clamp(
        &self,
        ctx: &mut ScopedContext,
        min: i32,
        max: i32,
        min_offset: i32,
    ) -> Result<i32, i32> {
        let mut res = self.to_i32_saturating(ctx)?;
        if res < min {
            res += min_offset;
            if res < min {
                res = min;
            }
        } else if res > max {
            res = max;
        }
        Ok(res)
    }

    #[inline(always)]
    pub fn to_u32(&self, ctx: &mut ScopedContext) -> Result<u32, i32> {
        self.to_i32(ctx).map(|v| v as u32)
    }

    pub fn to_i64_saturating(&self, ctx: &mut ScopedContext) -> Result<i64, i32> {
        let mut val = *self;

        let res: i64 = loop {
            match val {
                JSValue::NULL | JSValue::UNDEFINED => {
                    break 0;
                }
                JSValue::Int(i) => {
                    break i as i64;
                }
                JSValue::Bool(b) => {
                    break b as i64;
                }
                JSValue::Float64(d) => {
                    break if d.is_nan() {
                        0
                    } else if d < i64::MIN as f64 {
                        i64::MIN
                    } else if d > i64::MAX as f64 {
                        i64::MAX
                    } else {
                        d as i64
                    };
                }
                JSValue::EXCEPTION => {
                    return Err(-1);
                }
                _ => {
                    val = val.to_number(ctx)?;
                }
            }
        };

        Ok(res)
    }

    /// [min, max] is inclusive range.
    pub fn to_i64_clamp(
        &self,
        ctx: &mut ScopedContext,
        min: i64,
        max: i64,
        neg_offset: i64,
    ) -> Result<i64, i32> {
        let mut res = self.to_i64_saturating(ctx)?;
        if res < 0 {
            res += neg_offset;
        }
        if res < min {
            res = min;
        } else if res > max {
            res = max;
        }
        Ok(res)
    }

    /// coerce value to f64.
    pub fn to_f64(&self, ctx: &mut ScopedContext) -> Result<f64, i32> {
        match self {
            JSValue::Int(i) => Ok(*i as f64),
            JSValue::Bool(b) => Ok(*b as i32 as f64),
            JSValue::Float64(n) => Ok(*n),
            JSValue::NULL => Ok(0.0),
            JSValue::UNDEFINED => Ok(f64::NAN),

            #[cfg(debug_assertions)]
            JSValue::EXCEPTION => {
                unreachable!()
            }

            _ => match self.to_number(ctx)? {
                JSValue::Int(v) => Ok(v as f64),
                JSValue::Float64(v) => Ok(v),
                _ => {
                    unreachable!()
                }
            },
        }
    }

    /// convert value to length between \[0, MAX_SAFE_INTEGER\].
    #[inline]
    pub fn to_length(&self, ctx: &mut ScopedContext) -> Result<i64, i32> {
        self.to_i64_clamp(ctx, 0, MAX_SAFE_INTEGER, 0)
    }
}

/// this hint is for BigInt only
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum JSToNumberHintEnum {
    TON_FLAG_NUMBER,
    TON_FLAG_NUMERIC,
}

pub const fn JS_NumberIsNegativeOrMinusZero(val: &JSValue) -> bool {
    match val {
        JSValue::Int(v) => *v < 0,
        JSValue::Float64(d) => {
            let u = JSFloat64Union { d: *d };
            unsafe { (u.uint64 >> 63) != 0 }
        }
        _ => false,
    }
}

/// convert f64 to i32 according to the ECMAScript rules (not rust rules).
pub(crate) const fn js_f64_to_i32(d: f64) -> i32 {
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
