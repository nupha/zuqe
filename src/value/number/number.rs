use crate::{
    context::JSContext,
    error::JSResult,
    utils::skip_spaces,
    value::{
        JSTag, JSValue, JSValueConst, JSValueUnion, JS_DupValue, JS_FreeValue, JS_IsException,
        JS_IsNumber, HINT_NUMBER, JS_TAG_IS_FLOAT64, JS_TAG_NULL, JS_VALUE_GET_FLOAT64,
        JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_TAG,
    },
    JS_MKVAL,
};

use super::{js_atof, ATOD_ACCEPT_BIN_OCT};

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
        JS_MKVAL!(JSTag::Int, val)
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
            v32.into()
        } else {
            Self::from_f64_as_is(val as f64)
        }
    }
}

impl From<f64> for JSValue {
    #[inline(always)]
    fn from(d: f64) -> Self {
        union _U {
            d: f64,
            u: u64,
        }

        let val = d as i32;
        let u = _U { d };
        let t = _U { d: val as f64 };

        unsafe {
            // -0 cannot be represented as integer, so we compare the bit representation
            if u.u == t.u {
                JS_MKVAL!(JSTag::Int, val)
            } else {
                Self::from_f64_as_is(d)
            }
        }
    }
}

impl JSValue {
    #[inline(always)]
    pub const fn from_i32(val: i32) -> Self {
        JS_MKVAL!(JSTag::Int, val)
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
        JSValue {
            tag: JSTag::Float64,
            u: JSValueUnion { float64: d },
        }
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
        union _U {
            d: f64,
            u: u64,
        }

        let val = d as i32;
        let u = _U { d };
        let t = _U { d: val as f64 };

        unsafe {
            // -0 cannot be represented as integer, so we compare the bit representation
            if u.u == t.u {
                JS_MKVAL!(JSTag::Int, val)
            } else {
                Self::from_f64_as_is(d)
            }
        }
    }

