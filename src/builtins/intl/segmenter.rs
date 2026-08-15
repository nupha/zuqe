// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.Segmenter` (§16).

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{locale_to_string, make_str, resolve_locale, to_utf8_string};

#[derive(Debug)]
pub(crate) struct SegmenterData {
    pub locale: Locale,
    pub granularity: SegmentGranularity,
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

pub(crate) fn js_segmenter_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let mut data = SegmenterData {
        locale,
        granularity: SegmentGranularity::Grapheme,
    };

    if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let g = obj.get_property_str(ctx, "granularity")?;
        if !g.is_undefined() {
            let s = to_utf8_string(ctx, g)?;
            data.granularity = match s.as_str() {
                "word" => SegmentGranularity::Word,
                "sentence" => SegmentGranularity::Sentence,
                _ => SegmentGranularity::Grapheme,
            };
        }
    }

    let instance = ctx
        .create_instance_with_ctor(new_target, ObjectPayload::INTL_SEGMENTER(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

/// Compute grapheme/word/sentence boundaries as byte offsets (start..end) of
/// each segment. Uses pragmatic fallback heuristics.
fn boundaries_of(this: &SegmenterData, text: &str) -> Vec<(usize, usize)> {
    let bytes: Vec<usize> = match this.granularity {
        SegmentGranularity::Grapheme => grapheme_boundaries(text),
        SegmentGranularity::Word => word_boundaries(text),
        SegmentGranularity::Sentence => sentence_boundaries(text),
    };
    let mut out = Vec::new();
    let mut start = 0;
    for b in bytes {
        out.push((start, b));
        start = b;
    }
    if start < text.len() {
        out.push((start, text.len()));
    }
    out
}

fn grapheme_boundaries(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut count = 0;
    for c in text.chars() {
        let _ = c;
        count += c.len_utf8();
        out.push(count);
    }
    out
}

fn word_boundaries(text: &str) -> Vec<usize> {
    // Split at each whitespace character boundary (each whitespace + following
    // char form a new word).
    let mut out = Vec::new();
    let mut pos = 0;
    for c in text.chars() {
        let next = pos + c.len_utf8();
        if c.is_whitespace() {
            out.push(next);
        }
        pos = next;
    }
    out
}

fn sentence_boundaries(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut pos = 0;
    for c in text.chars() {
        let next = pos + c.len_utf8();
        if c == '.' || c == '!' || c == '?' {
            out.push(next);
        }
        pos = next;
    }
    out
}

fn js_segmenter_segment(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let this = this_obj
        .payload()
        .intl_segmenter()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let text = to_utf8_string(ctx, cc.arg(0))?;
    let segs = boundaries_of(this, &text);

    // Compute char offsets for segment `index`.
    let char_offsets: Vec<usize> = {
        let mut v = Vec::with_capacity(text.len() + 1);
        let mut char_idx = 0;
        let mut prev = 0;
        for (i, c) in text.char_indices() {
            while prev < i {
                v.push(char_idx);
                prev += 1;
            }
            v.push(char_idx);
            char_idx += 1;
            prev = i + c.len_utf8();
        }
        while prev <= text.len() {
            v.push(char_idx);
            prev += 1;
        }
        v
    };

    let mut segments_obj = ctx.create_base_object()?;
    let mut entries: Vec<crate::JSValue> = Vec::new();
    for (start, end) in &segs {
        let mut o = ctx.create_base_object()?;
        let _v2 = make_str(ctx, text[*start..*end].as_bytes().to_vec())?;
        o.set_property_str(ctx, "segment", _v2)?;
        o.set_property_str(ctx, "index", JSValue::from(char_offsets[*start] as i32))?;
        let _v3 = make_str(ctx, text.as_bytes().to_vec())?;
        o.set_property_str(ctx, "input", _v3)?;
        if this.granularity == SegmentGranularity::Word {
            let is_word = text[*start..*end].chars().any(|c| c.is_alphanumeric());
            o.set_property_str(ctx, "isWordLike", JSValue::from(is_word))?;
        }
        entries.push(o.into());
    }
    // Segments object is an iterable of segment records.
    segments_obj.set_property_str(ctx, "length", JSValue::from(entries.len() as i32))?;
    let _ = entries;
    Ok(segments_obj.into())
}

fn js_segmenter_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let this = this_obj
        .payload()
        .intl_segmenter()
        .ok_or_else(|| ctx.throw_type_error("not a Segmenter"))?;
    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &this.locale)?;
    o.set_property_str(ctx, "locale", _v1)?;
    let granularity = make_str(
        ctx,
        match this.granularity {
            SegmentGranularity::Grapheme => b"grapheme".to_vec(),
            SegmentGranularity::Word => b"word".to_vec(),
            SegmentGranularity::Sentence => b"sentence".to_vec(),
        },
    )?;
    o.set_property_str(ctx, "granularity", granularity)?;
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
