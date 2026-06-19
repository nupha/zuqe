// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ScopedContext,
    builtins::number::{js_parseFloat, js_parseInt},
    error::JSResult,
    eval::{EvalFlags, EvalType},
    function::{CallContext, FieldDef},
    string::{CharCode, CharCodeDecoder, StringBuf},
    utils::unlikely,
    value::JSValue,
};

pub(crate) fn js_global_eval(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let global = ctx.global_obj();
    let val = cc.arg(0);

    ctx.with_new_scope(|mut cx| {
        let res = val.eval_object(
            &mut cx,
            global.into(),
            EvalType::Indirect,
            EvalFlags::empty(),
            -1,
        );
        cx.return_with(res)
    })
}

pub const MAGIC_NUM_IS_NAN: i32 = 1;
pub const MAGIC_NUM_IS_FINITE: i32 = 2;

pub(crate) fn js_global_isNaN_isFinite(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let d = cc.arg(0).to_number(ctx)?.to_f64();
    let b = match cc.magic() {
        MAGIC_NUM_IS_NAN => f64::is_nan(d),
        MAGIC_NUM_IS_FINITE => f64::is_finite(d),
        _ => unreachable!(),
    };
    Ok(JSValue::Bool(b))
}

/// ES spec: uriReserved (used by decodeURI — does NOT include '#')
#[inline(always)]
const fn is_uri_reserved(c: u32) -> bool {
    matches!(
        c as u8,
        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b','
    ) && c < 128
}

/// ES spec: uriReserved + '#' (used by encodeURI)
#[inline(always)]
const fn is_uri_reserved_or_hash(c: u32) -> bool {
    is_uri_reserved(c) || c == b'#' as u32
}

/// ES spec: uriUnescaped = ALPHA | DIGIT | "-" | "_" | "." | "!" | "~" | "*" | "'" | "(" | ")"
#[inline(always)]
const fn is_uri_unescaped(c: u32) -> bool {
    matches!(
        c as u8,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
    ) && c < 128
}

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