    /// test if the value is an intergral number.
    /// returns Err if test has exception.
    pub fn is_integer(self, ctx: &mut JSContext) -> Result<bool, i32> {
        if !JS_IsNumber(self) {
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
    unsafe fn do_coerce_to_number_free(
        ctx: &mut JSContext,
        val: JSValue,
        flag: JSToNumberHintEnum,
    ) -> JSResult {
        let mut val = val.into_rc(ctx.rt());

        loop {
            match JS_VALUE_GET_NORM_TAG(*val) {
                JSTag::Int | JSTag::Float64 => {
                    break Ok(val.into());
                }
                JSTag::Bool | JSTag::Null => {
                    break Ok(JS_VALUE_GET_INT(val.into()).into());
                }
                JSTag::Undefined => {
                    break Ok(JSValue::NAN);
                }
                JSTag::Object => {
                    val = val.coerce_to_primitive(ctx, HINT_NUMBER)?.into_rc(ctx.rt());
                    continue;
                }
                JSTag::String => {
                    let (str_ptr0, len) = {
                        let str = val.coerce_to_utf8(ctx.handle())?;
                        let len = str.len();
                        (str.as_ptr(), len)
                    };

                    let mut p = str_ptr0.add(skip_spaces(str_ptr0));

                    let res = if p.offset_from(str_ptr0) as usize == len {
                        // is empty or white space string
                        JSValue::ZERO
                    } else {
                        let mut r = js_atof(
                            ctx as *mut JSContext,
                            p.cast(),
                            std::ptr::addr_of_mut!(p),
                            0,
                            ATOD_ACCEPT_BIN_OCT,
                        )
                        .check_exception()?;

                        p = p.add(skip_spaces(p.cast()));
                        if p.offset_from(str_ptr0) as usize != len {
                            JS_FreeValue(ctx, r);
                            r = JSValue::NAN;
                        }

                        r
                    };
                    break Ok(res);
                }
                JSTag::Symbol => {
                    ctx.JS_ThrowTypeError(c"cannot convert symbol to number");
                    break Err(-1);
                }
                JSTag::Exception => {
                    break Err(-1);
                }
                JSTag::BigInt => {
                    break if flag != JSToNumberHintEnum::TON_FLAG_NUMERIC {
                        ctx.JS_ThrowTypeError(c"cannot convert bigint to number");
                        Err(-1)
                    } else {
                        Ok(val.into())
                    };
                }
                _ => {
                    break Ok(JSValue::NAN);
                }
            }
        }
    }

    /// Note: `self` is consumed regardless of success or failure.
    #[inline(always)]
    pub fn coerce_to_number_free(self, ctx: &mut JSContext) -> Result<JSValue, i32> {
        unsafe { Self::do_coerce_to_number_free(ctx, self, JSToNumberHintEnum::TON_FLAG_NUMBER) }
    }

    /// Note: `self` is consumed regardless of success or failure.
    #[inline(always)]
    pub fn coerce_to_numeric_free(self, ctx: &mut JSContext) -> Result<JSValue, i32> {
        unsafe { Self::do_coerce_to_number_free(ctx, self, JSToNumberHintEnum::TON_FLAG_NUMERIC) }
    }

    /// Note: `self` is not consumed.
    #[inline(always)]
    pub unsafe fn coerce_to_number(self, ctx: &mut JSContext) -> Result<JSValue, i32> {
        self.dup().coerce_to_number_free(ctx)
    }

    /// Note: `self` is not consumed.
    #[inline(always)]
    pub fn coerce_to_numeric(self, ctx: &mut JSContext) -> Result<JSValue, i32> {
        self.dup().coerce_to_numeric_free(ctx)
    }

    /// coerce value to i32.
    /// Note: value `self` is consumed regardless of success or failure.
    pub fn coerce_to_i32_free(self, ctx: &mut JSContext) -> Result<i32, i32> {
        let mut val = self;

        let ret: i32 = loop {
            match JS_VALUE_GET_NORM_TAG(val) {
                JSTag::Int | JSTag::Bool | JSTag::Null | JSTag::Undefined => {
                    break JS_VALUE_GET_INT(val);
                }
                JSTag::Float64 => {
                    union _U {
                        d: f64,
                        u64: u64,
                    }
                    let d = JS_VALUE_GET_FLOAT64(val);
                    let u = _U { d };

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
    pub fn coerce_to_i32(self, ctx: &mut JSContext) -> Result<i32, i32> {
        self.dup().coerce_to_i32_free(ctx)
    }
}

#[deprecated(note = "use JSValue::from_i32() instead")]
pub fn JS_NewInt32(ctx: *mut JSContext, val: i32) -> JSValue {
    JSValue::from_i32(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_u32() instead")]
pub fn JS_NewUint32(ctx: *mut JSContext, val: u32) -> JSValue {
    JSValue::from_u32(val)
}

#[deprecated(note = "use JSValue::from_i64() instead")]
#[inline(always)]
pub fn JS_NewInt64(ctx: *mut JSContext, val: i64) -> JSValue {
    JSValue::from_i64(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64_as_is() instead")]
pub fn __JS_NewFloat64(ctx: *const JSContext, d: f64) -> JSValue {
    JSValue::from_f64_as_is(d)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64() instead")]
pub fn JS_NewFloat64(ctx: *mut JSContext, d: f64) -> JSValue {
    JSValue::from_f64(d)
}

/* Note: can return an exception (-1) */
#[deprecated(note = "use JSValue::is_integer() instead")]
pub unsafe fn JS_NumberIsInteger(ctx: *mut JSContext, val: JSValueConst) -> i32 {
    val.is_integer(ctx.as_mut().unwrap())
        .map_or(-1, |b| b as i32)
}

/* return (<0, 0) in case of exception */
#[deprecated(note = "use JSValue::to_i32_free() instead")]
pub unsafe fn JS_ToInt32Free(ctx: *mut JSContext, pres: *mut i32, val: JSValue) -> i32 {
    val.coerce_to_i32_free(ctx.as_mut().unwrap())
        .map_or(-1, |v| {
            *pres = v;
            0
        })
}

/// `val` is not consumed
#[inline(always)]
pub unsafe fn JS_ToInt32(ctx: *mut JSContext, val: JSValueConst) -> Result<i32, i32> {
    let mut n = 0;
    if JS_ToInt32Free(ctx, &mut n, JS_DupValue(ctx, val)) == -1 {
        Err(-1)
    } else {
        Ok(n)
    }
}

/// `val` is not consumed
#[inline(always)]
pub unsafe fn JS_ToUint32(ctx: *mut JSContext, val: JSValueConst) -> Result<u32, i32> {
    JS_ToInt32(ctx, val).map(|v| v as u32)
}

#[inline(always)]
pub unsafe fn JS_ToUint32Free(ctx: *mut JSContext, val: JSValue) -> Result<u32, i32> {
    let mut n = 0;
    if JS_ToInt32Free(ctx, &mut n, val) == -1 {
        Err(-1)
    } else {
        Ok(n as u32)
    }
}

pub unsafe fn JS_ToUint8ClampFree(ctx: *mut JSContext, pres: *mut i32, val: JSValue) -> i32 {
    let res: i32;
    let mut val = val;

    'redo: loop {
        match JS_VALUE_GET_NORM_TAG(val) {
            JSTag::Int | JSTag::Bool | JSTag::Null | JSTag::Undefined => {
                res = JS_VALUE_GET_INT(val).clamp(0, 255);
            }
            JSTag::Float64 => {
                let d = JS_VALUE_GET_FLOAT64(val);
                res = if d.is_nan() {
                    0
                } else if d < 0.0 {
                    0
                } else if d > 255.0 {
                    255
                } else {
                    f64::round_ties_even(d) as i32
                };
            }
            JSTag::Exception => {
                *pres = 0;
                return -1;
            }
            _ => {
                if let Ok(n) = val.coerce_to_number_free(ctx.as_mut().unwrap()) {
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

/// Note: `val` is consumed
pub unsafe fn __JS_ToFloat64Free(ctx: *mut JSContext, pres: *mut f64, val: JSValue) -> i32 {
    if let Ok(v) = val.coerce_to_number_free(ctx.as_mut().unwrap()) {
        let d: f64 = match JS_VALUE_GET_NORM_TAG(v) {
            JSTag::Int => JS_VALUE_GET_INT(v) as f64,
            JSTag::Float64 => JS_VALUE_GET_FLOAT64(v),
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

#[inline]
pub unsafe fn JS_ToFloat64Free(ctx: *mut JSContext, pres: *mut f64, val: JSValue) -> i32 {
    let tag = JS_VALUE_GET_TAG(val);
    if tag <= JS_TAG_NULL {
        *pres = JS_VALUE_GET_INT(val) as f64;
        0
    } else if JS_TAG_IS_FLOAT64(tag) {
        *pres = JS_VALUE_GET_FLOAT64(val);
        0
    } else {
        __JS_ToFloat64Free(ctx, pres, val)
    }
}

pub unsafe fn JS_ToFloat64(ctx: *mut JSContext, pres: *mut f64, val: JSValueConst) -> i32 {
    JS_ToFloat64Free(ctx, pres, val.dup())
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
    ctx: *mut JSContext,
    val: JSValue,
    flag: JSToNumberHintEnum,
) -> JSValue {
    JSValue::do_coerce_to_number_free(ctx.as_mut().unwrap(), val, flag)
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_number_free() instead")]
pub unsafe fn JS_ToNumberFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    val.coerce_to_number_free(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric_free() instead")]
pub unsafe fn JS_ToNumericFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    val.coerce_to_numeric_free(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric() instead")]
pub unsafe fn JS_ToNumeric(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    val.coerce_to_numeric(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

/* same as JS_ToNumber() but return 0 in case of NaN/Undefined */
pub unsafe fn JS_ToIntegerFree(ctx: *mut JSContext, mut val: JSValue) -> JSValue {
    let ret: JSValue;

    loop {
        let tag = JS_VALUE_GET_NORM_TAG(val);
        match tag {
            JSTag::Int | JSTag::Bool | JSTag::Null | JSTag::Undefined => {
                ret = JSValue::from_i32(JS_VALUE_GET_INT(val));
            }
            JSTag::Float64 => {
                let d = JS_VALUE_GET_FLOAT64(val);
                if d.is_nan() {
                    ret = JSValue::ZERO;
                } else {
                    /* convert -0 to +0 */
                    let d = d.trunc() + 0.0;
                    ret = JS_NewFloat64(ctx, d);
                }
            }
            JSTag::Exception => {
                ret = val;
            }
            _ => {
                val = JS_ToNumberFree(ctx, val);
                if JS_IsException(val) {
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
pub unsafe fn JS_ToInt32SatFree(ctx: *mut JSContext, pres: *mut i32, val: JSValue) -> i32 {
    let ret: i32 = loop {
        match JS_VALUE_GET_NORM_TAG(val) {
            JSTag::Int | JSTag::Bool | JSTag::Null | JSTag::Undefined => {
                break JS_VALUE_GET_INT(val);
            }
            JSTag::Float64 => {
                let d: f64 = JS_VALUE_GET_FLOAT64(val);
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
            JSTag::Exception => {
                *pres = 0;
                return -1;
            }
            _ => {
                let val = JS_ToNumberFree(ctx, val);
                if JS_IsException(val) {
                    *pres = 0;
                    return -1;
                }
            }
        }
    };

    *pres = ret;
    0
}

pub unsafe fn JS_ToInt32Sat(ctx: *mut JSContext, pres: *mut i32, val: JSValueConst) -> i32 {
    JS_ToInt32SatFree(ctx, pres, JS_DupValue(ctx, val))
}

pub unsafe fn JS_ToInt32Clamp(
    ctx: *mut JSContext,
    pres: *mut i32,
    val: JSValueConst,
    min: i32,
    max: i32,
    min_offset: i32,
) -> i32 {
    let res = JS_ToInt32SatFree(ctx, pres, JS_DupValue(ctx, val));
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

pub fn JS_NumberIsNegativeOrMinusZero(ctx: *mut JSContext, val: JSValueConst) -> bool {
    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::Int => {
            let v = JS_VALUE_GET_INT(val);
            v < 0
        }
        JSTag::Float64 => {
            let mut u: JSFloat64Union = JSFloat64Union { u64: 0 };
            u.d = JS_VALUE_GET_FLOAT64(val);
            unsafe { (u.u64 >> 63) != 0 }
        }
        JSTag::BigInt => {
            // let p: *mut JSBigFloat = JS_VALUE_GET_PTR(val);
            // // C2RS: Dereferencing raw pointer and accessing fields
            // ((*(*p).num.sign) != 0 && !bf_is_zero(&(*p).num)) as BOOL
            unimplemented!()
        }
        _ => false,
    }
}

pub unsafe fn JS_ToInt64SatFree(ctx: *mut JSContext, pres: *mut i64, mut val: JSValue) -> i32 {
    loop {
        match JS_VALUE_GET_NORM_TAG(val) {
            JSTag::Int | JSTag::Bool | JSTag::Null | JSTag::Undefined => {
                *pres = JS_VALUE_GET_INT(val) as i64;
                return 0;
            }
            JSTag::Exception => {
                *pres = 0;
                return -1;
            }
            JSTag::Float64 => {
                let d = JS_VALUE_GET_FLOAT64(val);
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
                if JS_IsException(val) {
                    *pres = 0;
                    return -1;
                }
                continue;
            }
        }
    }
}

pub unsafe fn JS_ToInt64Sat(ctx: *mut JSContext, pres: *mut i64, val: JSValueConst) -> i32 {
    JS_ToInt64SatFree(ctx, pres, JS_DupValue(ctx, val))
}

pub unsafe fn JS_ToInt64Clamp(
    ctx: *mut JSContext,
    pres: *mut i64,
    val: JSValueConst,
    min: i64,
    max: i64,
    neg_offset: i64,
) -> i32 {
    let res = JS_ToInt64SatFree(ctx, pres, JS_DupValue(ctx, val));
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
pub unsafe fn JS_ToLengthFree(ctx: *mut JSContext, val: JSValue) -> Result<i64, i32> {
    let mut len = 0;
    let res = JS_ToInt64Clamp(ctx, &mut len, val, 0, MAX_SAFE_INTEGER, 0);
    JS_FreeValue(ctx, val);
    if res == -1 {
        Err(-1)
    } else {
        Ok(len)
    }
}
