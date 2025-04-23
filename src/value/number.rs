use std::{ffi::CStr, io::Write};

use smallvec::SmallVec;

use crate::{
    context::JSContext,
    utils::skip_spaces,
    value::{
        JSTag, JSValue, JSValueConst, JSValueUnion, JS_DupValue, JS_FreeValue, JS_IsException,
        JS_IsNumber, HINT_NUMBER, JS_NAN, JS_TAG_IS_FLOAT64, JS_TAG_NULL, JS_VALUE_GET_FLOAT64,
        JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_TAG,
    },
    JS_MKVAL,
};

use super::string::strstart;

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

impl JSValue {
    #[inline(always)]
    pub fn from_i32(val: i32) -> Self {
        JS_MKVAL!(JSTag::Int, val)
    }

    #[inline(always)]
    pub fn from_u32(val: u32) -> JSValue {
        if val <= 0x7fffffff {
            JSValue::from_i32(val as _)
        } else {
            Self::from_f64_as_is(val as f64)
        }
    }

    #[inline(always)]
    pub fn from_i64(val: i64) -> JSValue {
        if val == val as i32 as i64 {
            JSValue::from_i32(val as i32)
        } else {
            Self::from_f64_as_is(val as f64)
        }
    }

    /// to create f64 value as-is
    #[cfg(not(feature = "nan_boxing"))]
    #[inline(always)]
    pub fn from_f64_as_is(d: f64) -> JSValue {
        JSValue {
            tag: JSTag::Float64,
            u: JSValueUnion { float64: d },
        }
    }

    /// to create f64 value as-is
    #[cfg(feature = "nan_boxing")]
    #[inline(always)]
    pub(crate) fn from_f64_as_is(d: f64) -> JSValue {
        union Float64Union {
            d: f64,
            u64: u64,
        }

        let u = Float64Union { d };

        // Normalize NaN
        if (u.u64 & 0x7fffffffffffffff) > 0x7ff0000000000000 {
            JS_NAN
        } else {
            JSValue::from_u64(u.u64 - ((super::value::JS_FLOAT64_TAG_ADDEND as u64) << 32))
        }
    }

    #[inline(always)]
    pub fn from_f64(d: f64) -> JSValue {
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

    ///  test if the value is an intergral number.
    /// returns Err if test has exception.
    pub unsafe fn is_integer(&self, ctx: &mut JSContext) -> Result<bool, i32> {
        let val = *self;
        if !JS_IsNumber(val) {
            Ok(false)
        } else {
            let mut d: f64 = 0.0;
            if JS_ToFloat64(ctx, &mut d, val) != 0 {
                Err(-1)
            } else {
                Ok(d.is_finite() && d.fract() == 0.0)
            }
        }
    }

    /// coerce `val` value to number value according to converting hint.
    /// Note: `val` is consumed.
    unsafe fn do_coerce_number_free(
        ctx: &mut JSContext,
        val: JSValue,
        flag: JSToNumberHintEnum,
    ) -> Result<JSValue, i32> {
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
                        let str = val.to_utf8_string(ctx)?;
                        let len = str.len();
                        (str.as_ptr(), len)
                    };

                    let mut p = str_ptr0.offset(skip_spaces(str_ptr0) as isize);

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

                        p = p.offset(skip_spaces(p.cast()) as isize);
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

    /// Note: value `self` is consumed.
    #[inline(always)]
    pub fn coerce_number_free(self, ctx: &mut JSContext) -> Result<JSValue, i32> {
        unsafe { Self::do_coerce_number_free(ctx, self, JSToNumberHintEnum::TON_FLAG_NUMBER) }
    }

    /// Note: value `self` is consumed.
    #[inline(always)]
    pub fn coerce_numeric_free(self, ctx: &mut JSContext) -> Result<JSValue, i32> {
        unsafe { Self::do_coerce_number_free(ctx, self, JSToNumberHintEnum::TON_FLAG_NUMERIC) }
    }

    /// Note: value `self` is not consumed.
    #[inline(always)]
    pub fn coerce_numeric(self, ctx: &mut JSContext) -> Result<JSValue, i32> {
        self.dup().coerce_numeric_free(ctx)
    }

    /// coerce value to i32.
    /// NOTE: will consume self value once.
    pub unsafe fn to_i32_free(self, ctx: *mut JSContext) -> Result<i32, i32> {
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
                    let e = ((u.u64 >> 52) & 0x7ff) as i32;
                    break if e <= (1023 + 30) {
                        // fast case
                        d as i32
                    } else if e <= (1023 + 30 + 53) {
                        /* remainder modulo 2^32 */
                        let mut v = (u.u64 & ((1u64 << 52) - 1)) | (1u64 << 52);
                        v = v << ((e - 1023 - 52 + 32) as u32);
                        let mut ret = (v >> 32) as i32;
                        // take the sign into account
                        if (u.u64 >> 63) != 0 {
                            ret = -ret;
                        }
                        ret
                    } else {
                        0 // also handles NaN and +inf
                    };
                }
                _ => {
                    val = JS_ToNumberFree(ctx, val);
                    if val.is_exception() {
                        return Err(-1);
                    }
                    continue;
                }
            }
        };

        Ok(ret)
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
    val.to_i32_free(ctx.as_mut().unwrap()).map_or(-1, |v| {
        *pres = v;
        0
    })
}

