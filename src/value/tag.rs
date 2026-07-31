// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! `JSValueTag` enum and low-level encode/decode helpers for NaN-boxing.
//!
//! Tag value mapping (based on QuickJS, adapted for zhuque's type system):
//!
//! | Raw i32 | Variant            | Payload         |
//! |---------|--------------------|-----------------|
//! | -8      | `Symbol`           | low 48-bit ptr  |
//! | -7      | `StringRope`       | low 48-bit ptr  |
//! | -6      | `String`           | low 48-bit ptr  |
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
#[repr(i32)]
pub(crate) enum JSValueTag {
    String = -6,
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
            -8 => Some(JSValueTag::Symbol),
            -6 => Some(JSValueTag::String),
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
    pub const fn is_float64(tag: i32) -> bool {
        // Non-float tags cover exactly Symbol(-8) .. Exception(6); everything else
        // (including the shifted raw tag -9 for negative infinity) is float64. The
        // f64 addend shift pushes real doubles' high-16-bit tag into
        // `[7, +∞) ∪ (-∞, -9)`, disjoint from `[-8, 6]`. Original doubles whose
        // high 16 bits fall in `[-8, 6]` (raw high bits [0xFFF1, 0xFFFF]) are all in
        // the sign-1 NaN region and are canonicalized to a quiet NaN, so they never
        // collide with any tag; negative infinity (high bits 0xFFF0) maps to -9,
        // which is treated as a float.
        tag < JSValueTag::Symbol as i32 || tag > JSValueTag::Exception as i32
    }

    /// Normalize a raw tag: return `Float64` for float64-encoded tags,
    /// otherwise look up the known tag.
    #[inline(always)]
    pub fn norm_tag(raw: i32) -> JSValueTag {
        if Self::is_float64(raw) {
            JSValueTag::Float64
        } else {
            // Safe: non-float64 raw tags always match a known variant.
            Self::from_raw(raw).unwrap_or(JSValueTag::Undefined)
        }
    }

    /// Encode as NaN-boxing u64: `(tag << 48) | (val & 0xFFFFFFFFFFFF)`.
    /// Tag occupies the high 16 bits (bits 48..64); payload the low 48 bits.
    #[inline(always)]
    pub(super) const fn mkval(self, val: u32) -> u64 {
        // mkval is only for "small immediate" tags: Int / Bool / CatchOffset.
        // Pointer tags (Object/String/...) must go through `mknode`; constant tags
        // (Null/Undefined/Exception/Uninitialized) use plain shift encoding.
        debug_assert!(matches!(self, Self::Bool | Self::Int | Self::CatchOffset));
        ((self as u64) << 48) | (val as u64 & 0x0000_FFFF_FFFF_FFFF)
    }

    /// Encode a gc node pointer into NaN-boxing u64.
    ///
    /// On 64-bit the GC-managed payload pointer is stored in the low 48 bits
    /// (canonical x86-64 user addresses are < 2^48); the tag occupies the high
    /// 16 bits. The full 64-bit pointer is preserved (no 4 GiB truncation).
    #[inline(always)]
    pub(crate) fn mknode(self, node: NonNull<GcHead>) -> u64 {
        debug_assert!(matches!(
            self,
            Self::String
                | Self::Symbol
                | Self::StringRope
                | Self::Module
                | Self::FunctionBytecode
                | Self::Object
        ));

        ((self as u64) << 48) | (node.as_ptr() as u64 & 0x0000_FFFF_FFFF_FFFF)
    }
}

// /// The most-negative reference-counted (gc node) tag — start of pointer-type tags.
// const JS_TAG_FIRST: i32 = JSValueTag::Symbol as i32;

/// Float64 tag addend (high 16 bits): `JS_FLOAT64_TAG_ADDEND << 48` is added to
/// a `f64`'s bit pattern on decode and subtracted on encode. Chosen so that every
/// real (non-NaN) double's high-16-bit tag lands in `[7, +∞) ∪ (-∞, -9]`,
/// disjoint from all non-float tags `[-8, 6]`. The `[-8, 6]` collision window
/// corresponds to `orig_high ∈ [0xFFF1, 0xFFFF]`, which is entirely the
/// sign-1 NaN-exponent region (`exp == 0x7FF`) and is always canonicalized to
/// `JS_NAN_U64`; the `-inf` boundary (`orig_high == 0xFFF0`) lands exactly at
/// raw tag `-9`, which is a valid float.
const JS_FLOAT64_TAG_ADDEND: i32 = 0xFFF9;

