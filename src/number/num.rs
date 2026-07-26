// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use core::f64;

use crate::{context::ScopedContext, value::JSValue};

use super::{
    IntOrFloat, MAX_SAFE_INTEGER, SAFE_INTEGER_RANGE, ToNumberHintEnum, f64_to_i32, is_safe_integer,
};

impl From<IntOrFloat> for JSValue {
    fn from(value: IntOrFloat) -> Self {
        match value {
            IntOrFloat::Int64(i) => JSValue::from(i),
            IntOrFloat::Float64(f) => JSValue::from(f),
        }
    }
}

impl IntOrFloat {
    pub fn is_integral(&self) -> bool {
        match self {
            Self::Int64(i) => SAFE_INTEGER_RANGE.contains(i),
            Self::Float64(d) => d.is_finite() && d.fract() == 0.0,
        }
    }

    pub fn to_safe_i64(self) -> Option<i64> {
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

    pub fn to_safe_f64(self) -> Option<f64> {
        match self {
            Self::Int64(val) => {
                if SAFE_INTEGER_RANGE.contains(&val) {
                    Some(val as f64)
                } else {
                    None
                }
            }
            Self::Float64(val) => Some(val),
        }
    }

    pub fn to_f64(self) -> f64 {
        match self {
            Self::Int64(i) => {
                #[cfg(debug_assertions)]
                if !SAFE_INTEGER_RANGE.contains(&i) {
                    eprintln!("integer {i} will loose precision");
                }

                i as f64
            }
            Self::Float64(d) => d,
        }
    }

    /// coerce number to i32.
    pub const fn to_i32(&self) -> i32 {
        match self {
            IntOrFloat::Int64(i) => {
                let i = *i;
                if i as i32 as i64 == i {
                    i as i32
                } else {
                    f64_to_i32(i as f64)
                }
            }
            IntOrFloat::Float64(d) => {
                if d.is_nan() {
                    0
                } else {
                    f64_to_i32(*d)
                }
            }
        }
    }

    #[inline(always)]
    pub const fn to_u32(&self) -> u32 {
        self.to_i32() as u32
    }

    pub fn to_i32_saturating(&self) -> i32 {
        match self {
            IntOrFloat::Int64(i) => (*i).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            IntOrFloat::Float64(d) => {
                let d = *d;
                if d.is_nan() {
                    0
                } else if d <= i32::MIN as f64 {
                    i32::MIN
                } else if d >= i32::MAX as f64 {
                    i32::MAX
                } else {
                    d as i32
                }
            }
        }
    }

    pub fn to_i32_clamp(&self, min: i32, max: i32, min_offset: i32) -> i32 {
        let mut res = self.to_i32_saturating();
        if res < min {
            res += min_offset;
            if res < min {
                res = min;
            }
        } else if res > max {
            res = max;
        }
        res
    }

    /// ECMAScript ToUint16 (https://tc39.es/ecma262/#sec-touint16)
    pub fn to_u16(&self) -> u16 {
        let d = self.to_f64();
        if !d.is_finite() {
            0 // NaN, ±∞ → +0
        } else {
            // integral part toward zero, then mod 2^16
            let int = if d >= 0.0 { d.floor() } else { d.ceil() };
            (f64_to_i32(int) as u32 & 0xffff) as u16
        }
    }

    pub fn to_i64_saturating(&self) -> i64 {
        match self {
            IntOrFloat::Int64(i) => *i,
            IntOrFloat::Float64(d) => {
                let d = *d;
                if d.is_nan() {
                    0
                } else if d <= i64::MIN as f64 {
                    i64::MIN
                } else if d >= i64::MAX as f64 {
                    i64::MAX
                } else {
                    d as i64
                }
            }
        }
    }

    /// [min, max] is inclusive range.
    pub fn to_i64_clamp(&self, min: i64, max: i64, neg_offset: i64) -> i64 {
        let mut res = self.to_i64_saturating();
        if res < 0 {
            res += neg_offset;
        }
        if res < min {
            res = min;
        } else if res > max {
            res = max;
        }
        res
    }

    pub fn to_u8_clamped(&self) -> u8 {
        match self {
            IntOrFloat::Int64(i) => (*i).clamp(0, 255) as u8,
            IntOrFloat::Float64(d) => {
                let d = *d;
                if d.is_nan() || d <= 0.0 {
                    0
                } else if d >= 255.0 {
                    255
                } else {
                    f64::round_ties_even(d) as u8
                }
            }
        }
    }

    pub fn add(&self, other: Self) -> Self {
        match (*self, other) {
            (Self::Int64(a), Self::Int64(b)) => {
                let s = a + b;
                if (i64::MIN..=i64::MAX).contains(&s) {
                    Self::Int64(s)
                } else {
                    Self::Float64(s as f64)
                }
            }
            (Self::Float64(a), Self::Float64(b)) => Self::Float64(a + b),
            (a, b) => Self::Float64(a.to_f64() + b.to_f64()),
        }
    }
}

impl From<u32> for JSValue {
    #[inline(always)]
    fn from(val: u32) -> Self {
        if val <= 0x7fffffff {
            Self::from(val as i32)
        } else {
            Self::make_float(val as f64)
        }
    }
}

impl From<i64> for JSValue {
    /// convert an i64 to JSValue.
    ///
    /// if value beyonds \[-2^53, 2^53\] range, precision is not guaranteed.
    fn from(i: i64) -> Self {
        if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
            Self::from(i as i32)
        } else {
            #[cfg(debug_assertions)]
            if !SAFE_INTEGER_RANGE.contains(&i) {
                eprintln!("integer {i} will loose precision");
            }

            Self::make_float(i as f64)
        }
    }
}