#[inline(always)]
pub unsafe fn JS_ToInt32(ctx: *mut JSContext, val: JSValueConst) -> Result<i32, i32> {
    let mut n = 0;
    if JS_ToInt32Free(ctx, &mut n, JS_DupValue(ctx, val)) == -1 {
        Err(-1)
    } else {
        Ok(n)
    }
}

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
                res = JS_VALUE_GET_INT(val);
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
                    d.round() as i32
                };
            }
            JSTag::Exception => {
                *pres = 0;
                return -1;
            }
            _ => {
                val = JS_ToNumberFree(ctx, val);
                if JS_IsException(val) {
                    *pres = 0;
                    return -1;
                }
                continue 'redo;
            }
        }

        break;
    }

    *pres = res;
    0
}

pub unsafe fn __JS_ToFloat64Free(ctx: *mut JSContext, pres: *mut f64, mut val: JSValue) -> i32 {
    val = JS_ToNumberFree(ctx, val);

    if JS_IsException(val) {
        *pres = JS_FLOAT64_NAN;
        return -1;
    }

    let tag = JS_VALUE_GET_NORM_TAG(val);
    let d: f64 = match tag {
        JSTag::Int => JS_VALUE_GET_INT(val) as f64,
        JSTag::Float64 => JS_VALUE_GET_FLOAT64(val),
        _ => {
            unreachable!()
        }
    };

    *pres = d;
    0
}

#[inline]
pub unsafe fn JS_ToFloat64Free(ctx: *mut JSContext, pres: *mut f64, val: JSValue) -> i32 {
    let tag = JS_VALUE_GET_TAG(val);
    if tag <= JS_TAG_NULL {
        *pres = JS_VALUE_GET_INT(val) as f64;
        return 0;
    } else if JS_TAG_IS_FLOAT64(tag) {
        *pres = JS_VALUE_GET_FLOAT64(val);
        return 0;
    } else {
        return __JS_ToFloat64Free(ctx, pres, val);
    }
}

pub unsafe fn JS_ToFloat64(ctx: *mut JSContext, pres: *mut f64, val: JSValueConst) -> i32 {
    JS_ToFloat64Free(ctx, pres, JS_DupValue(ctx, val))
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
    JSValue::do_coerce_number_free(ctx.as_mut().unwrap(), val, flag).unwrap_or(JSValue::EXCEPTION)

    // loop {
    //     match JS_VALUE_GET_NORM_TAG(val) {
    //         JSTag::Int | JSTag::Float64 | JSTag::Exception => {
    //             break val;
    //         }
    //         JSTag::Bool | JSTag::Null => {
    //             break JSValue::from_i32(JS_VALUE_GET_INT(val));
    //         }
    //         JSTag::Undefined => {
    //             break JS_NAN;
    //         }
    //         JSTag::Object => {
    //             if let Ok(v) = val.coerce_to_primitive(ctx.as_mut().unwrap(), HINT_NUMBER) {
    //                 JS_FreeValue(ctx, val);
    //                 val = v;
    //                 continue;
    //             } else {
    //                 break JSValue::EXCEPTION;
    //             }
    //         }
    //         JSTag::String => {
    //             let v = if let Ok(str) = val.to_utf8_string(ctx.as_mut().unwrap()) {
    //                 let len = str.len();
    //                 let mut p = str.as_ptr();
    //                 p = p.offset(skip_spaces(p.cast()) as isize);

    //                 if (p as usize - str.as_ptr() as usize) == len {
    //                     // is empty or white space string
    //                     JSValue::ZERO
    //                 } else {
    //                     let mut r = js_atof(
    //                         ctx,
    //                         p.cast(),
    //                         std::ptr::addr_of_mut!(p),
    //                         0,
    //                         ATOD_ACCEPT_BIN_OCT,
    //                     );
    //                     if !r.is_exception() {
    //                         p = p.offset(skip_spaces(p.cast()) as isize);
    //                         if (p as usize - str.as_ptr() as usize) != len {
    //                             JS_FreeValue(ctx, r);
    //                             r = JS_NAN;
    //                         }
    //                     }
    //                     r
    //                 }
    //             } else {
    //                 JS_FreeValue(ctx, val);
    //                 JSValue::EXCEPTION
    //             };

    //             JS_FreeValue(ctx, val);
    //             break v;
    //         }
    //         JSTag::Symbol => {
    //             JS_FreeValue(ctx, val);
    //             break ctx
    //                 .as_mut()
    //                 .unwrap()
    //                 .JS_ThrowTypeError(c"cannot convert symbol to number");
    //         }
    //         JSTag::BigInt => {
    //             break if flag != JSToNumberHintEnum::TON_FLAG_NUMERIC {
    //                 JS_FreeValue(ctx, val);
    //                 ctx.as_mut()
    //                     .unwrap()
    //                     .JS_ThrowTypeError(c"cannot convert bigint to number")
    //             } else {
    //                 val
    //             };
    //         }
    //         _ => {
    //             JS_FreeValue(ctx, val);
    //             break JS_NAN;
    //         }
    //     }
    // }
}

