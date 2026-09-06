// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use smallvec::SmallVec;

use crate::{
    context::ScopedContext,
    number::{IntOrFloat, NumValue},
    utils::unlikely,
};

#[cfg(feature = "big_int")]
use crate::bigint::parse_str as bigint_parse_str;

pub const ATOD_INT_ONLY: i32 = 1 << 0;
// accept Oo and Ob prefixes in addition to 0x prefix if radix = 0
pub const ATOD_ACCEPT_BIN_OCT: i32 = 1 << 2;
// accept O prefix as octal if radix == 0 and properly formed (Annex B)
pub const ATOD_ACCEPT_LEGACY_OCTAL: i32 = 1 << 4;
// accept _ between digits as a digit separator
pub const ATOD_ACCEPT_UNDERSCORES: i32 = 1 << 5;
// allow a suffix to override the type
pub const ATOD_ACCEPT_SUFFIX: i32 = 1 << 6;
// default type
pub const ATOD_TYPE_MASK: i32 = 3 << 7;
pub const ATOD_TYPE_FLOAT64: i32 = 0;
pub const ATOD_TYPE_BIG_INT: i32 = 1 << 7;
/// accept -0x1
pub const ATOD_ACCEPT_PREFIX_AFTER_SIGN: i32 = 1 << 10;

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

/// parse str to i64
pub(crate) fn parse_i64(txt: &str) -> Option<i64> {
    let (str, minus) = if let Some(s) = txt.strip_prefix('-') {
        (s, true)
    } else {
        (txt, false)
    };

    let len = str.len();
    if len == 0 {
        return None;
    }

    let mut num: i64 = 0;

    for (i, ch) in str.char_indices() {
        if ch.is_ascii_digit() {
            if i == 0 && ch == '0' {
                // starting with '0'
                return if len != 1 { None } else { Some(0) };
            }

            let n = ch as i64 - '0' as i64;
            let a = num.checked_mul(10).and_then(|x| x.checked_add(n))?;
            num = a;
        } else if i > 0 && ch == '_' {
            //skip '_'
        } else {
            return None;
        }
    }

    if minus {
        num.checked_mul(-1)
    } else {
        Some(num)
    }
}

/// parse str to u32
pub(crate) fn parse_u32(str: &str) -> Option<u32> {
    let len = str.len();
    if len == 0 || len > 10 {
        return None;
    }

    let mut num: u32 = 0;

    for (i, ch) in str.char_indices() {
        if ch.is_ascii_digit() {
            if i == 0 && ch == '0' {
                // starting with '0'
                return if len != 1 { None } else { Some(0) };
            }

            let n = ch as u64 - '0' as u64;
            let n64: u64 = (num as u64) * 10 + n;
            if n64 >> 32 != 0 {
                return None; // u32 overflow
            }
            num = n64 as u32;
        } else if i > 0 && ch == '_' {
            //skip '_'
        } else {
            return None;
        }
    }

    Some(num)
}