impl From<f64> for JSValue {
    fn from(d: f64) -> Self {
        if d == 0.0 && d.is_sign_negative() {
            // JSValue::Float(0.0) means -0.0
            Self::make_float(d)
        } else if d.fract() == 0.0 && d.is_finite() {
            // integral
            let i = d as i64;
            if i as i32 as i64 == i {
                Self::from(i as i32)
            } else {
                Self::make_float(d)
            }
        } else {
            Self::make_float(d)
        }
    }
}

impl JSValue {
    /// test if the value is an intergral number.
    /// returns Err if test has exception.
    pub fn is_integral(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        if !self.is_number() {
            Ok(false)
        } else {
            let d = self.to_number(ctx)?.to_f64();
            Ok(d.is_finite() && d.fract() == 0.0)
        }
    }

    /// Convert value to number.
    ///
    /// Because BigInt is not supported, JSToNumberHintEnum does not effect
    #[cfg(feature = "nan_boxing")]
    fn to_number_internal(
        self,
        ctx: &mut ScopedContext,
        undefined_is_nan: bool,
        _flag: ToNumberHintEnum,
    ) -> Result<IntOrFloat, i32> {
        match self.tag() {
            crate::JSValueTag::Int | crate::JSValueTag::Bool => {
                Ok(IntOrFloat::Int64(self.payload_i32().into()))
            }
            crate::JSValueTag::Float64 => Ok(unsafe { self.as_float_unchecked().into() }),
            crate::JSValueTag::Null => Ok(IntOrFloat::Int64(0)),
            crate::JSValueTag::Undefined => Ok(if undefined_is_nan {
                IntOrFloat::Float64(f64::NAN)
            } else {
                IntOrFloat::Int64(0)
            }),
            crate::JSValueTag::String => unsafe { Ok(self.as_string_unchecked().to_number(ctx)) },
            crate::JSValueTag::Object => unsafe { self.as_object_unchecked().to_number(ctx) },
            crate::JSValueTag::Symbol => {
                ctx.throw_type_error("cannot convert Symbol to number");
                Err(-1)
            }
            crate::JSValueTag::Exception => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                Err(-1)
            }
            _ => Ok(IntOrFloat::Float64(f64::NAN)),
        }
    }

    /// Convert value to number.
    ///
    /// Because BigInt is not supported, JSToNumberHintEnum does not effect
    #[cfg(not(feature = "nan_boxing"))]
    fn to_number_internal(
        self,
        ctx: &mut ScopedContext,
        undefined_is_nan: bool,
        _flag: ToNumberHintEnum,
    ) -> Result<IntOrFloat, i32> {
        match self {
            Self::Int(i) => Ok(i.into()),
            Self::Float64(f) => Ok(f.into()),
            Self::Bool(b) => Ok(IntOrFloat::Int64(b as i64)),
            Self::NULL => Ok(IntOrFloat::Int64(0)),
            Self::UNDEFINED => Ok(if undefined_is_nan {
                IntOrFloat::Float64(f64::NAN)
            } else {
                IntOrFloat::Int64(0)
            }),
            Self::String(s) => Ok(s.to_number(ctx)),
            Self::Object(o) => o.to_number(ctx),
            Self::Symbol(_) => {
                ctx.throw_type_error("cannot convert Symbol to number");
                Err(-1)
            }
            Self::EXCEPTION => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                Err(-1)
            }
            _ => Ok(IntOrFloat::Float64(f64::NAN)),
        }
    }

    /// Convert value to number (Int or Float64, could be NaN).
    #[inline(always)]
    pub fn to_number(&self, ctx: &mut ScopedContext) -> Result<IntOrFloat, i32> {
        self.to_number_internal(ctx, true, ToNumberHintEnum::TON_FLAG_NUMBER)
    }

    /// Convert value to Int, Float64 (could be NaN) value, or Err.
    /// Because BigInt is not supported, this method is same as coerce_to_number()
    #[inline(always)]
    pub fn to_numeric(&self, ctx: &mut ScopedContext) -> Result<IntOrFloat, i32> {
        self.to_number_internal(ctx, true, ToNumberHintEnum::TON_FLAG_NUMERIC)
    }

    /// convert value to length between \[0, MAX_SAFE_INTEGER\].
    pub fn to_length(&self, ctx: &mut ScopedContext) -> Result<i64, i32> {
        self.to_number(ctx)
            .map(|n| n.to_i64_clamp(0, MAX_SAFE_INTEGER, 0))
    }
}
