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
    Atom, context::ScopedContext, error::JSResult, function::CallContext, object::TypedArray,
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
fn b64_decode(ch: u8) -> Option<u8> {
    match ch {
        b'A'..=b'Z' => Some(ch - b'A'),
        b'a'..=b'z' => Some(ch - b'a' + 26),
        b'0'..=b'9' => Some(ch - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

#[inline]
fn is_std_b64(ch: u8) -> bool {
    matches!(ch, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/')
}

#[inline]
fn is_url_b64(ch: u8) -> bool {
    matches!(ch, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_')
}

#[inline]
fn is_ascii_whitespace(ch: u8) -> bool {
    matches!(ch, b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x0B)
}

#[inline]
fn hex_nibble(ch: u8) -> Option<u8> {
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
        return Err(ctx.throw_type_error(&format!(
            "{key} must be a string primitive, not an object"
        )));
    }
    let s = val.to_string(ctx)?;
    Ok(s.content().encode_utf8(false).into_owned())
}

/// Get a bool option.
fn get_option_bool(ctx: &mut ScopedContext, options: JSValue, key: &str, default: bool) -> Result<bool, i32> {
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
) -> Result<(Vec<u8>, usize), &'static str> {
    let is_valid = if alphabet == 1 { is_url_b64 } else { is_std_b64 };

    let mut chars: Vec<u8> = Vec::with_capacity(input.len());
    for &ch in input {
        if !is_ascii_whitespace(ch) {
            chars.push(ch);
        }
    }

    let total = chars.len();

    for &ch in &chars {
        if !is_valid(ch) && ch != b'=' {
            return Err("invalid character");
        }
    }

    let mut output = Vec::with_capacity((total * 3) / 4);
    let mut i = 0;

    while i + 4 <= total {
        let c0 = b64_decode(chars[i]).ok_or("invalid character")?;
        let c1 = b64_decode(chars[i + 1]).ok_or("invalid character")?;
        let c2 = chars[i + 2];
        let c3 = chars[i + 3];

        if c2 == b'=' && c3 == b'=' {
            output.push((c0 << 2) | (c1 >> 4));
            i += 4;
        } else if c3 == b'=' {
            let c2d = b64_decode(c2).ok_or("invalid character")?;
            output.push((c0 << 2) | (c1 >> 4));
            output.push((c1 << 4) | (c2d >> 2));
            i += 4;
        } else {
            let c2d = b64_decode(c2).ok_or("invalid character")?;
            let c3d = b64_decode(c3).ok_or("invalid character")?;
            output.push((c0 << 2) | (c1 >> 4));
            output.push((c1 << 4) | (c2d >> 2));
            output.push((c2d << 6) | c3d);
            i += 4;
        }
    }

    let remaining = total - i;
    if remaining > 0 {
        match last_chunk_handling {
            0 => {
                if remaining == 2 {
                    let c0 = b64_decode(chars[i]).ok_or("invalid character")?;
                    let c1 = b64_decode(chars[i + 1]).ok_or("invalid character")?;
                    output.push((c0 << 2) | (c1 >> 4));
                } else if remaining == 3 {
                    let c0 = b64_decode(chars[i]).ok_or("invalid character")?;
                    let c1 = b64_decode(chars[i + 1]).ok_or("invalid character")?;
                    let c2 = b64_decode(chars[i + 2]).ok_or("invalid character")?;
                    output.push((c0 << 2) | (c1 >> 4));
                    output.push((c1 << 4) | (c2 >> 2));
                }
            }
            1 => return Err("invalid character"),
            2 => {}
            _ => unreachable!(),
        }
    }

    Ok((output, total))
}

// ---------------------------------------------------------------------------
// Hex decode
// ---------------------------------------------------------------------------

fn hex_decode(input: &[u8]) -> Result<(Vec<u8>, usize), &'static str> {
    let mut chars: Vec<u8> = Vec::with_capacity(input.len());
    for &ch in input {
        if !is_ascii_whitespace(ch) {
            chars.push(ch);
        }
    }

    if chars.len() % 2 != 0 {
        return Err("odd-length string");
    }

    let mut output = Vec::with_capacity(chars.len() / 2);
    let mut i = 0;
    while i < chars.len() {
        let hi = hex_nibble(chars[i]).ok_or("invalid character")?;
        let lo = hex_nibble(chars[i + 1]).ok_or("invalid character")?;
        output.push((hi << 4) | lo);
        i += 2;
    }

    Ok((output, chars.len()))
}

// ---------------------------------------------------------------------------
// Base64 encode
// ---------------------------------------------------------------------------

fn base64_encode(data: &[u8], alphabet: u8, omit_padding: bool) -> String {
    let table = if alphabet == 1 { URL_ALPHABET } else { STD_ALPHABET };
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);

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
        .ok_or_else(|| ctx.throw_type_error("Method typed array method called on incompatible type"))
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
    let ctor = cc.this();

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

    let (bytes, _) = base64_decode(&input, alphabet, last_chunk_handling)
        .map_err(|e| ctx.throw_syntax_error(e))?;

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
    let ctor = cc.this();

    let string = string_val.to_string(ctx)?;
    let input = string_ref_bytes(string);

    let (bytes, _) = hex_decode(&input).map_err(|e| ctx.throw_syntax_error(e))?;

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
    ctx.create_string_from_str(&result)
        .map(JSValue::make_string)
}

/// Uint8Array.prototype.toHex()
pub fn js_uint8array_proto_toHex(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let ta = require_uint8_array(ctx, cc.this())?;

    if ta.is_detached() {
        return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
    }

    let result = ta.with_bytes(hex_encode);
    ctx.create_string_from_str(&result)
        .map(JSValue::make_string)
}

/// Uint8Array.prototype.setFromBase64(string[, options])
pub fn js_uint8array_proto_setFromBase64(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut ta = require_uint8_array(ctx, cc.this())?;
    let string_val = cc.arg(0);
    let options = cc.arg(1);

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

    let (bytes, consumed) = base64_decode(&input, alphabet, last_chunk_handling)
        .map_err(|e| ctx.throw_syntax_error(e))?;

    if ta.is_detached() {
        return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
    }

    let written = bytes.len().min(ta.length());
    if written > 0 {
        ta.with_bytes_mut(|buf| buf[..written].copy_from_slice(&bytes[..written]));
    }

    make_result_object(ctx, consumed, written)
}

/// Uint8Array.prototype.setFromHex(string)
pub fn js_uint8array_proto_setFromHex(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut ta = require_uint8_array(ctx, cc.this())?;
    let string_val = cc.arg(0);

    let string = string_val.to_string(ctx)?;
    let input = string_ref_bytes(string);

    let (bytes, consumed) = hex_decode(&input).map_err(|e| ctx.throw_syntax_error(e))?;

    if ta.is_detached() {
        return Err(ctx.throw_type_error("attempt to access detached ArrayBuffer"));
    }

    let written = bytes.len().min(ta.length());
    if written > 0 {
        ta.with_bytes_mut(|buf| buf[..written].copy_from_slice(&bytes[..written]));
    }

    make_result_object(ctx, consumed, written)
}
