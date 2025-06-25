use crate::{
    context::JSContext,
    error::JSResult,
    utils::skip_spaces,
    value::{HINT_NUMBER, JS_FreeValue, JSValue, JSValueConst},
};

use super::{ATOD_ACCEPT_BIN_OCT, js_atof};

pub const MAX_SAFE_INTEGER: i64 = (1_i64 << 53) - 1;
pub const JS_FLOAT64_NAN: f64 = f64::NAN;

pub fn is_safe_integer(d: f64) -> bool {
    f64::is_finite(d) && d.floor() == d && d.abs() <= MAX_SAFE_INTEGER as f64
}

#[derive(Clone, Copy)]
pub union JSFloat64Union {
    pub d: f64,
    pub u64: u64,
    pub u32: [u32; 2],
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
            (val as i32).into()
        } else {
            Self::from_f64_as_is(val as f64)
        }
    }
}

impl From<i64> for JSValue {
    #[inline(always)]
    fn from(val: i64) -> Self {
        let v32 = val as i32;
        if val == v32 as i64 {
            Self::Int(v32)
        } else {
            Self::Float64(val as f64)
        }
    }
}

impl From<f64> for JSValue {
    #[inline(always)]
    fn from(d: f64) -> Self {
        let val = d as i32;
        let u = JSFloat64Union { d };
        let t = JSFloat64Union { d: val as f64 };

        unsafe {
            // -0 cannot be represented as integer, so we compare the bit representation
            if u.u64 == t.u64 {
                Self::Int(val)
            } else {
                Self::Float64(d)
            }
        }
    }
}

impl JSValue {
    #[inline(always)]
    pub const fn from_i32(val: i32) -> Self {
        Self::Int(val)
    }

    #[inline(always)]
    pub const fn from_u32(val: u32) -> JSValue {
        if val <= 0x7fffffff {
            JSValue::from_i32(val as _)
        } else {
            Self::from_f64_as_is(val as f64)
        }
    }

    #[inline(always)]
    pub const fn from_i64(val: i64) -> JSValue {
        if val == val as i32 as i64 {
            JSValue::from_i32(val as i32)
        } else {
            Self::from_f64_as_is(val as f64)
        }
    }

    /// to create f64 value as-is
    #[cfg(not(feature = "nan_boxing"))]
    #[inline(always)]
    pub const fn from_f64_as_is(d: f64) -> JSValue {
        JSValue::Float64(d)
    }

    /// to create f64 value as-is
    #[cfg(feature = "nan_boxing")]
    #[inline(always)]
    pub(crate) const fn from_f64_as_is(d: f64) -> JSValue {
        union Float64Union {
            d: f64,
            u64: u64,
        }

        let u = Float64Union { d };

        // Normalize NaN
        if (u.u64 & 0x7fffffffffffffff) > 0x7ff0000000000000 {
            JSValue::NAN
        } else {
            JSValue::from_u64(u.u64 - ((super::value::JS_FLOAT64_TAG_ADDEND as u64) << 32))
        }
    }

    #[inline(always)]
    pub const fn from_f64(d: f64) -> JSValue {
        let val = d as i32;
        let u = JSFloat64Union { d };
        let t = JSFloat64Union { d: val as f64 };

        unsafe {
            // -0 cannot be represented as integer, so we compare the bit representation
            if u.u64 == t.u64 {
                Self::Int(val)
            } else {
                Self::Float64(d)
            }
        }
    }

    /// test if the value is an intergral number.
    /// returns Err if test has exception.
    pub fn is_integer(&self, ctx: &JSContext) -> Result<bool, i32> {
        if !self.is_number() {
            Ok(false)
        } else {
            let mut d: f64 = 0.0;
            if unsafe { JS_ToFloat64(ctx, &mut d, self) } != 0 {
                Err(-1)
            } else {
                Ok(d.is_finite() && d.fract() == 0.0)
            }
        }
    }