/// Parse number literal.
///
/// Returns parsed number and consumed bytes
pub(crate) fn js_aton(
    ctx: &mut ScopedContext,
    bytes: &[u8],
    mut radix: i32,
    flags: i32,
) -> Result<(NumValue, usize), i32> {
    macro_rules! fail {
        () => {
            return Err(ctx.throw_type_error("invalid number literal"));
        };
    }

    // Track sign and whether we must skip radix-prefix parsing
    // and go directly to "no radix prefix" handling (Infinity, etc.).
    let mut check_infinity = false;
    let mut is_neg = false;
    let mut idx: usize = 0;

    // Optional leading sign
    match bytes.get(idx).copied() {
        Some(b'+') => {
            idx += 1;
            if flags & ATOD_ACCEPT_PREFIX_AFTER_SIGN == 0 {
                check_infinity = true;
            }
        }
        Some(b'-') => {
            is_neg = true;
            idx += 1;
            if flags & ATOD_ACCEPT_PREFIX_AFTER_SIGN == 0 {
                check_infinity = true;
            }
        }
        _ => {}
    };

    // Optional numeric separator (underscore) accepted when flag is set
    let mut sep = if flags & ATOD_ACCEPT_UNDERSCORES != 0 {
        '_' as i32
    } else {
        256
    };

    // Optional radix prefixes (0x / 0o / 0b / legacy octal) when allowed.
    if !check_infinity {
        if matches!(bytes.get(idx), Some(b'0')) {
            let mut no_prefix = false;

            match bytes.get(idx + 1).copied() {
                Some(b'x' | b'X') if radix == 0 || radix == 16 => {
                    idx += 2;
                    radix = 16;
                }
                Some(b'o' | b'O') if radix == 0 && flags & ATOD_ACCEPT_BIN_OCT != 0 => {
                    idx += 2;
                    radix = 8;
                }
                Some(b'b' | b'B') if radix == 0 && flags & ATOD_ACCEPT_BIN_OCT != 0 => {
                    idx += 2;
                    radix = 2;
                }
                Some(b'0'..=b'9') if radix == 0 && flags & ATOD_ACCEPT_LEGACY_OCTAL != 0 => {
                    let mut i = 1usize;
                    sep = 256;
                    while matches!(bytes.get(idx + i), Some(b'0'..=b'7')) {
                        i += 1;
                    }
                    if matches!(bytes.get(idx + i), Some(b'8' | b'9')) {
                        no_prefix = true;
                    } else {
                        idx += 1;
                        radix = 8;
                    }
                }
                _ => {
                    no_prefix = true;
                }
            }

            if !no_prefix {
                if let Some(c) = bytes.get(idx).copied() {
                    if to_digit(c) >= radix {
                        fail!();
                    }
                } else {
                    fail!();
                }
            }
        } else {
            check_infinity = true;
        }
    }

    // Decide number representation kind (float64 vs bigint)
    let mut atod_type = flags & ATOD_TYPE_MASK;

    // Handle "Infinity" / "-Infinity" literal when we are in
    // the "no radix prefix" state.
    if check_infinity
        && flags & ATOD_INT_ONLY == 0
        && atod_type == ATOD_TYPE_FLOAT64
        && bytes[idx..].starts_with(b"Infinity")
    {
        return Ok((
            NumValue::Float(IntOrFloat::from(if is_neg {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            })),
            idx + "Infinity".len(),
        ));
    }

    if radix == 0 {
        radix = 10;
    }

    let mut is_float = false;
    let start_idx = idx;

    // Scan integer part: digits plus optional underscore separators.
    while let Some(c) = bytes.get(idx).copied() {
        let is_digit = (c as char).is_digit(radix as _);

        let is_sep = if c as i32 == sep {
            let next_is_digit = bytes
                .get(idx + 1)
                .copied()
                .map(|next| (next as char).is_digit(radix as _))
                .unwrap_or(false);

            if !next_is_digit {
                false
            } else if radix != 10 || idx != start_idx + 1 {
                true
            } else {
                bytes.get(idx - 1).copied() != Some(b'0')
            }
        } else {
            false
        };

        // Stop when we see the first non-digit and non-separator.
        if !(is_digit || is_sep) {
            break;
        }

        idx += 1;
    }

    if flags & ATOD_INT_ONLY == 0 {
        // Fractional part: optional '.' followed by digits/separators.
        if matches!(bytes.get(idx), Some(b'.'))
            && (idx > start_idx
                || bytes
                    .get(idx + 1)
                    .copied()
                    .map(|c| to_digit(c) < radix)
                    .unwrap_or(false))
        {
            is_float = true;
            idx += 1;
            if bytes
                .get(idx)
                .copied()
                .map(|c| c as i32 == sep)
                .unwrap_or(false)
            {
                fail!();
            }

            while bytes
                .get(idx)
                .copied()
                .map(|c| to_digit(c) < radix)
                .unwrap_or(false)
                || (matches!(bytes.get(idx), Some(c) if *c as i32 == sep)
                    && bytes
                        .get(idx + 1)
                        .copied()
                        .map(|c| to_digit(c) < radix)
                        .unwrap_or(false))
            {
                idx += 1;
            }
        }

        // Exponent part: 'e'/'E' for decimal, 'p'/'P' for non-decimal floats.
        if idx > start_idx
            && ((matches!(bytes.get(idx), Some(b'e' | b'E')) && radix == 10)
                || (matches!(bytes.get(idx), Some(b'p' | b'P'))
                    && (radix == 2 || radix == 8 || radix == 16)))
        {
            let mut p1 = idx + 1;
            is_float = true;
            if matches!(bytes.get(p1), Some(b'+' | b'-')) {
                p1 += 1;
            }
            if matches!(bytes.get(p1).copied(), Some(c) if (c as char).is_digit(radix as _)) {
                idx = p1 + 1;
                while matches!(bytes.get(idx).copied(), Some(c) if (c as char).is_digit(radix as _))
                    || (matches!(bytes.get(idx), Some(c) if *c as i32 == sep)
                        && matches!(bytes.get(idx + 1).copied(), Some(n) if (n as char).is_digit(radix as _)))
                {
                    idx += 1;
                }
            }
        }
    }

    // Require at least one digit overall.
    if idx == start_idx {
        fail!();
    }

    // Build normalized digit buffer (no separators, optional leading '-').
    let mut buf = SmallVec::<[u8; 64]>::new();
    let len_digits = (idx - start_idx) as i32;

    if len_digits + 1 > buf.capacity() as i32
        && buf.try_reserve_exact(len_digits as usize + 1).is_err()
    {
        return Err(ctx.throw_out_of_memory());
    }

    if is_neg {
        buf.push(b'-');
    }

    for i in 0..len_digits {
        let c = bytes[start_idx + i as usize];
        if c != b'_' {
            buf.push(c);
        }
    }

    // Optional suffix 'n' for BigInt and validation for non-decimal floats.
    if flags & ATOD_ACCEPT_SUFFIX != 0 {
        if matches!(bytes.get(idx), Some(b'n')) {
            // BigInt literal must not contain an ExponentPart or DecimalPoint.
            if is_float {
                fail!();
            }
            idx += 1;
            atod_type = ATOD_TYPE_BIG_INT;
        } else if is_float && radix != 10 {
            fail!();
        }
    } else if atod_type == ATOD_TYPE_FLOAT64 && is_float && radix != 10 {
        fail!();
    }

    match atod_type {
        ATOD_TYPE_FLOAT64 => {
            let txt = std::str::from_utf8(buf.as_slice()).unwrap();

            // Non-decimal integer: parse via integer path and keep IntOrFloat precise.
            if !is_float && radix != 10 {
                if let Ok(i) = i64::from_str_radix(txt, radix as _) {
                    let v = if unlikely(i == 0 && is_neg) {
                        IntOrFloat::Float64(-0.0)
                    } else {
                        IntOrFloat::Int64(i)
                    };
                    Ok((NumValue::Float(v), idx))
                } else if !is_neg && let Ok(u) = u64::from_str_radix(txt, radix as _) {
                    Ok((NumValue::Float(IntOrFloat::from(u as f64)), idx))
                } else if let Ok(i) = i128::from_str_radix(txt, radix as _) {
                    Ok((NumValue::Float(IntOrFloat::Float64(i as f64)), idx))
                } else {
                    fail!();
                }
            } else {
                // Decimal float or anything that went through fractional/exponent path.
                if let Ok(f) = txt.parse::<f64>() {
                    Ok((NumValue::Float(IntOrFloat::from(f)), idx))
                } else {
                    fail!();
                }
            }
        }
        ATOD_TYPE_BIG_INT => {
            #[cfg(feature = "big_int")]
            {
                let txt = std::str::from_utf8(buf.as_slice()).unwrap();
                let bi = bigint_parse_str(ctx, txt, radix as u32)?;
                Ok((NumValue::BigInt(bi), idx))
            }
            #[cfg(not(feature = "big_int"))]
            {
                Err(ctx.throw_type_error("big int is not supported"))
            }
        }

        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;

    fn with_ctx(f: impl FnOnce(&mut ScopedContext)) {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt.create_context(|_| Ok(())).unwrap();
        ctx.with_new_scope(|scope| {
            f(scope);
        });
    }

    /// Same sentinel bug in the slice-bounded `js_aton`: an embedded NUL byte
    /// must end the number (parseFloat/parseInt prefix semantics), never act
    /// as a digit separator.
    #[test]
    fn js_aton_nul_is_not_a_separator() {
        with_ctx(|ctx| {
            let (v, consumed) = js_aton(ctx, &[b'1', 0, b'5'], 10, 0).unwrap();
            assert_eq!(consumed, 1);
            assert_eq!(v, NumValue::Float(IntOrFloat::Int64(1)));

            let (v, consumed) = js_aton(ctx, &[b'1', b'2', 0, b'3'], 10, 0).unwrap();
            assert_eq!(consumed, 2);
            assert_eq!(v, NumValue::Float(IntOrFloat::Int64(12)));

            // with separators enabled, NUL still terminates, '_' still separates
            let (v, consumed) = js_aton(ctx, b"1_2\x003", 0, ATOD_ACCEPT_UNDERSCORES).unwrap();
            assert_eq!(consumed, 3);
            assert_eq!(v, NumValue::Float(IntOrFloat::Int64(12)));
        });
    }

    /// Round-trip test for `NumValue`: `n`-suffixed literals must produce a
    /// `NumValue::BigInt` with the correct magnitude/sign and consumed length.
    #[cfg(feature = "big_int")]
    #[test]
    fn js_aton_parses_bigint_literals() {
        use crate::bigint::to_i64_exact;

        const FLAGS: i32 = ATOD_ACCEPT_BIN_OCT
            | ATOD_ACCEPT_LEGACY_OCTAL
            | ATOD_ACCEPT_UNDERSCORES
            | ATOD_ACCEPT_SUFFIX;

        fn assert_bigint(ctx: &mut ScopedContext, s: &str, expect: i64) {
            let (v, consumed) = js_aton(ctx, s.as_bytes(), 0, FLAGS).unwrap();
            assert_eq!(consumed, s.len(), "consumed length for literal {}", s);
            match v {
                NumValue::Float(_) => panic!("expected BigInt for literal {}", s),
                NumValue::BigInt(b) => {
                    assert_eq!(
                        to_i64_exact(&b),
                        Some(expect),
                        "value mismatch for literal {}",
                        s
                    );
                }
            }
        }

        with_ctx(|ctx| {
            assert_bigint(ctx, "123n", 123);
            assert_bigint(ctx, "0n", 0);
            assert_bigint(ctx, "-0n", 0);
            assert_bigint(ctx, "-5n", -5);
            assert_bigint(ctx, "0x10n", 16);
            assert_bigint(ctx, "0b1010n", 10);
            assert_bigint(ctx, "0o777n", 511);
            assert_bigint(ctx, "1_000n", 1000);
            assert_bigint(ctx, "1_2_3_4n", 1234);

            // beyond i64 → large BigInt, sign preserved
            let big = "123456789012345678901234567890n";
            let (v, consumed) = js_aton(ctx, big.as_bytes(), 0, FLAGS).unwrap();
            assert_eq!(consumed, big.len());
            match v {
                NumValue::Float(_) => panic!("expected BigInt for large literal"),
                NumValue::BigInt(b) => {
                    let i = b.as_impl();
                    assert!(!i.sign());
                    assert!(i.limbs.len() > 1);
                }
            }

            let neg = "-123456789012345678901234567890n";
            let (v, consumed) = js_aton(ctx, neg.as_bytes(), 0, FLAGS).unwrap();
            assert_eq!(consumed, neg.len());
            match v {
                NumValue::Float(_) => panic!("expected BigInt for negative large literal"),
                NumValue::BigInt(b) => assert!(b.sign()),
            }
        });
    }
}
