use std::ops::RangeInclusive;

use crate::{
    context::JSContext,
    error::JSResult,
    value::{HINT_NUMBER, JSValue, JSValueConst},
};

use super::{ATOD_ACCEPT_BIN_OCT, js_atof};

pub const MAX_SAFE_INTEGER: i64 = (1_i64 << 53) - 1;
pub const SAFE_INTEGER_RANGE: RangeInclusive<i64> = -MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER;
pub const JS_FLOAT64_NAN: f64 = f64::NAN;

#[inline(always)]
pub(crate) fn is_safe_integer(d: f64) -> bool {
    f64::is_finite(d) && d.floor() == d && d.abs() <= MAX_SAFE_INTEGER as f64
}

/// TODO: use this enum to represent an int or float.
/// XXX
#[derive(Debug, Clone, Copy)]
pub enum IntOrFloat {
    Int32(i32),
    Float64(f64),
}

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

// impl TryFrom<i64> for JSValue {
//     type Error = i32;

//     /// try convert an i64 to JSValue.
//     /// if integer is beyond safe integer range, will raise Err.
//     fn try_from(val: i64) -> Result<Self, Self::Error> {
//         let v32 = val as i32;
//         if val == v32 as i64 {
//             Ok(Self::Int(v32))
//         } else if SAFE_INTEGER_RANGE.contains(&val) {
//             Ok(Self::from_f64_as_is(val as f64))
//         } else {
//             #[cfg(debug_assertions)]
//             log::warn!("integer {val} will lose precision");
//             Err(-1)
//         }
//     }
// }

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
                    log::warn!("integer {val} will lose precision");
                }
            }

            Self::from_f64_as_is(val as f64)
        }
    }
}

