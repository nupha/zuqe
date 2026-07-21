// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! `JSValueTag` enum and low-level encode/decode helpers for NaN-boxing.
//!
//! Tag value mapping (based on QuickJS, adapted for zhuque's type system):
//!
//! | Raw i32 | Variant            | Payload         |
//! |---------|--------------------|-----------------|
//! | -9      | `String`           | low 32-bit ptr  |
//! | -8      | `Symbol`           | low 32-bit ptr  |
//! | -7      | `StringRope`       | low 32-bit ptr  |
//! | -3      | `Module`           | low 32-bit ptr  |
//! | -2      | `FunctionBytecode` | low 32-bit ptr  |
//! | -1      | `Object`           | low 32-bit ptr  |
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

/// NaN-boxing tag enum.
///
/// Each variant's discriminant matches QuickJS tag constants directly.
/// Use `as i32` to get the raw value, `from_raw()` to recover from raw i32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JSValueTag {
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
    pub fn from_raw(tag: i32) -> Option<JSValueTag> {
        // Match on known tags; float64/invalid values fall through to None.
        match tag {
            -9 => Some(JSValueTag::String),
            -8 => Some(JSValueTag::Symbol),
            -7 => Some(JSValueTag::StringRope),
            -3 => Some(JSValueTag::Module),
            -2 => Some(JSValueTag::FunctionBytecode),
            -1 => Some(JSValueTag::Object),
            0  => Some(JSValueTag::Int),
            1  => Some(JSValueTag::Bool),
            2  => Some(JSValueTag::Null),
            3  => Some(JSValueTag::Undefined),
            4  => Some(JSValueTag::Uninitialized),
            5  => Some(JSValueTag::CatchOffset),
            6  => Some(JSValueTag::Exception),
            _ => None,
        }
    }

    /// Check whether a raw i32 tag encodes a float64 (including addend-shifted tags).
    #[inline(always)]
    pub fn is_float64_raw(tag: i32) -> bool {
        // Known non-float64 tags ∈ [-9, 6]; float64-encoded tags fall into
        // (-∞, -10] ∪ [8, +∞).  Use unsigned range check (no lookup table).
        (tag.wrapping_sub(JSValueTag::String as i32) as u32)
            >= ((JSValueTag::Float64 as i32 - JSValueTag::String as i32) as u32)
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

    /// Return the underlying i32 value (equivalent to `self as i32`).
    #[inline(always)]
    pub const fn to_i32(self) -> i32 {
        self as i32
    }

    // -- Encoding helpers --

    /// Encode as NaN-boxing u64: `(tag << 32) | val`.
    #[inline(always)]
    pub(crate) const fn mkval(self, val: u32) -> u64 {
        ((self as u64) << 32) | (val as u64)
    }

    /// Encode a pointer into NaN-boxing u64 (requires pointer in low 4 GiB).
    #[inline(always)]
    pub(crate) fn mkptr(self, ptr: *const ()) -> u64 {
        ((self as u64) << 32) | (ptr as u64 & 0xFFFF_FFFF)
    }
}

// ============================================================================
// Derived constants (computed from `JSValueTag` values for internal use)
// ============================================================================

/// The first (most negative) tag — start of reference-counted types.
pub(crate) const JS_TAG_FIRST: i32 = JSValueTag::String as i32;

/// Float64 tag addend: shifts `f64` exponent/mantissa bits so that
/// `bits[63:32]` falls into `[8, +∞)`, disjoint from all non-float64 tags `[-9, 7]`.
pub(crate) const JS_FLOAT64_TAG_ADDEND: i32 = 0x7ff80000 - JS_TAG_FIRST + 1;

/// NaN-boxing encoded quiet NaN.
pub(crate) const JS_NAN_U64: u64 =
    0x7ff8000000000000u64.wrapping_sub((JS_FLOAT64_TAG_ADDEND as u64) << 32);

// ============================================================================
// crate-internal low-level helpers (not public API; use `JSValue::from` etc.)
// ============================================================================

/// Extract the raw i32 tag from a NaN-boxing u64.
#[inline(always)]
pub(crate) fn js_value_get_tag(v: u64) -> i32 {
    (v >> 32) as i32
}

/// Decode a `f64` from a NaN-boxing u64.
#[inline(always)]
pub(crate) fn js_value_get_float64(v: u64) -> f64 {
    f64::from_bits(v.wrapping_add((JS_FLOAT64_TAG_ADDEND as u64) << 32))
}

/// Encode a `f64` into a NaN-boxing u64 (normalizes NaN payloads).
#[inline(always)]
pub(crate) const fn js_new_float64(d: f64) -> u64 {
    let bits = d.to_bits();
    // Canonicalize NaN: replace any NaN payload with the canonical quiet NaN.
    if (bits & 0x7fffffffffffffff) > 0x7ff0000000000000 {
        JS_NAN_U64
    } else {
        bits.wrapping_sub((JS_FLOAT64_TAG_ADDEND as u64) << 32)
    }
}
