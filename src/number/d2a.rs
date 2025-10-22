use std::{borrow::Cow, ffi::CStr, io::Write};

use crate::{context::JSContext, value::JSValue};

use super::MAX_SAFE_INTEGER;

/* radix != 10 is only supported with flags = JS_DTOA_VAR_FORMAT */
/* use as many digits as necessary */
pub const JS_DTOA_VAR_FORMAT: i32 = 0;
/// use n_digits significant digits (1 <= n_digits <= 101)
pub const JS_DTOA_FIXED_FORMAT: i32 = 1;
/// force fractional format: [-]dd.dd with n_digits fractional digits
pub const JS_DTOA_FRAC_FORMAT: i32 = 2;
/// force exponential notation either in fixed or variable format
pub const JS_DTOA_FORCE_EXP: i32 = 4;

const JS_DTOA_BUF_SIZE: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
                let f = v.fract();
                if f > 0.5 {
                    v.ceil()
                } else if f < 0.5 {
                    v.floor()
                } else {
                    v
                }
            }
            Self::FE_DOWNWARD => v.floor(),
            Self::FE_UPWARD => v.ceil(),
            Self::FE_TOWARDZERO => v.trunc(),
        }
    }

    pub fn roundn(&self, v: f64, decimals: u32) -> f64 {
        let scale = 10_f64.powi(decimals as i32);
        self.round(v * scale) / scale
    }
}

/// format an f64
pub(crate) fn f64_stringify<'a>(
    num: f64,
    radix: u32,
    fixed_digits: Option<u32>, // limit count of digits after decimal point
    flags: i32,
) -> Cow<'a, str> {
    // fast path
    if !num.is_finite() {
        return if num.is_nan() {
            "NaN"
        } else if num.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    } else if flags == JS_DTOA_VAR_FORMAT
        && num.fract() == 0.0
        && (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&(num as i64))
    {
        // safe integer
        let mut buf = [0u8; 33];
        unsafe {
            let p = i64toa(buf.as_mut_ptr(), num as i64, radix);
            return CStr::from_ptr(p.cast())
                .to_string_lossy()
                .into_owned()
                .into();
        }
    }

    let num = if num == 0.0 {
        0.0 // -0 to 0
    } else {
        num
    };

    if flags == JS_DTOA_FRAC_FORMAT {
        if let Some(p) = fixed_digits {
            format!("{:.1$}", num, p as usize)
        } else {
            format!("{num}")
        }
        .into()
    } else {
        let force_exp = flags & JS_DTOA_FORCE_EXP != 0;
        let is_fixed = flags & JS_DTOA_FIXED_FORMAT == JS_DTOA_FIXED_FORMAT;

        if force_exp {
            // force scientific notation
            f64_scientific(num, fixed_digits, true)
        } else if is_fixed {
            // fixed number of digits
            let n_max = fixed_digits.unwrap_or(21);
            debug_assert!(n_max > 0);

            let t0 = format!("{:.1$e}", num.abs(), 0);
            let mut parts = t0.splitn(2, 'e');
            let mentissa = parts.next().unwrap();
            let exp = parts.next().unwrap().parse::<i32>().unwrap();

            let n = mentissa.len() as u32 - if mentissa.contains('.') { 1 } else { 0 };
            let cnt = n as i32 + exp;

            if cnt > -6 && cnt <= n_max as i32 {
                format!("{:.1$}", num, (n_max as i32 - cnt) as usize).into()
            } else {
                // use scientific notation with round=n_max-1
                f64_scientific(num, Some(n_max - 1), true)
            }
        } else {
            // variable format
            if num == 0.0 || (1e-6..1e21).contains(&num.abs()) {
                if let Some(p) = fixed_digits {
                    format!("{:.1$}", num, p as usize)
                } else {
                    format!("{num}")
                }
                .into()
            } else {
                // use scientific notation
                f64_scientific(num, fixed_digits, true)
            }
        }
    }
}

// 'round': limit count of digits after decimal point
fn f64_scientific<'a>(num: f64, round: Option<u32>, exp_plus_sign: bool) -> Cow<'a, str> {
    let mut t = if let Some(p) = round {
        let t0 = format!("{:.1$e}", num, p as usize + 1);
        let i = t0.find('e').unwrap();
        let exp = t0[i + 1..].parse::<i32>().unwrap();
        let mentissa = &t0[0..i];

        if let Some(b'5') = mentissa.as_bytes().get(p as usize + 2) {
            // 检测是否为中间值(x.5)
            let m: f64 = num / 10_f64.powi(exp);

            let is_tie = if (1e-15..=1e15).contains(&num.abs()) {
                // fast path: 小数字使用容差比较来避免浮点数精度问题
                let factor = 10_f64.powi(p as i32);
                let scaled = m * factor;
                (scaled.fract() - 0.5).abs() < 1e-10
                    && (scaled.abs() - scaled.abs().floor() - 0.5).abs() < 1e-10
            } else {
                // slow path: 大数字使用精确的比较
                f64_round_is_tie(num, p as _)
            };

            // Note:
            // ECMAScript规定，如果四舍五入是中间值，则用round away from zero模式。
            // 否则用round to nearest
            if is_tie {
                let rnd = if num.is_sign_negative() {
                    RoundingMode::FE_DOWNWARD
                } else {
                    RoundingMode::FE_UPWARD
                };
                return format!("{:.1$}e{exp:+}", rnd.roundn(m, p), p as usize).into();
            }
        }

        // round to nearest
        format!("{:.1$e}", num, p as usize)
    } else {
        format!("{num:e}")
    };

    if exp_plus_sign {
        // add '+' for positive exp
        let i = t.find('e').unwrap();
        let exp = t[i + 1..].parse::<i32>().unwrap();
        if exp >= 0 {
            t = format!("{}e+{}", &t[..i], exp)
        }
    }

    t.into()
}