fn js_global_decodeURI(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_component = cc.magic() == 1;

    let str = cc.arg(0).to_string(ctx)?;
    let len = str.len();

    let mut buf = StringBuf::with_capacity(len, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    let mut char_it = str.content().chars();
    while let Some((_, ch)) = char_it.next() {
        match ch {
            CharCode::Char('%') => {
                // first %XX byte
                let mut d = 0u32;
                let mut hh = [0u8; 2];
                for x in hh.iter_mut() {
                    let (a, n) = match char_it.next().and_then(|(_, x)| x.char()) {
                        Some(a @ '0'..='9') => (a, a as u32 - '0' as u32),
                        Some(a @ 'A'..='F') => (a, a as u32 - 'A' as u32 + 10),
                        Some(a @ 'a'..='f') => (a, a as u32 - 'a' as u32 + 10),
                        _ => {
                            ctx.throw_uri_error("expecting hex digit");
                            return Err(-1);
                        }
                    };
                    d = d << 4 | n;
                    *x = a as u8;
                }
                debug_assert!(d <= 0xff);

                if d < 0x80 {
                    // ascii
                    if !is_component && is_uri_reserved(d as _) {
                        // for non-component, reserved chars are kept in %XX format.
                        buf.push_u8_slice([b'%', hh[0], hh[1]].as_slice())
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                    } else {
                        buf.push_u8(d as u8).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                    }
                } else {
                    // decode utf8 sequence with following bytes
                    let (reminder, c_min, mut uc) = if (0xc0..=0xdf).contains(&d) {
                        (1, 0x80, d & 0x1f)
                    } else if (0xe0..=0xef).contains(&d) {
                        (2, 0x800, d & 0xf)
                    } else if (0xf0..=0xf7).contains(&d) {
                        (3, 0x10000, d & 0x7)
                    } else {
                        ctx.throw_uri_error("malformed UTF-8");
                        return Err(-1);
                    };

                    for _ in 0..reminder {
                        if let Some((_, CharCode::Char('%'))) = char_it.next() {
                            // hex %XX
                            let mut d = 0u8;
                            for _ in 0..2 {
                                let n = match char_it.next().and_then(|(_, x)| x.char()) {
                                    Some(a @ '0'..='9') => a as u8 - b'0',
                                    Some(a @ 'A'..='F') => a as u8 - b'A' + 10,
                                    Some(a @ 'a'..='f') => a as u8 - b'a' + 10,
                                    _ => {
                                        ctx.throw_uri_error("expecting hex digit");
                                        return Err(-1);
                                    }
                                };
                                d = d << 4 | n;
                            }

                            if d & 0xc0 != 0x80 {
                                ctx.throw_uri_error("malformed UTF-8");
                                return Err(-1);
                            }
                            uc = (uc << 6) | (d as u32 & 0x3f);
                        } else {
                            // expect '%'
                            ctx.throw_uri_error("malformed UTF-8");
                            return Err(-1);
                        }
                    }

                    if unlikely(uc < c_min || uc > 0x10FFFF || (0xd800..0xe000).contains(&uc)) {
                        ctx.throw_uri_error("malformed UTF-8");
                        return Err(-1);
                    } else {
                        buf.push_u32(uc).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                    }
                }
            }
            CharCode::Char(c) => {
                buf.push_char(c).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            CharCode::Code(_) => {
                buf.push_u16_slice(ch.encode_utf16(&mut [0; 2]))
                    .map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
            }
        }
    }

    ctx.create_string_with_buf(buf).map(|s| s.into())
}

/// ES spec: encodeURI (19.2.6.3) / encodeURIComponent (19.2.6.4)
fn js_global_encodeURI(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_component = cc.magic() == 1;

    let str = cc.arg(0).to_string(ctx)?;
    let len = str.len();

    // Worst case: every code unit expands to 3 percent-encoded bytes (%XX)
    let mut buf = StringBuf::with_capacity(len * 3, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    let mut char_it = str.content().chars();
    while let Some((_, ch)) = char_it.next() {
        match ch {
            CharCode::Char(c) => {
                let cp = c as u32;
                // uriUnescaped is always kept; encodeURI also keeps uriReserved and '#'
                if is_uri_unescaped(cp) || (!is_component && is_uri_reserved_or_hash(cp)) {
                    buf.push_u8(c as u8).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                } else {
                    // encode as UTF-8 percent-encoded bytes
                    let mut utf8_buf = [0u8; 4];
                    let bytes = c.encode_utf8(&mut utf8_buf).as_bytes();
                    for &b in bytes {
                        buf.push_u8_slice(&[
                            b'%',
                            HEX_UPPER[(b >> 4) as usize],
                            HEX_UPPER[(b & 0xf) as usize],
                        ])
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                    }
                }
            }
            CharCode::Code(_) => {
                // broken surrogate → URIError
                ctx.throw_uri_error("malformed URI");
                return Err(-1);
            }
        }
    }

    ctx.create_string_with_buf(buf).map(|s| s.into())
}

/// ES spec Annex B.2.1.1: escape(string)
///
/// Unescaped set: `A-Z a-z 0-9 @ * _ + - . /`
/// Code point < 256 → `%XX`; code point >= 256 → `%uXXXX`
fn js_global_escape(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let str = cc.arg(0).to_string(ctx)?;
    let len = str.len();

    let mut buf = StringBuf::with_capacity(len * 3, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    for (_, ch) in str.content().chars() {
        let cp = match ch {
            CharCode::Char(c) => c as u32,
            CharCode::Code(v) => v, // broken surrogate → encode as %uXXXX
        };

        if matches!(
            cp as u8,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'@'
                | b'*'
                | b'_'
                | b'+'
                | b'-'
                | b'.'
                | b'/'
        ) && cp < 128
        {
            buf.push_u8(cp as u8).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        } else if cp < 256 {
            buf.push_u8_slice(&[
                b'%',
                HEX_UPPER[(cp >> 4) as usize],
                HEX_UPPER[(cp & 0xf) as usize],
            ])
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        } else {
            buf.push_u8_slice(&[
                b'%',
                b'u',
                HEX_UPPER[((cp >> 12) & 0xf) as usize],
                HEX_UPPER[((cp >> 8) & 0xf) as usize],
                HEX_UPPER[((cp >> 4) & 0xf) as usize],
                HEX_UPPER[(cp & 0xf) as usize],
            ])
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
    }

    ctx.create_string_with_buf(buf).map(|s| s.into())
}

#[inline(always)]
const fn hex_val(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        'A'..='F' => Some(c as u32 - 'A' as u32 + 10),
        'a'..='f' => Some(c as u32 - 'a' as u32 + 10),
        _ => None,
    }
}

/// ES spec Annex B.2.1.2: unescape(string)
///
/// Decodes `%uXXXX` (4-digit hex) and `%XX` (2-digit hex) sequences.
fn js_global_unescape(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let str = cc.arg(0).to_string(ctx)?;
    let len = str.len();

    let mut buf = StringBuf::with_capacity(len, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    let mut it = str.content().chars();
    while let Some((_, ch)) = it.next() {
        match ch {
            CharCode::Char('%') => {
                let base = it.pos().unwrap_or(len);
                // peek: is next char 'u'?
                let next = it.next();
                let is_u = matches!(next, Some((_, CharCode::Char('u'))));

                if is_u {
                    // try %uXXXX: read 4 hex digits
                    let h = [
                        it.next().and_then(|(_, c)| c.char()).and_then(hex_val),
                        it.next().and_then(|(_, c)| c.char()).and_then(hex_val),
                        it.next().and_then(|(_, c)| c.char()).and_then(hex_val),
                        it.next().and_then(|(_, c)| c.char()).and_then(hex_val),
                    ];
                    if let [Some(h0), Some(h1), Some(h2), Some(h3)] = h {
                        let cp = (h0 << 12) | (h1 << 8) | (h2 << 4) | h3;
                        buf.push_u16(cp as u16).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                        continue;
                    }
                } else {
                    // try %XX: read first hex digit
                    let h0 = next.and_then(|(_, c)| c.char()).and_then(hex_val);
                    if let Some(h0) = h0 {
                        let h1 = it.next().and_then(|(_, c)| c.char()).and_then(hex_val);
                        if let Some(h1) = h1 {
                            let cp = (h0 << 4) | h1;
                            buf.push_u16(cp as u16).map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                            continue;
                        }
                    }
                }

                // no valid escape → output literal '%', restore position to just after '%'
                buf.push_u8(b'%').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                it.set_pos(base);
            }
            CharCode::Char(c) => {
                buf.push_char(c).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            CharCode::Code(v) => {
                buf.push_u16(v as u16).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
        }
    }

    ctx.create_string_with_buf(buf).map(|s| s.into())
}

pub const js_global_props: &[FieldDef] = &[
    FieldDef::FUNC("parseInt", 2, js_parseInt, 0),
    FieldDef::FUNC("parseFloat", 1, js_parseFloat, 0),
    FieldDef::FUNC("isNaN", 1, js_global_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    FieldDef::FUNC(
        "isFinite",
        1,
        js_global_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _,
    ),
    FieldDef::FUNC("decodeURI", 1, js_global_decodeURI, 0),
    FieldDef::FUNC("decodeURIComponent", 1, js_global_decodeURI, 1),
    FieldDef::FUNC("encodeURI", 1, js_global_encodeURI, 0),
    FieldDef::FUNC("encodeURIComponent", 1, js_global_encodeURI, 1),
    FieldDef::FUNC("escape", 1, js_global_escape, 0),
    FieldDef::FUNC("unescape", 1, js_global_unescape, 0),
    FieldDef::FLOAT64("Infinity", f64::INFINITY, 0),
    FieldDef::FLOAT64("NaN", f64::NAN, 0),
    FieldDef::UNDEFINED("undefined", 0),
];
