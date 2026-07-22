// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! `JSValueTag` enum and low-level encode/decode helpers for NaN-boxing.
//!
//! Tag value mapping (based on QuickJS, adapted for zhuque's type system):
//!
//! | Raw i32 | Variant            | Payload         |
//! |---------|--------------------|-----------------|
//! | -9      | `String`           | low 48-bit ptr  |
//! | -8      | `Symbol`           | low 48-bit ptr  |
//! | -7      | `StringRope`       | low 48-bit ptr  |
//! | -3      | `Module`           | low 48-bit ptr  |
//! | -2      | `FunctionBytecode` | low 48-bit ptr  |
//! | -1      | `Object`           | low 48-bit ptr  |
//! | 0       | `Int`              | i32 payload     |
//! | 1       | `Bool`             | 0/1             |
//! | 2       | `Null`             | 0               |
//! | 3       | `Undefined`        | 0               |
//! | 4       | `Uninitialized`    | 0               |
//! | 5       | `CatchOffset`      | i32 payload     |
//! | 6       | `Exception`        | 0               |
//! | 8       | `Float64`          | NaN-encoded f64 |
//!
//! Note: Encoded float64 tags are scattered (offset by `JS_FLOAT64_TAG_ADDEND`).
//! `Float64` is only used as the normalized form, never as a raw encoding tag.

use std::ptr::NonNull;

use gc_lite::GcHead;

/// NaN-boxing tag enum.
///
/// Each variant's discriminant matches QuickJS tag constants directly.
/// Use `as i32` to get the raw value, `from_raw()` to recover from raw i32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub(crate) enum JSValueTag {
    String = -9,
    Symbol = -8,
    StringRope = -7,
    Module = -3,
    FunctionBytecode = -2,
    Object = -1,
    Int = 0,
    Bool = 1,
    Null = 2,
    Undefined = 3,
    Uninitialized = 4,
    CatchOffset = 5,
    Exception = 6,
    /// Float64 — only used as normalized tag, never as raw encoding tag.
    Float64 = 8,
}

impl JSValueTag {
    /// Recover a `JSValueTag` from a raw i32.
    ///
    /// Returns `None` for float64-encoded or invalid values.
    #[inline(always)]
    pub const fn from_raw(tag: i32) -> Option<JSValueTag> {
        // Match on known tags; float64/invalid values fall through to None.
        match tag {
            -9 => Some(JSValueTag::String),
            -8 => Some(JSValueTag::Symbol),
            -7 => Some(JSValueTag::StringRope),
            -3 => Some(JSValueTag::Module),
            -2 => Some(JSValueTag::FunctionBytecode),
            -1 => Some(JSValueTag::Object),
            0 => Some(JSValueTag::Int),
            1 => Some(JSValueTag::Bool),
            2 => Some(JSValueTag::Null),
            3 => Some(JSValueTag::Undefined),
            4 => Some(JSValueTag::Uninitialized),
            5 => Some(JSValueTag::CatchOffset),
            6 => Some(JSValueTag::Exception),
            _ => None,
        }
    }

    /// Check whether a raw i32 tag encodes a float64 (including addend-shifted tags).
    #[inline(always)]
    pub const fn is_float64_raw(tag: i32) -> bool {
        // Non-float tags are exactly String(-9) .. Exception(6); float64-encoded
        // tags are everything outside that range. The f64 addend offset pushes
        // real doubles' high-16-bit tag into `[7, +∞) ∪ (-∞, -10]`, disjoint
        // from `[-9, 6]`.
        tag < JSValueTag::String as i32 || tag > JSValueTag::Exception as i32
    }

    /// Normalize a raw tag: return `Float64` for float64-encoded tags,
    /// otherwise look up the known tag.
    #[inline(always)]
    pub fn norm_tag(tag: i32) -> JSValueTag {
        if Self::is_float64_raw(tag) {
            JSValueTag::Float64
        } else {
            // Safe: non-float64 raw tags always match a known variant.
            Self::from_raw(tag).unwrap_or(JSValueTag::Undefined)
        }
    }

    /// Encode as NaN-boxing u64: `(tag << 48) | (val & 0xFFFFFFFFFFFF)`.
    /// Tag occupies the high 16 bits (bits 48..64); payload the low 48 bits.
    #[inline(always)]
    pub(crate) const fn mkval(self, val: u32) -> u64 {
        ((self as u64) << 48) | (val as u64 & 0x0000_FFFF_FFFF_FFFF)
    }

    /// Encode a gc node pointer into NaN-boxing u64.
    ///
    /// On 64-bit the GC-managed payload pointer is stored in the low 48 bits
    /// (canonical x86-64 user addresses are < 2^48); the tag occupies the high
    /// 16 bits. The full 64-bit pointer is preserved (no 4 GiB truncation).
    #[inline(always)]
    pub(crate) fn mknode(self, node: NonNull<GcHead>) -> u64 {
        ((self as u64) << 48) | (node.as_ptr() as u64 & 0x0000_FFFF_FFFF_FFFF)
    }
}

// ============================================================================
// Derived constants (computed from `JSValueTag` values for internal use)
// ============================================================================

/// The first (most negative) tag — start of reference-counted types.
pub(crate) const JS_TAG_FIRST: i32 = JSValueTag::String as i32;

/// Float64 tag addend (high 16 bits): `JS_FLOAT64_TAG_ADDEND << 48` is added to
/// a `f64`'s bit pattern on decode and subtracted on encode. Chosen so that every
/// real (non-NaN) double's high-16-bit tag lands in `[7, +∞) ∪ (-∞, -10]`,
/// disjoint from all non-float tags `[-9, 6]`. The 16-wide collision window
/// `[ADDEND-9, ADDEND+6]` maps exactly onto the sign-1 NaN-exponent region
/// `[0xFFF0, 0xFFFF]` of the double's high bits, which is always canonicalized.
pub(crate) const JS_FLOAT64_TAG_ADDEND: i32 = 0xFFF9;

/// NaN-boxing encoded quiet NaN.
pub(crate) const JS_NAN_U64: u64 =
    0x7ff8000000000000u64.wrapping_sub((JS_FLOAT64_TAG_ADDEND as u64) << 48);

// ============================================================================
// crate-internal low-level helpers (not public API; use `JSValue::from` etc.)
// ============================================================================

/// Extract the raw i32 tag from a NaN-boxing u64.
///
/// Tag occupies the high 16 bits (bits 48..64), interpreted as a signed i16.
#[inline(always)]
pub(crate) const fn js_value_get_tag(v: u64) -> i32 {
    ((v >> 48) as i16) as i32
}

/// Decode a `f64` from a NaN-boxing u64.
#[inline(always)]
pub(crate) const fn js_value_get_float64(v: u64) -> f64 {
    f64::from_bits(v.wrapping_add((JS_FLOAT64_TAG_ADDEND as u64) << 48))
}

/// Encode a `f64` into a NaN-boxing u64 (normalizes NaN payloads).
#[inline(always)]
pub(crate) const fn js_new_float64(d: f64) -> u64 {
    let bits = d.to_bits();
    // Canonicalize NaN: replace any NaN payload with the canonical quiet NaN.
    if (bits & 0x7fffffffffffffff) > 0x7ff0000000000000 {
        JS_NAN_U64
    } else {
        bits.wrapping_sub((JS_FLOAT64_TAG_ADDEND as u64) << 48)
    }
}
