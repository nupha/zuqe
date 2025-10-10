use std::{borrow::Cow, ffi::CStr, io::Write};

use crate::{context::JSContext, value::JSValue};

use super::MAX_SAFE_INTEGER;

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

pub fn f64_stringify<'a>(
    num: f64,
    radix: u32,
    fixed_digits: Option<u32>,
    flags: i32,
) -> Cow<'a, str> {
    if !num.is_finite() {
        return if num.is_nan() {
            "NaN"
        } else if num.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }

    let mut num = if num == 0.0 {
        if fixed_digits.is_none() {
            return "0".into(); // +0.0, -0.0 -> 0
        } else {
            0.0 // +0.0, -0.0 -> 0
        }
    } else {
        num
    };

    let mut use_scientific = flags & JS_DTOA_FORCE_EXP != 0
        || (num != 0.0 && {
            let ab = num.abs();
            ab >= 1e21 || ab < 1e-6
        });

    // number of digits after decimal point
    let round: Option<i32> = if let Some(p) = fixed_digits {
        debug_assert!(p >= 1);

        let scientific = format!("{:.1$e}", num, p as usize - 1);
        let i = scientific.find('e').unwrap();
        debug_assert!(i > 0);
        let exponent = scientific[i + 1..].parse::<i32>().unwrap();

        if !use_scientific {
            use_scientific = (exponent < 0 && exponent.abs() > 6) || (exponent >= p as i32);
        }

        let n = if let Some(j) = scientific[..i].find('.') {
            i - j - 1
        } else {
            i - 1
        };
        Some(n as i32)
    } else {
        None
    };

    if let Some(p) = round {
        // round num up for .5 case
        debug_assert!(p >= 0);
        if num.abs() >= 1.0 {
            let t0 = format!("{:.1$e}", num, p as usize + 1);
            let i = t0.find('e').unwrap();
            let mentissa = &t0[..i];
            let exp = t0[i + 1..].parse::<i32>().unwrap();
            if mentissa.ends_with(".5") {
                num += 5.0 * 10_f64.powi(exp - 1) * if num.is_sign_positive() { 1.0 } else { -1.0 };
            }
        }
    }

    if use_scientific {
        let t = if let Some(p) = round {
            debug_assert!(p >= 0);
            format!("{:.1$e}", num, p as usize)
        } else {
            format!("{:.e}", num)
        };

        // add '+' for positive exp
        let i = t.find('e').unwrap();
        let exp = t[i + 1..].parse::<i32>().unwrap();
        if exp >= 0 {
            format!("{}e+{}", &t[..i], exp)
        } else {
            t
        }
        .into()
    } else {
        let int64 = num as i64;

        if num != int64 as f64 || !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&int64) {
            // is a float or is an integer beyond safe range
            if let Some(p) = round {
                format!("{:.1$}", num, p as usize)
            } else {
                format!("{num}")
            }
            .into()
        } else {
            // is an integer
            let mut buf = [0u8; 33];
            unsafe {
                CStr::from_ptr(i64toa(buf.as_mut_ptr(), int64, radix).cast())
                    .to_string_lossy()
                    .into_owned()
                    .into()
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
pub unsafe fn js_dtoa(ctx: &JSContext, d: f64, radix: i32, n_digits: i32, flags: i32) -> JSValue {
    let mut buf: [u8; JS_DTOA_BUF_SIZE] = [0; JS_DTOA_BUF_SIZE];
    js_dtoa1(buf.as_mut_ptr(), d, radix, n_digits, flags);
    ctx.create_string_from_str(CStr::from_ptr(buf.as_ptr().cast()).to_str().unwrap())
        .map_or(JSValue::EXCEPTION, JSValue::String)
}

/* buf1 contains the printf result */
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