/// NaN-boxing encoded quiet NaN.
pub(super) const JS_NAN_U64: u64 =
    0x7ff8000000000000u64.wrapping_sub((JS_FLOAT64_TAG_ADDEND as u64) << 48);

// ============================================================================
// crate-internal low-level helpers (not public API; use `JSValue::from` etc.)
// ============================================================================

/// Extract the raw i32 tag from a NaN-boxing u64.
///
/// Tag occupies the high 16 bits (bits 48..64), interpreted as a signed i16.
#[inline(always)]
pub(super) const fn js_value_get_tag(v: u64) -> i32 {
    ((v >> 48) as i16) as i32
}

/// Decode a `f64` from a NaN-boxing u64.
#[inline(always)]
pub(super) const fn js_value_get_float64(v: u64) -> f64 {
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

// ============================================================================
// Tests: validate the NaN-boxing tag/value mechanism.
// ============================================================================
#[cfg(test)]
mod tests {
    use std::marker::PhantomData;
    use std::ptr::NonNull;

    use gc_lite::GcHead;

    use super::*;
    use crate::value::JSValue;

    /// A fake GC node address within the low 48 bits (only for encoding checks,
    /// never actually dereferenced).
    fn fake_node(addr: u64) -> NonNull<GcHead> {
        NonNull::new(addr as *mut GcHead).unwrap()
    }

    // ---- Constant tags (Null/Undefined/Exception/Uninitialized, etc.) ----
    #[test]
    fn constant_tags_encode_and_recognize() {
        // tag() should return the normalized JSValueTag
        assert_eq!(JSValue::NULL.tag(), JSValueTag::Null);
        assert_eq!(JSValue::UNDEFINED.tag(), JSValueTag::Undefined);
        assert_eq!(JSValue::EXCEPTION.tag(), JSValueTag::Exception);
        assert_eq!(JSValue::UNINITIALIZED.tag(), JSValueTag::Uninitialized);

        // norm_tag should also recover from the raw tag
        assert_eq!(
            JSValueTag::norm_tag(js_value_get_tag(JSValue::NULL.0)),
            JSValueTag::Null
        );
        assert_eq!(
            JSValueTag::norm_tag(js_value_get_tag(JSValue::UNDEFINED.0)),
            JSValueTag::Undefined
        );

        // Distinct constant tags have distinct raw tags (high 16 bits)
        let raws = [
            (JSValue::NULL, JSValueTag::Null),
            (JSValue::UNDEFINED, JSValueTag::Undefined),
            (JSValue::EXCEPTION, JSValueTag::Exception),
            (JSValue::UNINITIALIZED, JSValueTag::Uninitialized),
        ];
        for (v, t) in raws {
            assert_eq!(v.tag(), t);
            assert_eq!(js_value_get_tag(v.0), t as i32);
        }
    }

    // ---- mkval is only for Int/Bool/CatchOffset (small immediates) ----
    #[test]
    fn mkval_works_for_int_bool_catchoffset() {
        assert_eq!(
            JSValueTag::Int.mkval(0x1234),
            ((JSValueTag::Int as u64) << 48) | 0x1234
        );
        assert_eq!(
            JSValueTag::Bool.mkval(1),
            ((JSValueTag::Bool as u64) << 48) | 1
        );
        assert_eq!(
            JSValueTag::CatchOffset.mkval(0xABCD),
            ((JSValueTag::CatchOffset as u64) << 48) | 0xABCD
        );

        // ZERO / FALSE / TRUE are constructed via mkval(Int/Bool)
        assert_eq!(JSValue::ZERO.tag(), JSValueTag::Int);
        assert_eq!(JSValue::ZERO.as_int(), Some(0));
        assert_eq!(JSValue::FALSE.tag(), JSValueTag::Bool);
        assert_eq!(JSValue::TRUE.tag(), JSValueTag::Bool);
        assert_eq!(JSValue::FALSE.as_bool(), Some(false));
        assert_eq!(JSValue::TRUE.as_bool(), Some(true));
    }

    // mkval rejects non-immediate tags (Null/Undefined/.../String/Object).
    // This constraint relies on debug_assert and only panics in debug builds.
    #[test]
    #[should_panic]
    #[cfg(debug_assertions)]
    fn mkval_rejects_non_immediate_tags() {
        let _ = JSValueTag::Null.mkval(0);
    }

    // ---- mknode is only for types backed by a gc node pointer ----
    #[test]
    fn mknode_encodes_gc_node_pointers() {
        let addr: u64 = 0x0000_0000_0000_1234;
        let node = fake_node(addr);
        let cases = [
            (JSValueTag::Object, "Object"),
            (JSValueTag::String, "String"),
            (JSValueTag::FunctionBytecode, "FunctionBytecode"),
            (JSValueTag::Module, "Module"),
        ];
        for (tag, name) in cases {
            let raw = tag.mknode(node);
            assert_eq!(js_value_get_tag(raw), tag as i32, "raw tag for {name}");
            let v = JSValue(raw, PhantomData);
            assert_eq!(v.tag(), tag, "JSValue::tag for {name}");
            // as_gc_node should return the original pointer (not dereferenced)
            let got = v.as_gc_node().expect("{name} must be a gc node");
            assert_eq!(got.as_ptr() as u64, addr, "ptr payload for {name}");
        }
    }

    #[test]
    fn mknode_encodes_symbol() {
        let raw = JSValueTag::Symbol.mknode(fake_node(0x1234));
        assert_eq!(js_value_get_tag(raw), JSValueTag::Symbol as i32);
        assert_eq!(JSValue(raw, PhantomData).tag(), JSValueTag::Symbol);
    }

    // ---- float / NaN / ptr are set and recognized correctly ----
    #[test]
    fn float64_roundtrip_and_recognition() {
        let cases: &[(f64, Option<f64>)] = &[
            (0.0, Some(0.0)),
            (1.0, Some(1.0)),
            (-1.0, Some(-1.0)),
            (1.5, Some(1.5)),
            (-2.5, Some(-2.5)),
            (f64::MAX, Some(f64::MAX)),
            (f64::MIN, Some(f64::MIN)),
            (f64::MIN_POSITIVE, Some(f64::MIN_POSITIVE)),
            (5e-324, Some(5e-324)), // subnormal
            (f64::INFINITY, Some(f64::INFINITY)),
            (f64::NEG_INFINITY, Some(f64::NEG_INFINITY)),
            (f64::NAN, None), // canonicalized to a quiet NaN
        ];
        for &(d, expected) in cases {
            let v = JSValue(js_new_float64(d), PhantomData);
            // must be recognized as float64
            assert!(
                JSValueTag::is_float64(js_value_get_tag(v.0)),
                "is_float64 for {d:?}"
            );
            assert_eq!(v.tag(), JSValueTag::Float64, "tag for {d:?}");

            let decoded = js_value_get_float64(v.0);
            match expected {
                Some(e) => assert_eq!(decoded, e, "roundtrip for {d:?}"),
                None => assert!(decoded.is_nan(), "NaN canonicalization for {d:?}"),
            }
            match expected {
                Some(e) => assert_eq!(v.as_float(), Some(e), "as_float for {d:?}"),
                None => assert!(v.as_float().unwrap().is_nan(), "as_float NaN for {d:?}"),
            }
        }

        // NAN constant
        assert_eq!(JSValue::NAN.tag(), JSValueTag::Float64);
        assert!(JSValue::NAN.as_float().unwrap().is_nan());
    }

    // Negative infinity must not be misread as String (high 16 bits are exactly
    // 0xFFF0, which maps to -9 after the shift).
    #[test]
    fn neg_infinity_is_float_not_string() {
        let v = JSValue(js_new_float64(f64::NEG_INFINITY), PhantomData);
        assert_ne!(v.tag(), JSValueTag::String);
        assert_eq!(v.tag(), JSValueTag::Float64);
        assert_eq!(v.as_float(), Some(f64::NEG_INFINITY));
    }

    // A string node and negative infinity must have distinguishable tags.
    #[test]
    fn string_node_distinct_from_neg_infinity() {
        let s = JSValue(JSValueTag::String.mknode(fake_node(0x1234)), PhantomData);
        let ninf = JSValue(js_new_float64(f64::NEG_INFINITY), PhantomData);
        assert_ne!(s.tag(), ninf.tag());
        assert_eq!(s.tag(), JSValueTag::String);
        assert_eq!(ninf.tag(), JSValueTag::Float64);
    }

    // ---- norm_tag classifies correctly ----
    #[test]
    fn norm_tag_classifies() {
        assert_eq!(
            JSValueTag::norm_tag(js_value_get_tag(JSValue::UNDEFINED.0)),
            JSValueTag::Undefined
        );
        assert_eq!(
            JSValueTag::norm_tag(js_value_get_tag(JSValue::NULL.0)),
            JSValueTag::Null
        );
        // raw tags in the float range should normalize to Float64
        assert_eq!(JSValueTag::norm_tag(7), JSValueTag::Float64);
        assert_eq!(JSValueTag::norm_tag(-16), JSValueTag::Float64); // neg-inf raw tag
    }

    // ---- All known tags are distinct, none is recognized as float, and each
    // round-trips unchanged ----
    #[test]
    fn all_known_tags_disjoint_and_recognized() {
        let all = [
            JSValueTag::String,
            JSValueTag::Symbol,
            JSValueTag::StringRope,
            JSValueTag::Module,
            JSValueTag::FunctionBytecode,
            JSValueTag::Object,
            JSValueTag::Int,
            JSValueTag::Bool,
            JSValueTag::Null,
            JSValueTag::Undefined,
            JSValueTag::Uninitialized,
            JSValueTag::CatchOffset,
            JSValueTag::Exception,
        ];
        for &t in &all {
            let raw = t as i32;
            // a known tag must not be in the float range
            assert!(!JSValueTag::is_float64(raw), "{t:?} must not be float");
            // from_raw / norm_tag must round-trip unchanged
            assert_eq!(JSValueTag::from_raw(raw), Some(t), "from_raw for {t:?}");
            assert_eq!(JSValueTag::norm_tag(raw), t, "norm_tag for {t:?}");
        }
        // any two known tags have distinct raw tags
        for &a in &all {
            for &b in &all {
                if a != b {
                    assert_ne!(a as i32, b as i32, "{a:?} collides with {b:?}");
                }
            }
        }
    }

    // ---- Exhaust f64 high 16 bits to prove "no finite double collides with the
    // tag window [-8,6]" ----
    // This is the core invariant of the encoding scheme: if JS_FLOAT64_TAG_ADDEND
    // is ever changed, this test will immediately expose any finite double being
    // misread as a tag (or vice versa).
    #[test]
    fn no_double_collides_with_tags() {
        const ADDEND: u64 = 0xFFF9;
        // Check all 65536 possible values of the f64 high 16 bits one by one:
        // as long as it represents a non-NaN double (exponent field exp != 0x7FF),
        // its encoded raw tag can never fall into the non-float window [-8, 6].
        // The exp == 0x7FF region (NaN/±Inf) is canonicalized to a quiet NaN or
        // encoded directly as ±Inf, with its raw tag outside the window.
        for h in 0u64..=0xFFFF {
            let exp = (h >> 4) & 0x7FF; // f64 exponent field (11 bits)
            let raw_tag = ((h as u16).wrapping_sub(ADDEND as u16)) as i16 as i32;
            if exp != 0x7FF {
                // Finite or subnormal (non-NaN/Inf): must be recognized as float,
                // never as a tag.
                assert!(
                    raw_tag < -8 || raw_tag > 6,
                    "h={h:#06x} raw_tag={raw_tag} would collide with a tag"
                );
            }
        }
        // ±Inf checked separately: its raw tag must also be outside the window.
        let pos_inf = 0x7FF0u64;
        let neg_inf = 0xFFF0u64;
        let pos_raw = ((pos_inf as u16).wrapping_sub(ADDEND as u16)) as i16 as i32;
        let neg_raw = ((neg_inf as u16).wrapping_sub(ADDEND as u16)) as i16 as i32;
        assert!(pos_raw > 6, "pos inf raw_tag={pos_raw} must be float");
        assert!(neg_raw < -8, "neg inf raw_tag={neg_raw} must be float");
    }

    // ---- Representative raw float tags should all be recognized as float ----
    #[test]
    fn float_raw_tags_recognized() {
        for raw in [7i32, -9, -16, 100, -100, 32767, -32768] {
            assert!(JSValueTag::is_float64(raw), "raw {raw} should be float");
        }
    }

    // ---- Negative zero (-0.0) round-trip and recognition ----
    #[test]
    fn neg_zero_roundtrip() {
        let v = JSValue(js_new_float64(-0.0), PhantomData);
        assert!(v.is_float64());
        assert_eq!(v.tag(), JSValueTag::Float64);
        let d = js_value_get_float64(v.0);
        // Compare by bit pattern to avoid -0.0 == 0.0 masking the difference
        assert_eq!(d.to_bits(), (-0.0f64).to_bits());
        assert_eq!(v.as_float(), Some(-0.0));
    }

    // ---- Int payload round-trip (including negatives / i32 boundaries) ----
    #[test]
    fn int_payload_roundtrip() {
        let cases = [0i32, 1, -1, i32::MIN, i32::MAX, 123456789, -123456789];
        for &n in &cases {
            let v = JSValue(JSValueTag::Int.mkval(n as u32), PhantomData);
            assert_eq!(v.tag(), JSValueTag::Int);
            assert!(!v.is_float64());
            assert_eq!(v.as_int(), Some(n), "as_int for {n}");
            assert_eq!(v.payload_i32(), n, "payload_i32 for {n}");
        }
    }

    // ---- mknode preserves the full 48-bit payload (including the all-ones bound) ----
    #[test]
    fn mknode_payload_preserved_low48() {
        let addrs = [
            0x10u64,
            0x1234,
            0x0000_FFFF_FFFF_FFFF,
            0x0000_ABCD_1234_5678,
        ];
        for &a in &addrs {
            let raw = JSValueTag::Object.mknode(fake_node(a));
            let v = JSValue(raw, PhantomData);
            assert_eq!(v.tag(), JSValueTag::Object);
            let got = v.as_gc_node().expect("object is gc node");
            assert_eq!(got.as_ptr() as u64, a, "ptr for {a:#x}");
        }
    }

    // ---- CatchOffset payload round-trip ----
    #[test]
    fn catch_offset_roundtrip() {
        let cases = [0i32, 1, -1, 100, -100, i32::MIN, i32::MAX];
        for &n in &cases {
            let v = JSValue(JSValueTag::CatchOffset.mkval(n as u32), PhantomData);
            assert_eq!(v.tag(), JSValueTag::CatchOffset);
            assert!(!v.is_float64());
            assert_eq!(v.as_catch_offset(), Some(n), "as_catch_offset for {n}");
        }
    }

    // ---- ZERO(Int) / FALSE(Bool) / TRUE(Bool) are distinct (even when payloads
    // are both 0/1) ----
    #[test]
    fn zero_false_true_are_distinct_values() {
        assert_ne!(JSValue::ZERO, JSValue::FALSE);
        assert_ne!(JSValue::ZERO, JSValue::TRUE);
        assert_ne!(JSValue::FALSE, JSValue::TRUE);
        assert_eq!(JSValue::FALSE.tag(), JSValueTag::Bool);
        assert_eq!(JSValue::TRUE.tag(), JSValueTag::Bool);
        assert_eq!(JSValue::ZERO.tag(), JSValueTag::Int);
    }

    // ---- StringRope(-7) is inside the tag window and correctly recognized (not float) ----
    #[test]
    fn string_rope_tag_recognized() {
        assert_eq!(JSValueTag::from_raw(-7), Some(JSValueTag::StringRope));
        assert!(!JSValueTag::is_float64(-7));
        assert_eq!(JSValueTag::norm_tag(-7), JSValueTag::StringRope);
    }
}