impl From<f64> for JSValue {
    fn from(d: f64) -> Self {
        if d.fract() == 0.0 {
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
    pub fn is_integral(&self, ctx: &JSContext) -> Result<bool, i32> {
        if !self.is_number() {
            Ok(false)
        } else {
            let d = self.to_f64(ctx)?;
            Ok(d.is_finite() && d.fract() == 0.0)
        }
    }

    /// Convert value to number value.
    /// returns JSValue::Int, JSValue::Float64 (could be NaN) value, or Err.
    ///
    /// Because BigInt is not supported, JSToNumberHintEnum does not effect
    fn to_num_internal(&self, ctx: &JSContext, flag: JSToNumberHintEnum) -> JSResult {
        let mut val = self.clone();

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
                    let str = s.content().to_utf8();
                    let str = str.as_ref().trim();

                    let res: JSValue = if str.is_empty() {
                        JSValue::Int(0)
                    } else {
                        // FIXME: don't use raw pointer to parse number.
                        let mut p = str.as_ptr();
                        if let Ok(num) =
                            unsafe { js_atof(ctx, p.cast(), &mut p, 0, ATOD_ACCEPT_BIN_OCT) }
                        {
                            if unsafe { p.offset_from(str.as_ptr()) != str.len() as isize } {
                                // `str` has remaining characters that are not part of the number.
                                // This is not a valid number.
                                JSValue::Float64(f64::NAN)
                            } else {
                                num
                            }
                        } else {
                            JSValue::Float64(f64::NAN)
                        }
                    };
                    break Ok(res);
                }
                Self::Symbol(_) => {
                    ctx.throw_type_error("cannot convert symbol to number");
                    break Err(-1);
                }
                Self::Object(o) => {
                    val = o.to_number(ctx)?;
                    continue;
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
    pub fn to_number(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.to_num_internal(ctx, JSToNumberHintEnum::TON_FLAG_NUMBER)
    }

    /// Convert value to Int, Float64 (could be NaN) value, or Err.
    /// Because BigInt is not supported, this method is same as coerce_to_number()
    #[inline(always)]
    pub fn to_numeric(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.to_num_internal(ctx, JSToNumberHintEnum::TON_FLAG_NUMERIC)
    }

    pub fn coerce_to_u8_clamp(&self, ctx: &JSContext) -> Result<u8, i32> {
        let mut val = self.clone();

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

    /// convert value to i32.
    pub fn to_i32(&self, ctx: &JSContext) -> Result<i32, i32> {
        let mut val = self.clone();

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
                    let u = JSFloat64Union { d };

                    // we avoid doing fmod(x, 2^32)
                    let e = unsafe { (u.uint64 >> 52) & 0x7ff } as i32;
                    break if e <= (1023 + 30) {
                        // fast case
                        d as i32
                    } else if e <= (1023 + 30 + 53) {
                        /* remainder modulo 2^32 */
                        let mut v = unsafe { (u.uint64 & ((1u64 << 52) - 1)) | (1u64 << 52) };
                        v <<= (e - 1023 - 52 + 32) as u32;

                        let mut ret = (v >> 32) as i32;
                        // take the sign into account
                        if unsafe { u.uint64 >> 63 } != 0 {
                            ret = -ret;
                        }
                        ret
                    } else {
                        0 // also handles NaN and +inf
                    };
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
    pub fn to_i32_saturating(&self, ctx: &JSContext) -> Result<i32, i32> {
        let mut val = self.clone();

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
        ctx: &JSContext,
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
    pub fn to_u32(&self, ctx: &JSContext) -> Result<u32, i32> {
        self.to_i32(ctx).map(|v| v as u32)
    }

    pub fn to_i64_saturating(&self, ctx: &JSContext) -> Result<i64, i32> {
        let mut val = self.clone();

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
                    } else {
                        if d < i64::MIN as f64 {
                            i64::MIN
                        } else if d > i64::MAX as f64 {
                            i64::MAX
                        } else {
                            d as i64
                        }
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
        ctx: &JSContext,
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

    pub fn to_f64(&self, ctx: &JSContext) -> Result<f64, i32> {
        match self {
            JSValue::Int(i) => Ok(*i as f64),
            JSValue::Bool(b) => Ok(*b as i32 as f64),
            JSValue::Float64(n) => Ok(*n),
            JSValue::NULL => Ok(0.0),
            JSValue::UNDEFINED => Ok(f64::NAN),
            JSValue::EXCEPTION => Err(-1),
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
    pub fn to_length(&self, ctx: &JSContext) -> Result<i64, i32> {
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

// /* same as JS_ToNumber() but return 0 in case of NaN/Undefined */
// pub unsafe fn JS_ToIntegerFree(ctx: &JSContext, mut val: JSValue) -> JSValue {
//     let ret: JSValue;

//     loop {
//         match val {
//             JSValue::UNDEFINED | JSValue::NULL => {
//                 ret = JSValue::Int(0);
//             }
//             JSValue::Bool(b) => {
//                 ret = JSValue::Int(b as i32);
//             }
//             JSValue::Int(i) => {
//                 ret = JSValue::Int(i);
//             }
//             JSValue::Float64(d) => {
//                 if d.is_nan() {
//                     ret = 0.into();
//                 } else {
//                     /* convert -0 to +0 */
//                     let d = d.trunc() + 0.0;
//                     ret = JSValue::Float64(d);
//                 }
//             }
//             JSValue::EXCEPTION => {
//                 ret = val;
//             }
//             _ => {
//                 val = JS_ToNumberFree(ctx, val);
//                 if val.is_exception() {
//                     return val;
//                 }
//                 continue;
//             }
//         }

//         break;
//     }

//     ret
// }

pub const fn JS_NumberIsNegativeOrMinusZero(val: &JSValueConst) -> bool {
    match val {
        JSValue::Int(v) => *v < 0,
        JSValue::Float64(d) => {
            let u = JSFloat64Union { d: *d };
            unsafe { (u.uint64 >> 63) != 0 }
        }
        _ => false,
    }
}