    /// coerce `val` value to number value according to converting hint.
    /// Note: `val` is consumed.
    fn do_coerce_to_number_free(
        ctx: &JSContext,
        mut val: JSValue,
        flag: JSToNumberHintEnum,
    ) -> JSResult {
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
                Self::Object(_) => {
                    val = val.coerce_to_primitive(ctx, HINT_NUMBER)?;
                    continue;
                }
                Self::String(_) => {
                    let (str_ptr0, len) = {
                        let str = val.coerce_to_utf8(ctx)?;
                        let len = str.len();
                        (str.as_ptr(), len)
                    };

                    let mut p = unsafe { str_ptr0.add(skip_spaces(str_ptr0)) };

                    let res: JSValue = if unsafe { p.offset_from(str_ptr0) as usize } == len {
                        // is empty or white space string
                        0.into()
                    } else {
                        let mut r = unsafe {
                            js_atof(
                                ctx,
                                p.cast(),
                                std::ptr::addr_of_mut!(p),
                                0,
                                ATOD_ACCEPT_BIN_OCT,
                            )
                            .check_exception()?
                        };

                        p = unsafe { p.add(skip_spaces(p.cast())) };
                        if unsafe { p.offset_from(str_ptr0) as usize } != len {
                            r = JSValue::Float64(f64::NAN);
                        }
                        r
                    };
                    break Ok(res);
                }
                Self::Symbol(_) => {
                    ctx.JS_ThrowTypeError("cannot convert symbol to number");
                    break Err(-1);
                }
                Self::EXCEPTION => {
                    break Err(-1);
                }
                _ => {
                    break Ok(JSValue::Float64(f64::NAN));
                }
            }
        }
    }

    /// Note: `self` is consumed regardless of success or failure.
    #[inline(always)]
    pub fn coerce_to_number_free(self, ctx: &JSContext) -> Result<JSValue, i32> {
        Self::do_coerce_to_number_free(ctx, self, JSToNumberHintEnum::TON_FLAG_NUMBER)
    }

    /// Note: `self` is consumed regardless of success or failure.
    #[inline(always)]
    pub fn coerce_to_numeric_free(self, ctx: &JSContext) -> Result<JSValue, i32> {
        Self::do_coerce_to_number_free(ctx, self, JSToNumberHintEnum::TON_FLAG_NUMERIC)
    }

    /// Note: `self` is not consumed.
    #[inline(always)]
    pub unsafe fn coerce_to_number(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.clone().coerce_to_number_free(ctx)
    }

    /// Note: `self` is not consumed.
    #[inline(always)]
    pub fn coerce_to_numeric(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.clone().coerce_to_numeric_free(ctx)
    }

    /// coerce value to i32.
    /// Note: value `self` is consumed regardless of success or failure.
    pub fn coerce_to_i32_free(self, ctx: &JSContext) -> Result<i32, i32> {
        let mut val = self;

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
                    let e = unsafe { (u.u64 >> 52) & 0x7ff } as i32;
                    break if e <= (1023 + 30) {
                        // fast case
                        d as i32
                    } else if e <= (1023 + 30 + 53) {
                        /* remainder modulo 2^32 */
                        let mut v = unsafe { (u.u64 & ((1u64 << 52) - 1)) | (1u64 << 52) };
                        v <<= (e - 1023 - 52 + 32) as u32;

                        let mut ret = (v >> 32) as i32;
                        // take the sign into account
                        if unsafe { u.u64 >> 63 } != 0 {
                            ret = -ret;
                        }
                        ret
                    } else {
                        0 // also handles NaN and +inf
                    };
                }
                _ => {
                    val = val.coerce_to_number_free(ctx)?;
                    continue;
                }
            }
        };

        Ok(ret)
    }

    /// coerce value to i32.
    /// Note: value `self` is not consumed
    #[inline(always)]
    pub fn coerce_to_i32(&self, ctx: &JSContext) -> Result<i32, i32> {
        self.clone().coerce_to_i32_free(ctx)
    }
}