/// 检测四舍五入是否为中间值
fn f64_round_is_tie(num: f64, precision: usize) -> bool {
    // 1. 使用字符串表示来精确解析数字
    let num_str = format!("{:.16e}", num);
    let mut parts = num_str.splitn(2, 'e');
    let base_part = parts.next().unwrap();
    // let exponent_part = parts.next().unwrap_or("0");

    // 2. 解析尾数和指数
    // let exponent = exponent_part.parse::<i32>().unwrap_or(0);
    let mut base_parts = base_part.splitn(2, '.');
    let int_part = base_parts.next().unwrap();
    let dec_part = base_parts.next().unwrap_or("0");

    // 3. 重建精确的尾数
    let full_mantissa = format!("{}{}", int_part, dec_part);
    let mantissa = full_mantissa.parse::<f64>().unwrap_or(0.0) / 10_f64.powi(dec_part.len() as i32);
    let mantissa = if num < 0.0 { -mantissa } else { mantissa };

    // 4. 计算缩放后的值
    let factor = 10_f64.powi(precision as i32);
    let scaled = mantissa * factor;

    // 5. 更精确的中间值检测
    let is_tie = {
        // 获取需要判断的位
        let digits = format!("{:.20}", scaled.abs());
        let mut digit_parts = digits.split('.');
        let _int_part = digit_parts.next().unwrap_or("0");
        let dec_part = digit_parts.next().unwrap_or("0");

        // 检查是否正好是x.50000...
        let is_half = dec_part.starts_with("5") && dec_part.chars().skip(1).all(|c| c == '0');

        // 检查整数部分是否是整数
        let is_whole = scaled.fract().abs() < 1e-10;

        is_half && is_whole
    };

    is_tie
}

/* 2 <= base <= 36 */
unsafe fn i64toa(buf: *mut u8, n: i64, base: u32) -> *mut u8 {
    let mut is_neg = 0;
    let mut n = n;

    if n < 0 {
        is_neg = 1;
        n = -n;
    }

    let mut q = buf;

    // C2RS: Use offset to move pointer backwards and write null terminator
    *q.offset(-1) = b'\0';
    q = q.offset(-1);

    loop {
        let digit = (n as u64) % (base as u64);
        n /= base as i64;

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

    q
}

/* XXX: slow and maybe not fully correct. Use libbf when it is fast enough.
   XXX: radix != 10 is only supported for small integers
*/
#[deprecated]
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
        let int64 = d as i64;
        if d != int64 as f64 || !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&int64) {
            let is_fixed = flags & 3 == JS_DTOA_FIXED_FORMAT;
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
                libc::sprintf(q.cast(), c"%d".as_ptr(), p);
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
            } else if (-5..=0).contains(&n) {
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
                libc::sprintf(q as *mut libc::c_char, c"%d".as_ptr(), p);
            }
        } else {
            ptr = i64toa(buf1.as_mut_ptr().offset(69), int64, radix as u32);
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
                libc::sprintf(q as *mut libc::c_char, c"%d".as_ptr(), p);
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
            } else if (-5..=0).contains(&n) {
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
                libc::sprintf(q as *mut libc::c_char, c"%d".as_ptr(), p);
            }
        }
    }
}

#[deprecated(note = "use f64_stringify() instead")]
unsafe fn js_dtoa(ctx: &JSContext, d: f64, radix: i32, n_digits: i32, flags: i32) -> JSValue {
    let mut buf: [u8; JS_DTOA_BUF_SIZE] = [0; JS_DTOA_BUF_SIZE];
    js_dtoa1(buf.as_mut_ptr(), d, radix, n_digits, flags);
    ctx.create_string_from_str(CStr::from_ptr(buf.as_ptr().cast()).to_str().unwrap())
        .map_or(JSValue::EXCEPTION, JSValue::String)
}

/* buf1 contains the printf result */
#[deprecated(note = "use f64_stringify() instead")]
unsafe fn js_ecvt1(
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

    *sign = if *buf1 == b'-' { 1 } else { 0 };

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
#[deprecated(note = "use f64_stringify() instead")]
unsafe fn js_ecvt(
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
    n_digits
}

#[deprecated(note = "use f64_stringify() instead")]
unsafe fn js_fcvt(buf: *mut u8, buf_size: usize, d: f64, n_digits: i32) {
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

unsafe fn js_fcvt1(
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
    n
}