#[deprecated(note = "use JSValue::coerce_number_free() instead")]
pub unsafe fn JS_ToNumberFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    val.coerce_number_free(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric_free() instead")]
pub unsafe fn JS_ToNumericFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    val.coerce_numeric_free(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric() instead")]
pub unsafe fn JS_ToNumeric(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    val.coerce_numeric(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

pub const ATOD_INT_ONLY: i32 = 1 << 0;
/* accept Oo and Ob prefixes in addition to 0x prefix if radix = 0 */
pub const ATOD_ACCEPT_BIN_OCT: i32 = 1 << 2;
/* accept O prefix as octal if radix == 0 and properly formed (Annex B) */
pub const ATOD_ACCEPT_LEGACY_OCTAL: i32 = 1 << 4;
/* accept _ between digits as a digit separator */
pub const ATOD_ACCEPT_UNDERSCORES: i32 = (1 << 5);
/* allow a suffix to override the type */
pub const ATOD_ACCEPT_SUFFIX: i32 = 1 << 6;
/* default type */
pub const ATOD_TYPE_MASK: i32 = (3 << 7);
pub const ATOD_TYPE_FLOAT64: i32 = (0 << 7);
pub const ATOD_TYPE_BIG_INT: i32 = (1 << 7);
/// accept -0x1
pub const ATOD_ACCEPT_PREFIX_AFTER_SIGN: i32 = 1 << 10;

/// `val` keeps ref count after calling this function.
pub unsafe fn JS_ToNumber(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    JS_ToNumberFree(ctx, val.dup())
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

/* radix != 10 is only supported with flags = JS_DTOA_VAR_FORMAT */
/* use as many digits as necessary */
pub const JS_DTOA_VAR_FORMAT: i32 = 0;
/* use n_digits significant digits (1 <= n_digits <= 101) */
pub const JS_DTOA_FIXED_FORMAT: i32 = 1;
/* force fractional format: [-]dd.dd with n_digits fractional digits */
pub const JS_DTOA_FRAC_FORMAT: i32 = 2;
/* force exponential notation either in fixed or variable format */
pub const JS_DTOA_FORCE_EXP: i32 = 4;

const JS_DTOA_BUF_SIZE: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoundingMode {
    FE_TONEAREST,
    FE_DOWNWARD,
    FE_UPWARD,
    FE_TOWARDZERO,
}

impl RoundingMode {
    pub fn round(&self, v: f64) -> f64 {
        match self {
            Self::FE_TONEAREST => {
                if v.fract() >= 0.5 {
                    v.ceil()
                } else {
                    v.floor()
                }
            }
            Self::FE_DOWNWARD => v.floor(),
            Self::FE_UPWARD => v.ceil(),
            Self::FE_TOWARDZERO => v.trunc(),
        }
    }
}

/* buf1 contains the printf result */
pub unsafe fn js_ecvt1(
    d: f64,
    n_digits: i32,
    decpt: *mut i32,
    sign: *mut i32,
    buf: *mut u8,
    rounding_mode: RoundingMode,
    buf1: *mut u8,
    buf1_size: usize,
) {
    // format float
    {
        let rounded_d = rounding_mode.round(d);
        let mut write = std::io::Cursor::new(std::slice::from_raw_parts_mut(buf1, buf1_size));
        write!(write, "{:+.*e}\0", n_digits as usize - 1, rounded_d).unwrap();
    }

    *sign = if *buf1 == '-' as u8 { 1 } else { 0 };

    // if rounding_mode != FE_TONEAREST {
    //     fesetround(rounding_mode);
    // }
    // libc::snprintf(
    //     buf1 as *mut i8,
    //     buf1_size,
    //     "%+.*e\0".as_ptr().cast(),
    //     n_digits - 1,
    //     d,
    // );
    // if rounding_mode != FE_TONEAREST {
    //     fesetround(FE_TONEAREST);
    // }
    // *sign = if *buf1.offset(0) == '-' as u8 { 1 } else { 0 };

    /* mantissa */
    *buf = *buf1.offset(1);
    if n_digits > 1 {
        std::ptr::copy_nonoverlapping(buf1.offset(3), buf.offset(1), (n_digits - 1) as usize);
    }
    *buf.offset(n_digits as isize) = b'\0';

    /* exponent */
    *decpt = libc::atoi(
        buf1.add(n_digits as usize + 2 + if n_digits > 1 { 1 } else { 0 })
            .cast(),
    ) + 1;
}

// needed because ecvt usually limits the number of digits to
// 17. Return the number of digits.
pub unsafe fn js_ecvt(
    d: f64,
    mut n_digits: i32,
    decpt: *mut i32,
    sign: *mut i32,
    buf: *mut u8,
    is_fixed: bool,
) -> i32 {
    let rounding_mode: RoundingMode;
    let mut buf_tmp: [u8; JS_DTOA_BUF_SIZE] = [0; JS_DTOA_BUF_SIZE];

    if !is_fixed {
        let mut n_digits_min: u32 = 1;
        let mut n_digits_max: u32 = 17;
        // find the minimum amount of digits (XXX: inefficient but simple)
        while n_digits_min < n_digits_max {
            n_digits = ((n_digits_min + n_digits_max) / 2) as i32;
            js_ecvt1(
                d,
                n_digits,
                decpt,
                sign,
                buf,
                RoundingMode::FE_TONEAREST,
                buf_tmp.as_mut_ptr(),
                std::mem::size_of::<[u8; JS_DTOA_BUF_SIZE]>(),
            );
            if libc::strtod(buf_tmp.as_ptr().cast(), std::ptr::null_mut()) == d {
                // no need to keep the trailing zeros
                while n_digits >= 2 && *buf.offset((n_digits - 1) as isize) == b'0' {
                    n_digits -= 1;
                }
                n_digits_max = n_digits as u32;
            } else {
                n_digits_min = n_digits as u32 + 1;
            }
        }
        n_digits = n_digits_max as i32;
        rounding_mode = RoundingMode::FE_TONEAREST;
    } else {
        rounding_mode = RoundingMode::FE_TONEAREST;

        #[cfg(CONFIG_PRINTF_RNDN)]
        {
            let mut buf1: [u8; JS_DTOA_BUF_SIZE] = [0; JS_DTOA_BUF_SIZE];
            let mut buf2: [u8; JS_DTOA_BUF_SIZE] = [0; JS_DTOA_BUF_SIZE];
            let mut decpt1: i32;
            let mut sign1: i32;
            let mut decpt2: i32;
            let mut sign2: i32;
            // The JS rounding is specified as round to nearest ties away
            // from zero (RNDNA), but in printf the "ties" case is not
            // specified (for example it is RNDN for glibc, RNDNA for
            // Windows), so we must round manually.
            js_ecvt1(
                d,
                n_digits + 1,
                &mut decpt1,
                &mut sign1,
                buf1.as_mut_ptr(),
                FE_TONEAREST,
                buf_tmp.as_mut_ptr(),
                std::mem::size_of::<[u8; JS_DTOA_BUF_SIZE]>(),
            );
            // XXX: could use 2 digits to reduce the average running time
            if *buf1.offset(n_digits as isize) == b'5' {
                js_ecvt1(
                    d,
                    n_digits + 1,
                    &mut decpt1,
                    &mut sign1,
                    buf1.as_mut_ptr(),
                    FE_DOWNWARD,
                    buf_tmp.as_mut_ptr(),
                    std::mem::size_of::<[u8; JS_DTOA_BUF_SIZE]>(),
                );
                js_ecvt1(
                    d,
                    n_digits + 1,
                    &mut decpt2,
                    &mut sign2,
                    buf2.as_mut_ptr(),
                    FE_UPWARD,
                    buf_tmp.as_mut_ptr(),
                    std::mem::size_of::<[u8; JS_DTOA_BUF_SIZE]>(),
                );
                if memcmp(buf1.as_ptr(), buf2.as_ptr(), (n_digits + 1) as usize) == 0
                    && decpt1 == decpt2
                {
                    // exact result: round away from zero
                    if sign1 != 0 {
                        rounding_mode = FE_DOWNWARD;
                    } else {
                        rounding_mode = FE_UPWARD;
                    }
                }
            }
        }
    }

    js_ecvt1(
        d,
        n_digits,
        decpt,
        sign,
        buf,
        rounding_mode,
        buf_tmp.as_mut_ptr(),
        std::mem::size_of::<[u8; JS_DTOA_BUF_SIZE]>(),
    );
    return n_digits;
}

pub unsafe fn js_fcvt(buf: *mut u8, buf_size: usize, d: f64, n_digits: i32) {
    let mut rounding_mode = RoundingMode::FE_TONEAREST;

    #[cfg(CONFIG_PRINTF_RNDN)]
    {
        let mut n1: i32;
        let mut n2: i32;
        let mut buf1: [u8; JS_DTOA_BUF_SIZE];
        let mut buf2: [u8; JS_DTOA_BUF_SIZE];

        // The JS rounding is specified as round to nearest ties away from
        // zero (RNDNA), but in printf the "ties" case is not specified
        // (for example it is RNDN for glibc, RNDNA for Windows), so we
        // must round manually.
        n1 = js_fcvt1(
            buf1.as_mut_ptr(),
            mem::size_of_val(&buf1),
            d,
            n_digits + 1,
            FE_TONEAREST,
        );
        rounding_mode = FE_TONEAREST;
        // XXX: could use 2 digits to reduce the average running time
        if *buf1.offset((n1 - 1) as isize) == b'5' {
            n1 = js_fcvt1(
                buf1.as_mut_ptr(),
                mem::size_of_val(&buf1),
                d,
                n_digits + 1,
                FE_DOWNWARD,
            );
            n2 = js_fcvt1(
                buf2.as_mut_ptr(),
                mem::size_of_val(&buf2),
                d,
                n_digits + 1,
                FE_UPWARD,
            );
            if n1 == n2 && memcmp(buf1.as_ptr(), buf2.as_ptr(), n1 as usize) == 0 {
                // exact result: round away from zero
                if *buf1.as_ptr() == b'-' {
                    rounding_mode = FE_DOWNWARD;
                } else {
                    rounding_mode = FE_UPWARD;
                }
            }
        }
    }

    js_fcvt1(buf.cast(), buf_size, d, n_digits, rounding_mode);
}

pub unsafe fn js_fcvt1(
    buf: *mut u8,
    buf_size: usize,
    d: f64,
    n_digits: i32,
    rounding_mode: RoundingMode,
) -> usize {
    {
        let rounded_d = rounding_mode.round(d);
        let mut write = std::io::Cursor::new(std::slice::from_raw_parts_mut(buf, buf_size));
        write!(write, "{:.*}\0", n_digits as usize - 1, rounded_d).unwrap();
    }

    let n = CStr::from_ptr(buf.cast()).count_bytes();
    assert!(n < buf_size, "Buffer overflow detected");
    return n;
}

/* XXX: slow and maybe not fully correct. Use libbf when it is fast enough.
   XXX: radix != 10 is only supported for small integers
*/
unsafe fn js_dtoa1(buf: *mut u8, d: f64, radix: i32, n_digits: i32, flags: i32) {
    let mut q: *mut u8;

    if !d.is_finite() {
        if d.is_nan() {
            // C2RS: strcpy replaced with manual copy
            let s = b"NaN\0";
            std::ptr::copy_nonoverlapping(s.as_ptr(), buf, 4);
        } else {
            q = buf;
            if d < 0.0 {
                *q = b'-';
                q = q.offset(1);
            }
            // C2RS: strcpy replaced with manual copy
            let s = b"Infinity\0";
            std::ptr::copy_nonoverlapping(s.as_ptr(), q, 9);
        }
    } else if flags == JS_DTOA_VAR_FORMAT {
        let mut buf1: [u8; 70] = [0; 70];
        let ptr: *mut u8;
        let i64 = d as i64;
        if d != i64 as f64 || i64 > MAX_SAFE_INTEGER || i64 < -MAX_SAFE_INTEGER {
            // C2RS: goto generic_conv converted to inline code
            let is_fixed = (flags & 3) == JS_DTOA_FIXED_FORMAT;
            let n_max = if is_fixed { n_digits } else { 21 };
            let mut decpt: i32 = 0;
            let mut sign: i32 = 0;
            let k = js_ecvt(
                d,
                n_digits,
                &mut decpt as *mut i32,
                &mut sign as *mut i32,
                buf1.as_mut_ptr(),
                is_fixed,
            );
            let n = decpt;
            q = buf;
            if sign != 0 {
                *q = b'-';
                q = q.offset(1);
            }
            if (flags & JS_DTOA_FORCE_EXP) != 0 {
                // C2RS: goto force_exp converted to inline code
                *q = buf1[0];
                q = q.offset(1);
                if k > 1 {
                    *q = b'.';
                    q = q.offset(1);
                    for i in 1..k {
                        *q = *buf1.as_ptr().offset(i as isize);
                        q = q.offset(1);
                    }
                }
                *q = b'e';
                q = q.offset(1);
                let p = n - 1;
                if p >= 0 {
                    *q = b'+';
                    q = q.offset(1);
                }
                libc::sprintf(q.cast(), b"%d\0".as_ptr().cast(), p);
            } else if n >= 1 && n <= n_max {
                if k <= n {
                    std::ptr::copy_nonoverlapping(buf1.as_ptr(), q, k as usize);
                    q = q.offset(k as isize);
                    for _ in 0..(n - k) {
                        *q = b'0';
                        q = q.offset(1);
                    }
                    *q = 0;
                } else {
                    std::ptr::copy_nonoverlapping(buf1.as_ptr(), q, n as usize);
                    q = q.offset(n as isize);
                    *q = b'.';
                    q = q.offset(1);
                    for i in 0..(k - n) {
                        *q = *buf1.as_ptr().offset((n + i) as isize);
                        q = q.offset(1);
                    }
                    *q = 0;
                }
            } else if n >= -5 && n <= 0 {
                *q = b'0';
                q = q.offset(1);
                *q = b'.';
                q = q.offset(1);
                for _ in 0..(-n) {
                    *q = b'0';
                    q = q.offset(1);
                }
                std::ptr::copy_nonoverlapping(buf1.as_ptr(), q, k as usize);
                q = q.offset(k as isize);
                *q = 0;
            } else {
                // force_exp:
                *q = buf1[0];
                q = q.offset(1);
                if k > 1 {
                    *q = b'.';
                    q = q.offset(1);
                    for i in 1..k {
                        *q = *buf1.as_ptr().offset(i as isize);
                        q = q.offset(1);
                    }
                }
                *q = b'e';
                q = q.offset(1);
                let p = n - 1;
                if p >= 0 {
                    *q = b'+';
                    q = q.offset(1);
                }
                libc::sprintf(
                    q as *mut libc::c_char,
                    b"%d\0".as_ptr() as *const libc::c_char,
                    p,
                );
            }
        } else {
            ptr = i64toa(buf1.as_mut_ptr().offset(69), i64, radix as u32);
            // C2RS: strcpy replaced with manual copy
            let mut src = ptr;
            let mut dest = buf;
            while *src != 0 {
                *dest = *src;
                dest = dest.offset(1);
                src = src.offset(1);
            }
            *dest = 0;
        }
    } else {
        let d = if d == 0.0 { 0.0 } else { d };
        if flags == JS_DTOA_FRAC_FORMAT {
            js_fcvt(buf, JS_DTOA_BUF_SIZE, d, n_digits);
        } else {
            let mut buf1: [u8; JS_DTOA_BUF_SIZE] = [0; JS_DTOA_BUF_SIZE];
            let mut sign: i32 = 0;
            let mut decpt: i32 = 0;
            let mut k: i32;
            let n_max = if (flags & 3) == JS_DTOA_FIXED_FORMAT {
                n_digits
            } else {
                21
            };
            // generic_conv:
            k = js_ecvt(
                d,
                n_digits,
                &mut decpt as *mut i32,
                &mut sign as *mut i32,
                buf1.as_mut_ptr(),
                (flags & 3) == JS_DTOA_FIXED_FORMAT,
            );
            let n = decpt;
            q = buf;
            if sign != 0 {
                *q = b'-';
                q = q.offset(1);
            }
            if (flags & JS_DTOA_FORCE_EXP) != 0 {
                // force_exp:
                *q = buf1[0];
                q = q.offset(1);
                if k > 1 {
                    *q = b'.';
                    q = q.offset(1);
                    for i in 1..k {
                        *q = *buf1.as_ptr().offset(i as isize);
                        q = q.offset(1);
                    }
                }
                *q = b'e';
                q = q.offset(1);
                let p = n - 1;
                if p >= 0 {
                    *q = b'+';
                    q = q.offset(1);
                }
                libc::sprintf(
                    q as *mut libc::c_char,
                    b"%d\0".as_ptr() as *const libc::c_char,
                    p,
                );
            } else if n >= 1 && n <= n_max {
                if k <= n {
                    std::ptr::copy_nonoverlapping(buf1.as_ptr(), q, k as usize);
                    q = q.offset(k as isize);
                    for _ in 0..(n - k) {
                        *q = b'0';
                        q = q.offset(1);
                    }
                    *q = 0;
                } else {
                    std::ptr::copy_nonoverlapping(buf1.as_ptr(), q, n as usize);
                    q = q.offset(n as isize);
                    *q = b'.';
                    q = q.offset(1);
                    for i in 0..(k - n) {
                        *q = *buf1.as_ptr().offset((n + i) as isize);
                        q = q.offset(1);
                    }
                    *q = 0;
                }
            } else if n >= -5 && n <= 0 {
                *q = b'0';
                q = q.offset(1);
                *q = b'.';
                q = q.offset(1);
                for _ in 0..(-n) {
                    *q = b'0';
                    q = q.offset(1);
                }
                std::ptr::copy_nonoverlapping(buf1.as_ptr(), q, k as usize);
                q = q.offset(k as isize);
                *q = 0;
            } else {
                // force_exp:
                *q = buf1[0];
                q = q.offset(1);
                if k > 1 {
                    *q = b'.';
                    q = q.offset(1);
                    for i in 1..k {
                        *q = *buf1.as_ptr().offset(i as isize);
                        q = q.offset(1);
                    }
                }
                *q = b'e';
                q = q.offset(1);
                let p = n - 1;
                if p >= 0 {
                    *q = b'+';
                    q = q.offset(1);
                }
                libc::sprintf(
                    q as *mut libc::c_char,
                    b"%d\0".as_ptr() as *const libc::c_char,
                    p,
                );
            }
        }
    }
}

/* 2 <= base <= 36 */
unsafe fn i64toa(buf_end: *mut u8, n: i64, base: u32) -> *mut u8 {
    let mut is_neg = 0;
    let mut n = n;

    if n < 0 {
        is_neg = 1;
        n = -n;
    }

    let mut q = buf_end;

    // C2RS: Use offset to move pointer backwards and write null terminator
    *q.offset(-1) = b'\0';
    q = q.offset(-1);

    loop {
        let digit = (n as u64) % (base as u64);
        n = (n as i64) / (base as i64);

        if digit < 10 {
            *q.offset(-1) = digit as u8 + b'0';
        } else {
            *q.offset(-1) = digit as u8 - 10 + b'a';
        }
        q = q.offset(-1);

        if n == 0 {
            break;
        }
    }

    if is_neg != 0 {
        *q.offset(-1) = b'-';
        q = q.offset(-1);
    }

    return q;
}

pub unsafe fn js_dtoa(
    ctx: *mut JSContext,
    d: f64,
    radix: i32,
    n_digits: i32,
    flags: i32,
) -> JSValue {
    let mut buf: [u8; JS_DTOA_BUF_SIZE] = [0; JS_DTOA_BUF_SIZE];
    js_dtoa1(buf.as_mut_ptr(), d, radix, n_digits, flags);
    (*ctx).create_string(CStr::from_ptr(buf.as_ptr().cast()).to_str().unwrap())
}

#[inline(always)]
pub fn to_digit(c: u8) -> i32 {
    if (b'0'..=b'9').contains(&c) {
        (c - b'0') as _
    } else if (b'A'..=b'Z').contains(&c) {
        (c - b'A' + 10) as _
    } else if (b'a'..=b'z').contains(&c) {
        (c - b'a' + 10) as _
    } else {
        36 // '$'
    }
}

pub unsafe fn js_atof(
    ctx: *mut JSContext,
    str: *const u8,
    pp: *mut *const u8,
    mut radix: i32,
    flags: i32,
) -> JSValue {
    let mut p = str;
    let mut p_start = p;

    macro_rules! fail {
        () => {
            if !pp.is_null() {
                *pp = p;
            }
            return JS_NAN;
        };
    }

    // optional separator between digits
    let mut sep = if flags & ATOD_ACCEPT_UNDERSCORES != 0 {
        '_' as i32
    } else {
        256
    };

    enum GotoLabel {
        NoRadixPrefix(bool),
    }

    let mut goto: Option<GotoLabel> = None;
    let mut is_neg: bool = false;

    // 用loop+break让goto处提前退出
    loop {
        match *p {
            b'+' => {
                p = p.offset(1);
                p_start = p_start.offset(1);
                if flags & ATOD_ACCEPT_PREFIX_AFTER_SIGN == 0 {
                    goto = Some(GotoLabel::NoRadixPrefix(false));
                    break;
                }
            }
            b'-' => {
                is_neg = true;
                p = p.offset(1);
                p_start = p_start.offset(1);
                if flags & ATOD_ACCEPT_PREFIX_AFTER_SIGN == 0 {
                    goto = Some(GotoLabel::NoRadixPrefix(true));
                    break;
                }
            }
            _ => {}
        };

        if *p == b'0' {
            let mut no_prefix = false;

            // check next char for radix
            match *p.offset(1) {
                b'x' | b'X' if radix == 0 || radix == 16 => {
                    p = p.offset(2);
                    radix = 16;
                }
                b'o' | b'O' if radix == 0 && flags & ATOD_ACCEPT_BIN_OCT != 0 => {
                    p = p.offset(2);
                    radix = 8;
                }
                b'b' | b'B' if radix == 0 && flags & ATOD_ACCEPT_BIN_OCT != 0 => {
                    p = p.offset(2);
                    radix = 2;
                }
                b'0'..=b'9' if radix == 0 && flags & ATOD_ACCEPT_LEGACY_OCTAL != 0 => {
                    let mut i = 1;
                    sep = 256;
                    while *p.offset(i) >= b'0' && *p.offset(i) <= b'7' {
                        i += 1;
                    }
                    if *p.offset(i) == b'8' || *p.offset(i) == b'9' {
                        no_prefix = true;
                    }
                    p = p.offset(1);
                    radix = 8;
                }
                _ => {
                    no_prefix = true;
                }
            }

            if !no_prefix {
                // there must be a digit after the prefix
                if to_digit(*p) >= radix {
                    fail!();
                }
            }
        } else {
            goto = Some(GotoLabel::NoRadixPrefix(is_neg));
        }

        break;
    }

    let mut atod_type = flags & ATOD_TYPE_MASK;

    while let Some(label) = goto.take() {
        match label {
            GotoLabel::NoRadixPrefix(is_neg) => {
                if flags & ATOD_INT_ONLY == 0
                    && atod_type == ATOD_TYPE_FLOAT64
                    && strstart(p, b"Infinity\0".as_ptr(), std::ptr::addr_of_mut!(p))
                {
                    let mut d = 1.0 / 0.0;
                    if is_neg {
                        d = -d;
                    }

                    if !pp.is_null() {
                        *pp = p;
                    }
                    return JS_NewFloat64(ctx, d);
                }
            }
        }
    }

    if radix == 0 {
        radix = 10;
    }

    let mut is_float = false;
    p_start = p;
    while (*p as char).is_digit(radix as _)
        || (*p == sep as u8
            && (radix != 10 || p != p_start.offset(1) || *p.offset(-1) != b'0')
            && (*p.add(1) as char).is_digit(radix as _))
    {
        p = p.add(1);
    }

    if !(flags & ATOD_INT_ONLY != 0) {
        if *p == b'.' && (p > p_start || to_digit(*p.offset(1)) < radix) {
            is_float = true;
            p = p.offset(1);
            if *p == sep as u8 {
                fail!();
            }
            while to_digit(*p) < radix || (*p == sep as u8 && to_digit(*p.offset(1)) < radix) {
                p = p.offset(1);
            }
        }
        if p > p_start
            && (((*p == b'e' || *p == b'E') && radix == 10)
                || ((*p == b'p' || *p == b'P') && (radix == 2 || radix == 8 || radix == 16)))
        {
            let mut p1 = p.offset(1);
            is_float = true;
            if *p1 == b'+' {
                p1 = p1.offset(1);
            } else if *p1 == b'-' {
                p1 = p1.offset(1);
            }
            if (*p1 as char).is_digit(radix as _) {
                p = p1.offset(1);
                while (*p as char).is_digit(radix as _)
                    || (*p == sep as u8 && (*p.add(1) as char).is_digit(radix as _))
                {
                    p = p.offset(1);
                }
            }
        }
    }

    if p == p_start {
        fail!();
    }

    let mut buf = SmallVec::<[u8; 64]>::new();
    let len = p.offset_from(p_start) as i32;
    if len + 1 > buf.capacity() as i32 {
        if buf.try_reserve_exact(len as usize + 1).is_err() {
            if !pp.is_null() {
                *pp = p;
            }
            return ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
        }
    }

    if is_neg {
        buf.push(b'-');
    }
    // remove the separators and the radix prefixes
    for i in 0..len {
        let c = *p_start.offset(i as isize);
        if c != b'_' {
            buf.push(c);
        }
    }

    if flags & ATOD_ACCEPT_SUFFIX != 0 {
        if *p == b'n' {
            p = p.offset(1);
            atod_type = ATOD_TYPE_BIG_INT;
        } else {
            if is_float && radix != 10 {
                fail!();
            }
        }
    } else {
        if atod_type == ATOD_TYPE_FLOAT64 {
            if is_float && radix != 10 {
                fail!();
            }
        }
    }

    match atod_type {
        ATOD_TYPE_FLOAT64 => {
            let s = std::str::from_utf8(buf.as_slice()).unwrap();
            let val = if is_float {
                if let Ok(f) = s.parse::<f64>() {
                    JS_NewFloat64(ctx, f)
                } else {
                    fail!();
                }
            } else {
                let len = s.len();
                if radix == 2 || len <= 8 {
                    // fast path to parse i32
                    if let Ok(i) = i32::from_str_radix(s, radix as _) {
                        JSValue::from_i32(i)
                    } else {
                        fail!();
                    }
                } else if let Ok(i) = i64::from_str_radix(s, radix as _) {
                    if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
                        JSValue::from_i32(i as i32)
                    } else {
                        JSValue::from_i64(i)
                    }
                } else {
                    fail!();
                }
            };

            if !pp.is_null() {
                *pp = p;
            }
            return val;
        }
        ATOD_TYPE_BIG_INT => {
            log::warn!("BigInt is not impleted");
            fail!();
        }
        _ => unreachable!(),
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