#[deprecated(note = "use JSValue::from_i32() instead")]
pub fn JS_NewInt32(ctx: &JSContext, val: i32) -> JSValue {
    JSValue::from_i32(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_u32() instead")]
pub fn JS_NewUint32(ctx: &JSContext, val: u32) -> JSValue {
    JSValue::from_u32(val)
}

#[deprecated(note = "use JSValue::from_i64() instead")]
#[inline(always)]
pub fn JS_NewInt64(ctx: &JSContext, val: i64) -> JSValue {
    JSValue::from_i64(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64_as_is() instead")]
pub fn __JS_NewFloat64(ctx: &JSContext, d: f64) -> JSValue {
    JSValue::from_f64_as_is(d)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64() instead")]
pub fn JS_NewFloat64(ctx: &JSContext, d: f64) -> JSValue {
    JSValue::from_f64(d)
}

/* Note: can return an exception (-1) */
#[deprecated(note = "use JSValue::is_integer() instead")]
pub unsafe fn JS_NumberIsInteger(ctx: &JSContext, val: JSValueConst) -> i32 {
    val.is_integer(ctx).map_or(-1, |b| b as i32)
}

/* return (<0, 0) in case of exception */
#[deprecated(note = "use JSValue::coerce_to_i32_free() instead")]
pub unsafe fn JS_ToInt32Free(ctx: &JSContext, pres: *mut i32, val: JSValue) -> i32 {
    val.coerce_to_i32_free(ctx).map_or(-1, |v| {
        *pres = v;
        0
    })
}

/// `val` is not consumed
#[inline(always)]
pub unsafe fn JS_ToInt32(ctx: &JSContext, val: &JSValueConst) -> Result<i32, i32> {
    let mut n = 0;
    if JS_ToInt32Free(ctx, &mut n, val.clone()) == -1 {
        Err(-1)
    } else {
        Ok(n)
    }
}

/// `val` is not consumed
#[inline(always)]
pub unsafe fn JS_ToUint32(ctx: &JSContext, val: &JSValueConst) -> Result<u32, i32> {
    JS_ToInt32(ctx, val).map(|v| v as u32)
}

#[inline(always)]
pub unsafe fn JS_ToUint32Free(ctx: &JSContext, val: JSValue) -> Result<u32, i32> {
    let mut n = 0;
    if JS_ToInt32Free(ctx, &mut n, val) == -1 {
        Err(-1)
    } else {
        Ok(n as u32)
    }
}

pub unsafe fn JS_ToUint8ClampFree(ctx: &JSContext, pres: *mut i32, val: JSValue) -> i32 {
    let res: i32;
    let mut val = val;

    'redo: loop {
        match val {
            JSValue::NULL | JSValue::UNDEFINED => {
                res = 0;
            }
            JSValue::Int(i) => {
                res = i.clamp(0, 255);
            }
            JSValue::Bool(b) => {
                res = b as i32;
            }
            JSValue::Float64(d) => {
                res = if d.is_nan() || d < 0.0 {
                    0
                } else if d > 255.0 {
                    255
                } else {
                    f64::round_ties_even(d) as i32
                };
            }
            JSValue::EXCEPTION => {
                *pres = 0;
                return -1;
            }
            _ => {
                if let Ok(n) = val.coerce_to_number_free(ctx) {
                    val = n;
                    continue 'redo;
                } else {
                    *pres = 0;
                    return -1;
                }
            }
        }

        break;
    }

    *pres = res;
    0
}

pub unsafe fn JS_ToUint8Clamp(ctx: &JSContext, pres: *mut i32, val: &JSValue) -> i32 {
    JS_ToUint8ClampFree(ctx, pres, val.clone())
}

/// Note: `val` is consumed
pub unsafe fn __JS_ToFloat64Free(ctx: &JSContext, pres: *mut f64, val: JSValue) -> i32 {
    if let Ok(v) = val.coerce_to_number_free(ctx) {
        let d: f64 = match v {
            JSValue::Int(v) => v as f64,
            JSValue::Float64(v) => v,
            _ => {
                unreachable!()
            }
        };

        *pres = d;
        0
    } else {
        *pres = f64::NAN; //JS_FLOAT64_NAN;
        -1
    }
}

/// val is consumed
#[inline]
pub unsafe fn JS_ToFloat64Free(ctx: &JSContext, pres: *mut f64, val: JSValue) -> i32 {
    match val {
        JSValue::Int(i) => {
            *pres = i as f64;
            0
        }
        JSValue::Float64(n) => {
            *pres = n;
            0
        }
        JSValue::UNDEFINED => {
            *pres = f64::NAN;
            return -1; // Error
        }
        JSValue::NULL => {
            *pres = 0.0;
            0
        }
        _ => __JS_ToFloat64Free(ctx, pres, val),
    }
}

pub unsafe fn JS_ToFloat64(ctx: &JSContext, pres: *mut f64, val: &JSValueConst) -> i32 {
    JS_ToFloat64Free(ctx, pres, val.clone())
}

/// this hint is for BigInt only
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum JSToNumberHintEnum {
    TON_FLAG_NUMBER,
    TON_FLAG_NUMERIC,
}

/// returns value of int, float64, nan, exception
#[deprecated(note = "use JSValue::do_coerce_number_free() instead")]
pub unsafe fn JS_ToNumberHintFree(
    ctx: &JSContext,
    val: JSValue,
    flag: JSToNumberHintEnum,
) -> JSValue {
    JSValue::do_coerce_to_number_free(ctx, val, flag).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_number_free() instead")]
pub unsafe fn JS_ToNumberFree(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_number_free(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric_free() instead")]
pub unsafe fn JS_ToNumericFree(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_numeric_free(ctx)
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric() instead")]
pub unsafe fn JS_ToNumeric(ctx: &JSContext, val: JSValueConst) -> JSValue {
    val.coerce_to_numeric(ctx).unwrap_or(JSValue::EXCEPTION)
}

/* same as JS_ToNumber() but return 0 in case of NaN/Undefined */
pub unsafe fn JS_ToIntegerFree(ctx: &JSContext, mut val: JSValue) -> JSValue {
    let ret: JSValue;

    loop {
        match val {
            JSValue::UNDEFINED | JSValue::NULL => {
                ret = 0.into();
            }
            JSValue::Bool(b) => {
                ret = (0 as i32).into();
            }
            JSValue::Int(i) => {
                ret = JSValue::Int(i);
            }
            JSValue::Float64(d) => {
                if d.is_nan() {
                    ret = 0.into();
                } else {
                    /* convert -0 to +0 */
                    let d = d.trunc() + 0.0;
                    ret = JSValue::Float64(d);
                }
            }
            JSValue::EXCEPTION => {
                ret = val;
            }
            _ => {
                val = JS_ToNumberFree(ctx, val);
                if val.is_exception() {
                    return val;
                }
                continue;
            }
        }

        break;
    }

    ret
}

// Note: the integer value is saturated to 32 bits
pub unsafe fn JS_ToInt32SatFree(ctx: &JSContext, pres: *mut i32, mut val: JSValue) -> i32 {
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
                } else {
                    if d < i32::MIN as f64 {
                        i32::MIN
                    } else if d > i32::MAX as f64 {
                        i32::MAX
                    } else {
                        d as i32
                    }
                };
            }
            JSValue::EXCEPTION => {
                *pres = 0;
                return -1;
            }
            _ => {
                val = JS_ToNumberFree(ctx, val);
                if val.is_exception() {
                    *pres = 0;
                    return -1;
                }
            }
        }
    };

    *pres = ret;
    0
}

/// val is not consumed
pub unsafe fn JS_ToInt32Sat(ctx: &JSContext, pres: *mut i32, val: &JSValueConst) -> i32 {
    JS_ToInt32SatFree(ctx, pres, val.clone())
}

/// val is not consumed
pub unsafe fn JS_ToInt32Clamp(
    ctx: &JSContext,
    pres: *mut i32,
    val: &JSValueConst,
    min: i32,
    max: i32,
    min_offset: i32,
) -> i32 {
    let res = JS_ToInt32SatFree(ctx, pres, val.clone());
    if res == 0 {
        if *pres < min {
            *pres += min_offset;
            if *pres < min {
                *pres = min;
            }
        } else {
            if *pres > max {
                *pres = max;
            }
        }
    }
    res
}

pub fn JS_NumberIsNegativeOrMinusZero(val: &JSValueConst) -> bool {
    match val {
        JSValue::Int(v) => *v < 0,
        JSValue::Float64(d) => {
            let u = JSFloat64Union { d: *d };
            unsafe { (u.u64 >> 63) != 0 }
        }
        _ => false,
    }
}

pub unsafe fn JS_ToInt64SatFree(ctx: &JSContext, pres: *mut i64, mut val: JSValue) -> i32 {
    loop {
        match val {
            JSValue::NULL | JSValue::UNDEFINED => {
                *pres = 0;
                return 0;
            }
            JSValue::Int(i) => {
                *pres = i as i64;
                return 0;
            }
            JSValue::Bool(b) => {
                *pres = b as i64;
                return 0;
            }
            JSValue::EXCEPTION => {
                *pres = 0;
                return -1;
            }
            JSValue::Float64(d) => {
                if d.is_nan() {
                    *pres = 0;
                } else {
                    if d < i64::MIN as f64 {
                        *pres = i64::MIN;
                    } else if d > i64::MAX as f64 {
                        *pres = i64::MAX;
                    } else {
                        *pres = d as i64;
                    }
                }
                return 0;
            }

            _ => {
                // update val, then redo
                val = JS_ToNumberFree(ctx, val);
                if val.is_exception() {
                    *pres = 0;
                    return -1;
                }
                continue;
            }
        }
    }
}

/// val is not consumed
pub unsafe fn JS_ToInt64Sat(ctx: &JSContext, pres: *mut i64, val: &JSValueConst) -> i32 {
    JS_ToInt64SatFree(ctx, pres, val.clone())
}

/// val is not consumed
pub unsafe fn JS_ToInt64Clamp(
    ctx: &JSContext,
    pres: *mut i64,
    val: &JSValueConst,
    min: i64,
    max: i64,
    neg_offset: i64,
) -> i32 {
    let res = JS_ToInt64SatFree(ctx, pres, val.clone());
    if res == 0 {
        if (*pres) < 0 {
            (*pres) += neg_offset;
        }
        if (*pres) < min {
            (*pres) = min;
        } else if (*pres) > max {
            (*pres) = max;
        }
    }
    res
}

/// convert a value to a length between 0 and MAX_SAFE_INTEGER.
pub unsafe fn JS_ToLengthFree(ctx: &JSContext, val: JSValue) -> Result<i64, i32> {
    let mut len = 0;
    let res = JS_ToInt64Clamp(ctx, &mut len, &val, 0, MAX_SAFE_INTEGER, 0);
    if res == -1 { Err(-1) } else { Ok(len) }
}
