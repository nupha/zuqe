use smallvec::SmallVec;

use crate::{context::JSContext, error::JSErrorEnum, string::strstart, value::JSValue};

pub const ATOD_INT_ONLY: i32 = 1 << 0;
/* accept Oo and Ob prefixes in addition to 0x prefix if radix = 0 */
pub const ATOD_ACCEPT_BIN_OCT: i32 = 1 << 2;
/* accept O prefix as octal if radix == 0 and properly formed (Annex B) */
pub const ATOD_ACCEPT_LEGACY_OCTAL: i32 = 1 << 4;
/* accept _ between digits as a digit separator */
pub const ATOD_ACCEPT_UNDERSCORES: i32 = 1 << 5;
/* allow a suffix to override the type */
pub const ATOD_ACCEPT_SUFFIX: i32 = 1 << 6;
/* default type */
pub const ATOD_TYPE_MASK: i32 = 3 << 7;
pub const ATOD_TYPE_FLOAT64: i32 = 0;
pub const ATOD_TYPE_BIG_INT: i32 = 1 << 7;
/// accept -0x1
pub const ATOD_ACCEPT_PREFIX_AFTER_SIGN: i32 = 1 << 10;

#[inline(always)]
pub const fn to_digit(c: u8) -> i32 {
    if c.is_ascii_digit() {
        (c - b'0') as _
    } else if c.is_ascii_uppercase() {
        (c - b'A' + 10) as _
    } else if c.is_ascii_lowercase() {
        (c - b'a' + 10) as _
    } else {
        36 // '$'
    }
}

pub unsafe fn js_atof(
    ctx: &JSContext,
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
            return JSValue::Float64(f64::NAN);
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

    match *p {
        b'+' => {
            p = p.offset(1);
            p_start = p_start.offset(1);
            if flags & ATOD_ACCEPT_PREFIX_AFTER_SIGN == 0 {
                goto = Some(GotoLabel::NoRadixPrefix(false));
            }
        }
        b'-' => {
            is_neg = true;
            p = p.offset(1);
            p_start = p_start.offset(1);
            if flags & ATOD_ACCEPT_PREFIX_AFTER_SIGN == 0 {
                goto = Some(GotoLabel::NoRadixPrefix(true));
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

    let mut atod_type = flags & ATOD_TYPE_MASK;

    while let Some(label) = goto.take() {
        match label {
            GotoLabel::NoRadixPrefix(is_neg) => {
                if flags & ATOD_INT_ONLY == 0
                    && atod_type == ATOD_TYPE_FLOAT64
                    && strstart(p, c"Infinity".as_ptr().cast(), std::ptr::addr_of_mut!(p))
                {
                    let mut d = 1.0 / 0.0;
                    if is_neg {
                        d = -d;
                    }

                    if !pp.is_null() {
                        *pp = p;
                    }
                    return JSValue::from(d);
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

    if flags & ATOD_INT_ONLY == 0 {
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
            if matches!(*p1, b'+' | b'-') {
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
            ctx.JS_ThrowOutOfMemory();
            return JSValue::EXCEPTION;
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
        } else if is_float && radix != 10 {
            fail!();
        }
    } else if atod_type == ATOD_TYPE_FLOAT64 {
        if is_float && radix != 10 {
            fail!();
        }
    }

    match atod_type {
        ATOD_TYPE_FLOAT64 => {
            let s = std::str::from_utf8(buf.as_slice()).unwrap();
            let val = if is_float {
                if let Ok(f) = s.parse::<f64>() {
                    JSValue::from(f)
                } else {
                    fail!();
                }
            } else {
                let len = s.len();
                if radix == 2 || len <= 8 {
                    // fast path to parse i32
                    if let Ok(i) = i32::from_str_radix(s, radix as _) {
                        JSValue::Int(i)
                    } else {
                        fail!();
                    }
                } else if let Ok(i) = i64::from_str_radix(s, radix as _) {
                    if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
                        JSValue::Int(i as i32)
                    } else {
                        JSValue::from(i)
                    }
                } else {
                    fail!();
                }
            };

            if !pp.is_null() {
                *pp = p;
            }
            val
        }
        ATOD_TYPE_BIG_INT => {
            // fail!();
            ctx.JS_ThrowError(JSErrorEnum::Syntax, "big int is not supported");
            if !pp.is_null() {
                *pp = p;
            }
            JSValue::EXCEPTION
        }

        _ => unreachable!(),
    }
}
