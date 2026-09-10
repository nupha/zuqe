// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Implements Uint8Array base64/hex APIs per ES2025 proposal.
//! - Uint8Array.fromBase64(string[, options])
//! - Uint8Array.fromHex(string)
//! - Uint8Array.prototype.toBase64([options])
//! - Uint8Array.prototype.toHex()
//! - Uint8Array.prototype.setFromBase64(string[, options])
//! - Uint8Array.prototype.setFromHex(string)

use crate::{
    context::ScopedContext, error::JSResult, function::CallContext, object::TypedArray,
    value::JSValue,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

use crate::object::JS_PROP_C_W_E;

const STD_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

// ---------------------------------------------------------------------------
// Character classification
// ---------------------------------------------------------------------------

#[inline]
#[allow(dead_code)]
const fn is_std_b64(ch: u8) -> bool {
    matches!(ch, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/')
}

#[allow(dead_code)]
const fn is_url_b64(ch: u8) -> bool {
    matches!(ch, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_')
}

#[inline]
const fn is_ascii_whitespace(ch: u8) -> bool {
    matches!(ch, b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x0B)
}

#[inline]
const fn hex_nibble(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(ch - b'a' + 10),
        b'A'..=b'F' => Some(ch - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Options helpers
// ---------------------------------------------------------------------------

/// Get a string option from an options object. Returns the raw primitive value
/// (not toString-coerced). String objects throw TypeError per spec.
fn get_option_str(
    ctx: &mut ScopedContext,
    options: JSValue,
    key: &str,
    default: &str,
) -> Result<String, i32> {
    if options.is_undefined_or_null() {
        return Ok(default.to_string());
    }
    let val = options.get_property_str(ctx, key)?;
    if val.is_undefined() {
        return Ok(default.to_string());
    }
    // Only string primitives accepted; objects throw TypeError
    if val.as_object().is_some() {
        return Err(
            ctx.throw_type_error(&format!("{key} must be a string primitive, not an object"))
        );
    }
    let s = val.to_string(ctx)?;
    Ok(s.content().encode_utf8(false).into_owned())
}

/// Get a bool option.
fn get_option_bool(
    ctx: &mut ScopedContext,
    options: JSValue,
    key: &str,
    default: bool,
) -> Result<bool, i32> {
    if options.is_undefined_or_null() {
        return Ok(default);
    }
    let val = options.get_property_str(ctx, key)?;
    if val.is_undefined() {
        return Ok(default);
    }
    val.to_bool()
}

// ---------------------------------------------------------------------------
// Base64 decode
// ---------------------------------------------------------------------------

fn base64_decode(
    input: &[u8],
    alphabet: u8,
    last_chunk_handling: u8,
    max_length: usize,
) -> Result<(Vec<u8>, usize), (Vec<u8>, usize, String)> {
    // Spec: maxLength = 0 returns { Read: 0, Bytes: [], Error: none } and does
    // NOT validate the input at all.
    if max_length == 0 {
        return Ok((Vec::new(), 0));
    }

    let b64_value = |b: u8| -> Option<u8> {
        let table: &[u8; 64] = if alphabet == 1 {
            URL_ALPHABET
        } else {
            STD_ALPHABET
        };
        table.iter().position(|&c| c == b).map(|p| p as u8)
    };

    // Strip ASCII whitespace only. Keep every other character (including
    // invalid ones) so that:
    //  - the decode loop can tell a real syntax error from trailing garbage
    //    that must be ignored once the buffer is full;
    //  - `read` counts consumed characters (the spec [[Read]] field), and
    //  - positions stay accurate when whitespace is present.
    // We record the original byte index so `read` reflects the original string
    // position (matching the spec, which counts whitespace as consumed).
    let chars: Vec<(usize, u8)> = input
        .iter()
        .enumerate()
        .filter(|(_, c)| !is_ascii_whitespace(**c))
        .map(|(i, c)| (i, *c))
        .collect();

    let total = chars.len();
    let mut output = Vec::with_capacity((total * 3) / 4 + 3);
    let mut pos = 0usize;
    let mut read = 0usize;

    while pos < total {
        if pos + 4 <= total {
            // When the output buffer is already full, the current (and any
            // following) chunk is trailing garbage and is ignored without
            // raising a syntax error, even if it contains invalid characters.
            if output.len() >= max_length {
                return Ok((output, chars[pos].0));
            }
            let (_i0, c0) = chars[pos];
            let (_i1, c1) = chars[pos + 1];
            let (_i2, c2) = chars[pos + 2];
            let (i3, c3) = chars[pos + 3];

            // Padding must not appear in the first two positions.
            if c0 == b'=' || c1 == b'=' {
                return Err((
                    output.clone(),
                    chars[pos].0,
                    "invalid character".to_string(),
                ));
            }
            if b64_value(c0).is_none() || b64_value(c1).is_none() {
                return Err((
                    output.clone(),
                    chars[pos].0,
                    "invalid character".to_string(),
                ));
            }
            let v0 = b64_value(c0).unwrap();
            let v1 = b64_value(c1).unwrap();

            let bytes: Vec<u8> = if c2 == b'=' && c3 == b'=' {
                if last_chunk_handling == 1 && (v1 & 0x0f) != 0 {
                    return Err((
                        output.clone(),
                        chars[pos].0,
                        "non-zero padding bits".to_string(),
                    ));
                }
                vec![(v0 << 2) | (v1 >> 4)]
            } else if c2 == b'=' {
                // A single '=' at c2 with a real c3 is malformed: full padding
                // needs both c2 and c3 to be '='.
                return Err((
                    output.clone(),
                    chars[pos].0,
                    "invalid character".to_string(),
                ));
            } else if c3 == b'=' {
                if b64_value(c2).is_none() {
                    return Err((
                        output.clone(),
                        chars[pos].0,
                        "invalid character".to_string(),
                    ));
                }
                let v2 = b64_value(c2).unwrap();
                if last_chunk_handling == 1 && (v2 & 0x03) != 0 {
                    return Err((
                        output.clone(),
                        chars[pos].0,
                        "non-zero padding bits".to_string(),
                    ));
                }
                vec![(v0 << 2) | (v1 >> 4), (v1 << 4) | (v2 >> 2)]
            } else {
                if b64_value(c2).is_none() || b64_value(c3).is_none() {
                    return Err((
                        output.clone(),
                        chars[pos].0,
                        "invalid character".to_string(),
                    ));
                }
                let v2 = b64_value(c2).unwrap();
                let v3 = b64_value(c3).unwrap();
                vec![(v0 << 2) | (v1 >> 4), (v1 << 4) | (v2 >> 2), (v2 << 6) | v3]
            };

            // A chunk that contains padding must be the final chunk. If there
            // are still characters after it, the tail is invalid: the padded
            // chunk is NOT written and a syntax error is raised (the bytes
            // produced by earlier chunks are still reported).
            if (c2 == b'=' || c3 == b'=') && pos + 4 < total {
                return Err((
                    output.clone(),
                    chars[pos].0,
                    "invalid character".to_string(),
                ));
            }

            // Respect max_length: a complete chunk is written only if it fits
            // entirely. If it does not fit, stop (trailing garbage after a full
            // buffer is ignored) and report the position before this chunk.
            if output.len() + bytes.len() > max_length {
                return Ok((output, chars[pos].0));
            }
            output.extend_from_slice(&bytes);
            read = i3 + 1;
            pos += 4;
        } else {
            let remaining = total - pos;
            match last_chunk_handling {
                0 => {
                    // loose
                    // Once the target is full, stop silently (trailing garbage
                    // in an incomplete chunk is ignored).
                    if output.len() == max_length {
                        return Ok((output, chars[pos].0));
                    }
                    if remaining == 1 {
                        // A single trailing character cannot form a byte.
                        return Err((output.clone(), chars[pos].0, "invalid length".to_string()));
                    }
                    // A padding symbol '=' must never appear inside an
                    // incomplete (partial) chunk.
                    if chars[pos..total].iter().any(|&(_, c)| c == b'=') {
                        return Err((
                            output.clone(),
                            chars[pos].0,
                            "invalid character".to_string(),
                        ));
                    }
                    // Any non-alphabet character is a syntax error.
                    if chars[pos..total]
                        .iter()
                        .any(|&(_, c)| b64_value(c).is_none())
                    {
                        return Err((
                            output.clone(),
                            chars[pos].0,
                            "invalid character".to_string(),
                        ));
                    }
                    let v0 = b64_value(chars[pos].1).unwrap();
                    let v1 = b64_value(chars[pos + 1].1).unwrap();
                    if remaining == 3 {
                        let v2 = b64_value(chars[pos + 2].1).unwrap();
                        if output.len() + 2 > max_length {
                            return Ok((output, chars[pos].0));
                        }
                        output.push((v0 << 2) | (v1 >> 4));
                        output.push((v1 << 4) | (v2 >> 2));
                        read = chars[total - 1].0 + 1;
                    } else {
                        // remaining == 2
                        if output.len() + 1 > max_length {
                            return Ok((output, chars[pos].0));
                        }
                        output.push((v0 << 2) | (v1 >> 4));
                        read = chars[total - 1].0 + 1;
                    }
                    pos = total;
                }
                1 => {
                    // strict
                    if output.len() == max_length {
                        return Ok((output, chars[pos].0));
                    }
                    // A strict final chunk must be a complete 4-char group.
                    return Err((output.clone(), chars[pos].0, "invalid length".to_string()));
                }
                2 => {
                    // stop-before-partial
                    if output.len() == max_length {
                        return Ok((output, chars[pos].0));
                    }
                    // Stop before the partial (incomplete) final chunk. The
                    // partial chunk is skipped (no error, nothing written) only
                    // when it consists entirely of valid data characters, or is a
                    // padded `XX=` chunk. Any other content (an illegal
                    // character, or padding in a bad position) is a syntax error.
                    let remaining = total - pos;
                    let is_partial_padding = remaining == 3
                        && chars[pos].1 != b'='
                        && chars[pos + 1].1 != b'='
                        && chars[pos + 2].1 == b'=';
                    let all_data = !chars[pos..total]
                        .iter()
                        .any(|&(_, c)| c == b'=' || b64_value(c).is_none());
                    if !(is_partial_padding || all_data) {
                        return Err((
                            output.clone(),
                            chars[pos].0,
                            "invalid character".to_string(),
                        ));
                    }
                    read = chars[pos].0;
                    pos = total;
                }
                _ => unreachable!(),
            }
        }
    }

    Ok((output, read))
}

// ---------------------------------------------------------------------------
// Hex decode
// ---------------------------------------------------------------------------

fn hex_decode(
    input: &[u8],
    max_length: usize,
) -> Result<(Vec<u8>, usize), (Vec<u8>, usize, String)> {
    // Hex does NOT skip any whitespace characters: every non-nibble byte
    // (including spaces, tabs, newlines) is an illegal character that becomes
    // a syntax error in the decode loop, while bytes already written before
    // the error are preserved. We keep every input byte (including whitespace)
    // so positions and the trailing-garbage handling stay accurate.
    let chars: Vec<(usize, u8)> = input.iter().enumerate().map(|(i, c)| (i, *c)).collect();

    let total = chars.len();
    let mut output = Vec::with_capacity(total / 2 + 1);
    let mut pos = 0usize;
    let mut read = 0usize;

    while pos + 2 <= total {
        // When the output buffer is already full, the remaining input is
        // trailing garbage and is ignored (no syntax error).
        if output.len() >= max_length {
            return Ok((output, chars[pos].0));
        }
        let (_, c0) = chars[pos];
        let (i1, c1) = chars[pos + 1];
        let hi = hex_nibble(c0);
        let lo = hex_nibble(c1);
        if hi.is_none() || lo.is_none() {
            // Invalid character: report it but keep already-written bytes.
            let bad = if hi.is_none() { c0 } else { c1 };
            let ch = bad as char;
            if ch.is_ascii() {
                return Err((
                    output.clone(),
                    chars[pos].0,
                    format!("invalid character: '{}'", ch),
                ));
            }
            return Err((
                output.clone(),
                chars[pos].0,
                format!("invalid character: '\\u{{{:04x}}}'", ch as u32),
            ));
        }
        if output.len() + 1 > max_length {
            return Ok((output, read));
        }
        output.push((hi.unwrap() << 4) | lo.unwrap());
        read = i1 + 1;
        pos += 2;
    }

    // Leftover single character at the end is an odd-length error. When the
    // whole input is a valid-length-odd string of legal nibbles (no illegal
    // character was encountered while decoding), nothing has been written yet
    // and nothing is written (the error is raised before any bytes are
    // produced). Illegal characters are caught in the loop above and keep the
    // already-decoded bytes.
    if pos < total {
        let (_, c) = chars[pos];
        let ch = c as char;
        if ch.is_ascii() {
            return Err((
                Vec::new(),
                chars[pos].0,
                format!("invalid character: '{}'", ch),
            ));
        }
        return Err((
            Vec::new(),
            chars[pos].0,
            format!("invalid character: '\\u{{{:04x}}}'", ch as u32),
        ));
    }

    Ok((output, read))
}

// ---------------------------------------------------------------------------
// Base64 encode
// ---------------------------------------------------------------------------

fn base64_encode(data: &[u8], alphabet: u8, omit_padding: bool) -> String {
    let table = if alphabet == 1 {
        URL_ALPHABET
    } else {
        STD_ALPHABET
    };
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        match chunk.len() {
            3 => {
                let n = (b0 << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
                out.push(table[((n >> 18) & 0x3F) as usize] as char);
                out.push(table[((n >> 12) & 0x3F) as usize] as char);
                out.push(table[((n >> 6) & 0x3F) as usize] as char);
                out.push(table[(n & 0x3F) as usize] as char);
            }
            2 => {
                let n = (b0 << 16) | ((chunk[1] as u32) << 8);
                out.push(table[((n >> 18) & 0x3F) as usize] as char);
                out.push(table[((n >> 12) & 0x3F) as usize] as char);
                out.push(table[((n >> 6) & 0x3F) as usize] as char);
                if !omit_padding {
                    out.push('=');
                }
            }
            1 => {
                let n = b0 << 16;
                out.push(table[((n >> 18) & 0x3F) as usize] as char);
                out.push(table[((n >> 12) & 0x3F) as usize] as char);
                if !omit_padding {
                    out.push('=');
                    out.push('=');
                }
            }
            _ => unreachable!(),
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Hex encode
// ---------------------------------------------------------------------------

fn hex_encode(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(data.len() * 2);
    for &b in data {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Result object { read, written }
// ---------------------------------------------------------------------------

fn make_result_object(ctx: &mut ScopedContext, read: usize, written: usize) -> JSResult {
    let mut obj = ctx.create_base_object()?;
    obj.define_property_value_str(ctx, "read", JSValue::from(read as u32), JS_PROP_C_W_E)?;
    obj.define_property_value_str(ctx, "written", JSValue::from(written as u32), JS_PROP_C_W_E)?;
    Ok(obj.into())
}

// ---------------------------------------------------------------------------
// TypedArray helpers
// ---------------------------------------------------------------------------

fn require_uint8_array(ctx: &mut ScopedContext, this: JSValue) -> Result<TypedArray, i32> {
    this.as_object_TypedArray()
        .filter(|ta| ta.class_id() == crate::JS_CLASS_UINT8_ARRAY)
        .ok_or_else(|| {
            ctx.throw_type_error("Method typed array method called on incompatible type")
        })
}

fn create_uint8_array(ctx: &mut ScopedContext, ctor: JSValue, len: usize) -> Result<JSValue, i32> {
    TypedArray::create_empty(ctx, ctor, len, crate::JS_CLASS_UINT8_ARRAY).map(|ta| ta.into())
}

/// Get bytes from a StringRef as a Vec<u8>.
fn string_ref_bytes(s: crate::string::StringRef) -> Vec<u8> {
    s.content().encode_utf8_bytes(true).into_owned()
}

// ===========================================================================
// Static methods
// ===========================================================================

/// Uint8Array.fromBase64(string[, options])
pub fn js_uint8array_fromBase64(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let string_val = cc.arg(0);
    let options = cc.arg(1);
    let ctor = ctx.global_obj().get_property_str(ctx, "Uint8Array")?;

    if !string_val.is_string() {
        return Err(ctx.throw_type_error("Uint8Array.fromBase64: first argument must be a string"));
    }
    let string = string_val.to_string(ctx)?;
    let input = string_ref_bytes(string);

    let alphabet_str = get_option_str(ctx, options, "alphabet", "base64")?;
    let alphabet = if alphabet_str == "base64" {
        0u8
    } else if alphabet_str == "base64url" {
        1
    } else {
        return Err(ctx.throw_type_error("invalid alphabet"));
    };

    let lch_str = get_option_str(ctx, options, "lastChunkHandling", "loose")?;
    let last_chunk_handling = if lch_str == "loose" {
        0u8
    } else if lch_str == "strict" {
        1
    } else if lch_str == "stop-before-partial" {
        2
    } else {
        return Err(ctx.throw_type_error("invalid lastChunkHandling"));
    };

    let (bytes, _) = base64_decode(&input, alphabet, last_chunk_handling, usize::MAX)
        .map_err(|e| ctx.throw_syntax_error(&e.2))?;

    let arr = create_uint8_array(ctx, ctor, bytes.len())?;
    if !bytes.is_empty() {
        let mut ta = arr.as_object_TypedArray().unwrap();
        ta.with_bytes_mut(|buf| buf[..bytes.len()].copy_from_slice(&bytes));
    }
    Ok(arr)
}

/// Uint8Array.fromHex(string)
pub fn js_uint8array_fromHex(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let string_val = cc.arg(0);
    let ctor = ctx.global_obj().get_property_str(ctx, "Uint8Array")?;

    if !string_val.is_string() {
        return Err(ctx.throw_type_error("Uint8Array.fromHex: first argument must be a string"));
    }
    let string = string_val.to_string(ctx)?;
    let input = string_ref_bytes(string);

    let (bytes, _) = hex_decode(&input, usize::MAX).map_err(|e| ctx.throw_syntax_error(&e.2))?;

    let arr = create_uint8_array(ctx, ctor, bytes.len())?;
    if !bytes.is_empty() {
        let mut ta = arr.as_object_TypedArray().unwrap();
        ta.with_bytes_mut(|buf| buf[..bytes.len()].copy_from_slice(&bytes));
    }
    Ok(arr)
}

// ===========================================================================
// Prototype methods
// ===========================================================================

/// Uint8Array.prototype.toBase64([options])
pub fn js_uint8array_proto_toBase64(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let ta = require_uint8_array(ctx, cc.this())?;

    let options = cc.arg(0);
    let alphabet_str = get_option_str(ctx, options, "alphabet", "base64")?;
    let alphabet = if alphabet_str == "base64" {
        0u8
    } else if alphabet_str == "base64url" {
        1
    } else {
        return Err(ctx.throw_type_error("invalid alphabet"));
    };
    let omit_padding = get_option_bool(ctx, options, "omitPadding", false)?;

    if ta.is_detached() {
        return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
    }

    let result = ta.with_bytes(|buf| base64_encode(buf, alphabet, omit_padding));
    ctx.create_string_from_utf8(result.into_bytes())
        .map(JSValue::make_string)
}

/// Uint8Array.prototype.toHex()
pub fn js_uint8array_proto_toHex(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let ta = require_uint8_array(ctx, cc.this())?;

    if ta.is_detached() {
        return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
    }

    let result = ta.with_bytes(hex_encode);
    ctx.create_string_from_utf8(result.into_bytes())
        .map(JSValue::make_string)
}

/// Uint8Array.prototype.setFromBase64(string[, options])
pub fn js_uint8array_proto_setFromBase64(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut ta = require_uint8_array(ctx, cc.this())?;
    let string_val = cc.arg(0);
    let options = cc.arg(1);

    if !string_val.is_string() {
        return Err(ctx.throw_type_error(
            "Uint8Array.prototype.setFromBase64: first argument must be a string",
        ));
    }

    if ta.is_detached() {
        return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
    }

    let max_length = ta.length();
    let string = string_val.to_string(ctx)?;
    let input = string_ref_bytes(string);

    let alphabet_str = get_option_str(ctx, options, "alphabet", "base64")?;
    let alphabet = if alphabet_str == "base64" {
        0u8
    } else if alphabet_str == "base64url" {
        1
    } else {
        return Err(ctx.throw_type_error("invalid alphabet"));
    };

    let lch_str = get_option_str(ctx, options, "lastChunkHandling", "loose")?;
    let last_chunk_handling = if lch_str == "loose" {
        0u8
    } else if lch_str == "strict" {
        1
    } else if lch_str == "stop-before-partial" {
        2
    } else {
        return Err(ctx.throw_type_error("invalid lastChunkHandling"));
    };

    let res = base64_decode(&input, alphabet, last_chunk_handling, max_length);
    // Per spec, bytes already decoded before a syntax error are still written
    // to the target; the error is reported afterwards (writes-up-to-error).
    let (bytes, consumed) = match &res {
        Ok((bytes, consumed)) => (bytes.clone(), *consumed),
        Err((bytes, consumed, _)) => (bytes.clone(), *consumed),
    };
    let written = bytes.len();
    if written > 0 {
        // If the underlying buffer was detached (e.g. via an option getter) the
        // writable slice is empty; writing into it would panic. Per spec this
        // must throw a TypeError rather than crash.
        let detached = ta.with_bytes_mut(|buf| buf.len() < written);
        if detached {
            return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
        }
        ta.with_bytes_mut(|buf| buf[..written].copy_from_slice(&bytes));
    }
    if let Err((_, _, msg)) = res {
        return Err(ctx.throw_syntax_error(&msg));
    }

    make_result_object(ctx, consumed, written)
}

/// Uint8Array.prototype.setFromHex(string)
pub fn js_uint8array_proto_setFromHex(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut ta = require_uint8_array(ctx, cc.this())?;
    let string_val = cc.arg(0);

    if !string_val.is_string() {
        return Err(ctx
            .throw_type_error("Uint8Array.prototype.setFromHex: first argument must be a string"));
    }

    if ta.is_detached() {
        return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
    }

    let max_length = ta.length();
    let string = string_val.to_string(ctx)?;
    let input = string_ref_bytes(string);

    let res = hex_decode(&input, max_length);
    // Per spec, bytes already decoded before a syntax error are still written
    // to the target; the error is reported afterwards (writes-up-to-error).
    let (bytes, consumed) = match &res {
        Ok((bytes, consumed)) => (bytes.clone(), *consumed),
        Err((bytes, consumed, _)) => (bytes.clone(), *consumed),
    };
    let written = bytes.len();
    if written > 0 {
        // If the underlying buffer was detached (e.g. via an option getter) the
        // writable slice is empty; writing into it would panic. Per spec this
        // must throw a TypeError rather than crash.
        let detached = ta.with_bytes_mut(|buf| buf.len() < written);
        if detached {
            return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
        }
        ta.with_bytes_mut(|buf| buf[..written].copy_from_slice(&bytes));
    }
    if let Err((_, _, msg)) = res {
        return Err(ctx.throw_syntax_error(&msg));
    }

    make_result_object(ctx, consumed, written)
}
