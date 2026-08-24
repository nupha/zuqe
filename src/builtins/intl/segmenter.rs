// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.Segmenter` (§16).

use icu::locale::Locale;
use icu::segmenter::{
    GraphemeClusterSegmenter, SentenceSegmenter, WordSegmenter,
    options::{SentenceBreakInvariantOptions, WordBreakInvariantOptions},
};

use crate::{
    Atom, Callback, ClassCtorDef, ClassDef, ScopedContext,
    builtins::iterator::make_iterator_next_result,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object, ObjectPayload},
    value::JSValue,
};

use super::common::{
    canonicalize_locale_list, locale_to_string, make_str, resolve_locale, to_utf8_string,
};

#[derive(Debug)]
pub struct SegmenterData {
    pub(crate) locale: Locale,
    pub(crate) granularity: SegmentGranularity,
}

impl GcTrace for SegmenterData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SegmentGranularity {
    Grapheme,
    Word,
    Sentence,
}

pub(crate) static CLASS_DEF_SEGMENTER: ClassDef = ClassDef::builtin(
    Atom::CONST_Segmenter,
    js_segmenter_proto_funcs,
    Some(ClassCtorDef::new(js_segmenter_constructor, 0, true)),
);

/// Bound-data slots shared by the Segments/Iterator native functions.
const D_SEGMENTER: usize = 0; // Intl.Segmenter instance
const D_INPUT: usize = 1; // input string value
const D_SEGMENTS: usize = 2; // Segments object (branding)
const D_POS: usize = 3; // iterator state: next UTF-16 index
const D_ITERATOR: usize = 4; // iterator object (branding)

pub(crate) fn js_segmenter_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    // ECMA-402 §16 step 1: NewTarget must be defined.
    if new_target.is_undefined() {
        return Err(ctx.throw_type_error("Intl.Segmenter must be called with 'new'"));
    }
    // OrdinaryCreateFromConstructor: reading new_target.prototype is
    // observable and must precede any argument/option validation.
    super::common::observable_proto_get(ctx, new_target)?;

    let locale = resolve_segmenter_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let mut granularity = SegmentGranularity::Grapheme;
    if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        // GetOption order matters (localeMatcher is read before granularity).
        let matcher = get_string_option(ctx, &obj, "localeMatcher", &["lookup", "best fit"])?;
        let _ = matcher;
        if let Some(g) =
            get_string_option(ctx, &obj, "granularity", &["grapheme", "word", "sentence"])?
        {
            granularity = match g.as_str() {
                "word" => SegmentGranularity::Word,
                "sentence" => SegmentGranularity::Sentence,
                _ => SegmentGranularity::Grapheme,
            };
        }
    }

    let data = SegmenterData {
        locale,
        granularity,
    };
    let instance = ctx
        .create_instance_with_ctor(new_target, ObjectPayload::INTL_SEGMENTER(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

/// GetOption for string-valued options: read the property, coerce via
/// ToString, and throw RangeError when not in `values`. `Ok(None)` when the
/// property is absent/undefined.
fn get_string_option(
    ctx: &mut ScopedContext,
    obj: &Object,
    key: &str,
    values: &[&str],
) -> Result<Option<String>, i32> {
    let v = obj.get_property_str(ctx, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = to_utf8_string(ctx, v)?;
    if !values.contains(&s.as_str()) {
        return Err(ctx.throw_range_error("invalid option value"));
    }
    Ok(Some(s))
}

/// Primary language subtags treated as available when resolving the Segmenter
/// locale (ECMA-402 ResolveLocale skips unavailable requested locales, e.g.
/// `["xyz", "ar"]` resolves to `ar`). ICU4X compiled data cannot enumerate its
/// locales, so this mirrors the other static availability tables in this
/// module. Full ISO-639-1 code list.
// ponytail: static availability table; derive from CLDR if coverage gaps show up
const AVAILABLE_LANGUAGES: &[&str] = &[
    "aa", "ab", "af", "ak", "am", "ar", "as", "av", "ay", "az", "ba", "be", "bg", "bi", "bm", "bn",
    "bo", "br", "bs", "ca", "ce", "ch", "co", "cr", "cs", "cu", "cv", "cy", "da", "de", "dv", "dz",
    "ee", "el", "en", "eo", "es", "et", "eu", "fa", "ff", "fi", "fj", "fo", "fr", "fy", "ga", "gd",
    "gl", "gn", "gu", "gv", "ha", "he", "hi", "ho", "hr", "ht", "hu", "hy", "hz", "ia", "id", "ie",
    "ig", "ii", "ik", "io", "is", "it", "iu", "ja", "jv", "ka", "kg", "ki", "kj", "kk", "kl", "km",
    "kn", "ko", "kr", "ks", "ku", "kv", "kw", "ky", "la", "lb", "lg", "li", "ln", "lo", "lt", "lu",
    "lv", "mg", "mh", "mi", "mk", "ml", "mn", "mr", "ms", "mt", "my", "na", "nb", "nd", "ne", "ng",
    "nl", "nn", "no", "nr", "nv", "ny", "oc", "oj", "om", "or", "os", "pa", "pi", "pl", "ps", "pt",
    "qu", "rm", "rn", "ro", "ru", "rw", "sa", "sc", "sd", "se", "sg", "si", "sk", "sl", "sm", "sn",
    "so", "sq", "sr", "ss", "st", "su", "sv", "sw", "ta", "te", "tg", "th", "ti", "tk", "tl", "tn",
    "to", "tr", "ts", "tt", "tw", "ty", "ug", "uk", "ur", "uz", "ve", "vi", "vo", "wa", "wo", "xh",
    "yi", "yo", "za", "zh", "zu",
];

/// Resolve the effective Segmenter locale: first requested locale whose
/// primary language is available; the runtime default otherwise.
fn resolve_segmenter_locale(ctx: &mut ScopedContext, locales: JSValue) -> Result<Locale, i32> {
    if !locales.is_undefined() {
        let list = canonicalize_locale_list(ctx, locales)?;
        if let Some(obj) = list.as_object() {
            let len = obj.get_length_prop(ctx)?;
            for i in 0..len {
                let v = obj.get_property_i64(ctx, i)?;
                let tag = to_utf8_string(ctx, v)?;
                let lang = tag.split(['-', '_']).next().unwrap_or("");
                if AVAILABLE_LANGUAGES.contains(&lang) {
                    return tag.parse().map_err(|_| -1);
                }
            }
        }
    }
    resolve_locale(ctx, JSValue::UNDEFINED)
}

/// One segment: UTF-16 code-unit range (+ word-likeness).
struct Seg {
    start_u: usize,
    end_u: usize,
    word_like: bool,
}

/// Extract a JS string as raw UTF-16 code units (lone surrogates preserved).
fn js_to_utf16(s: &crate::string::StringRef) -> Vec<u16> {
    let c = s.content();
    if let Some(w) = c.utf16() {
        w.to_vec()
    } else {
        (0..c.len()).filter_map(|i| c.get(i)).collect()
    }
}

fn compute_segments(granularity: SegmentGranularity, units: &[u16]) -> Vec<Seg> {
    match granularity {
        SegmentGranularity::Grapheme => collect(
            GraphemeClusterSegmenter::new()
                .segment_utf16(units)
                .map(|b| (b, false)),
        ),
        SegmentGranularity::Word => collect(
            WordSegmenter::new_auto(WordBreakInvariantOptions::default())
                .segment_utf16(units)
                .iter_with_word_type()
                .map(|(b, t)| (b, t.is_word_like())),
        ),
        SegmentGranularity::Sentence => collect(
            SentenceSegmenter::new(SentenceBreakInvariantOptions::default())
                .segment_utf16(units)
                .map(|b| (b, false)),
        ),
    }
}

/// Build segments from code-unit boundaries (ICU yields 0..len; duplicates
/// are ignored).
fn collect(bounds: impl Iterator<Item = (usize, bool)>) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut prev = 0usize;
    for (b, word_like) in bounds.chain([(usize::MAX, false)]) {
        if b <= prev || b == usize::MAX {
            continue;
        }
        out.push(Seg {
            start_u: prev,
            end_u: b,
            word_like,
        });
        prev = b;
    }
    out
}

fn js_segmenter_segment(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    let this_obj = this_val
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    if this_obj.payload().intl_segmenter().is_none() {
        return Err(ctx.throw_type_error("not a Segmenter"));
    }
    let sref = cc.arg(0).to_string(ctx)?;
    let input = JSValue::make_string(sref);
    let mut segments_obj = ctx.create_base_object()?;

    let binding: Box<[JSValue]> =
        Box::new([this_val, input, segments_obj.into(), JSValue::UNDEFINED]);
    let containing = ctx.create_native_function_with_data(
        Callback::Func(js_segments_containing),
        "containing",
        1,
        0,
        binding.clone(),
    )?;
    segments_obj.define_property_value_str(
        ctx,
        "containing",
        containing.into(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    )?;
    let iterator = ctx.create_native_function_with_data(
        Callback::Func(js_segments_iterate),
        "[Symbol.iterator]",
        0,
        0,
        binding,
    )?;
    segments_obj.define_property_value_str(
        ctx,
        "[Symbol.iterator]",
        iterator.into(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    )?;
    Ok(segments_obj.into())
}

/// `%Segments.prototype%.containing(index)`
fn js_segments_containing(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let segments_val = cc.data(D_SEGMENTS);
    if cc.this() != segments_val {
        return Err(ctx.throw_type_error("not a Segments object"));
    }
    let seg_val = cc.data(D_SEGMENTER);
    let seg_obj = seg_val
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let data = seg_obj
        .payload()
        .intl_segmenter()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let sref = cc.data(D_INPUT).to_string(ctx)?;
    let units = js_to_utf16(&sref);

    // ToIntegerOrInfinity.
    let n = cc.arg(0).to_number(ctx)?.to_f64();
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    if n < 0.0 || n >= units.len() as f64 {
        return Ok(JSValue::UNDEFINED);
    }
    let i = n as usize;

    let segs = compute_segments(data.granularity, &units);
    let seg = segs
        .iter()
        .find(|s| s.start_u <= i && i < s.end_u)
        .ok_or(-1)?;
    create_segment_record(ctx, &units, seg, data.granularity, cc.data(D_INPUT))
}

/// `%Segments.prototype%[@@iterator]()`: brand-check, then build a fresh
/// iterator with its own position state (nested iteration safe).
fn js_segments_iterate(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let segments_val = cc.data(D_SEGMENTS);
    if cc.this() != segments_val {
        return Err(ctx.throw_type_error("not a Segments object"));
    }

    let mut iter_obj = ctx.create_base_object()?;
    let binding: Box<[JSValue]> = Box::new([
        cc.data(D_SEGMENTER),
        cc.data(D_INPUT),
        cc.data(D_SEGMENTS),
        JSValue::from(0i32), // D_POS: iteration start
        iter_obj.into(),
    ]);
    let next = ctx.create_native_function_with_data(
        Callback::Func(js_segment_iterator_next),
        "next",
        0,
        0,
        binding,
    )?;
    iter_obj.define_property_value_str(
        ctx,
        "next",
        next.into(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    )?;
    Ok(iter_obj.into())
}

/// `%SegmentIterator.prototype%.next()`
fn js_segment_iterator_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let iter_val = cc.data(D_ITERATOR);
    if cc.this() != iter_val {
        return Err(ctx.throw_type_error("not a Segment Iterator"));
    }
    let seg_val = cc.data(D_SEGMENTER);
    let seg_obj = seg_val
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let data = seg_obj
        .payload()
        .intl_segmenter()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let sref = cc.data(D_INPUT).to_string(ctx)?;
    let units = js_to_utf16(&sref);

    let cur = cc.data(D_POS).to_number(ctx)?.to_f64().trunc();
    let segs = compute_segments(data.granularity, &units);
    let seg = match segs.iter().find(|s| s.end_u as f64 > cur) {
        Some(s) => s,
        None => return make_iterator_next_result(ctx, JSValue::UNDEFINED, true),
    };
    // Advance the iterator's own state slot.
    let mut cc = cc;
    cc.replace_data(D_POS, JSValue::from(seg.end_u as i32));
    let record = create_segment_record(ctx, &units, seg, data.granularity, cc.data(D_INPUT))?;
    make_iterator_next_result(ctx, record, false)
}

/// CreateSegmentDataObject: `{segment, index, input[, isWordLike]}` in that
/// property order, created as data properties (no setter traps).
fn create_segment_record(
    ctx: &mut ScopedContext,
    units: &[u16],
    seg: &Seg,
    granularity: SegmentGranularity,
    input_val: JSValue,
) -> Result<JSValue, i32> {
    let mut o = ctx.create_base_object()?;
    // Build the segment string from raw code units so lone surrogates
    // round-trip exactly.
    let v_seg = ctx.create_string_with_buf(crate::string::StringBuf::Utf16(
        units[seg.start_u..seg.end_u].to_vec(),
    ))?;
    o.define_property_value_str(ctx, "segment", JSValue::make_string(v_seg), JS_PROP_C_W_E)?;
    o.define_property_value_str(
        ctx,
        "index",
        JSValue::from(seg.start_u as i32),
        JS_PROP_C_W_E,
    )?;
    o.define_property_value_str(ctx, "input", input_val, JS_PROP_C_W_E)?;
    if granularity == SegmentGranularity::Word {
        o.define_property_value_str(
            ctx,
            "isWordLike",
            JSValue::from(seg.word_like),
            JS_PROP_C_W_E,
        )?;
    }
    Ok(o.into())
}

fn js_segmenter_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;

    let seg = this_obj
        .payload()
        .intl_segmenter()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;

    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &seg.locale)?;
    o.define_property_value_str(ctx, "locale", _v1, JS_PROP_C_W_E)?;
    let granularity = make_str(
        ctx,
        match seg.granularity {
            SegmentGranularity::Grapheme => b"grapheme".to_vec(),
            SegmentGranularity::Word => b"word".to_vec(),
            SegmentGranularity::Sentence => b"sentence".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "granularity", granularity, JS_PROP_C_W_E)?;

    Ok(o.into())
}

const js_segmenter_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("segment", 1, js_segmenter_segment, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_segmenter_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.Segmenter",
        JS_PROP_CONFIGURABLE,
    ),
];
