// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

// add in Cargo.toml
// onig = "6.5"
// onig_sys = "*"

use std::{borrow::Cow, cell::Cell, ops::Range, rc::Rc};

use gc_lite::{GcRef, GcTrace};
use onig::{EncodedBytes, Regex, RegexOptions, Syntax};
use onig_sys::{OnigEncoding, OnigEncodingUTF8};

use crate::{
    JS_CLASS_REGEXP, JS_CLASS_REGEXP_STRING_ITERATOR,
    atom::Atom,
    class::ClassID,
    context::{Context, ContextImpl},
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, Object, ObjectImpl, ObjectPayload},
    object_cast,
    string::{CharCode, StringData, StringRef, Utf8Stream, map_utf8_to_utf16_range},
    value::JSValue,
};

use super::{
    js_regexp_constructor, js_regexp_funcs, js_regexp_proto_funcs,
    js_regexp_string_iterator_proto_funcs,
};

pub const RE_HEADER_FLAGS: usize = 0;
pub const RE_HEADER_CAPTURE_COUNT: usize = 1;
pub const RE_HEADER_STACK_SIZE: usize = 2;
pub const RE_HEADER_LEN: usize = 7;

pub const LRE_FLAG_GLOBAL: i32 = 1 << 0;
pub const LRE_FLAG_IGNORECASE: i32 = 1 << 1;
pub const LRE_FLAG_MULTILINE: i32 = 1 << 2;
pub const LRE_FLAG_DOTALL: i32 = 1 << 3;
pub const LRE_FLAG_UTF16: i32 = 1 << 4;
pub const LRE_FLAG_STICKY: i32 = 1 << 5;
pub const LRE_FLAG_INDICES: i32 = 1 << 6; // Unused by libregexp, just recorded.
pub const LRE_FLAG_NAMED_GROUPS: i32 = 1 << 7; // named groups are present in the regexp

#[derive(Debug)]
pub struct RegExpStringIterPayload {
    iterating_regexp: Object,
    iterated_string: StringRef,
    pub global: bool,
    pub unicode: bool,
    done: Cell<bool>,
}

impl GcTrace for RegExpStringIterPayload {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.iterated_string.trace(tracer);
        self.iterating_regexp.trace(tracer);
    }
}

impl RegExpStringIterPayload {
    pub fn new(regexp: Object, str: StringRef, global: bool, unicode: bool) -> Box<Self> {
        Box::new(Self {
            iterating_regexp: regexp,
            iterated_string: str,
            global,
            unicode,
            done: Cell::new(false),
        })
    }

    #[inline(always)]
    pub const fn regexp(&self) -> Object {
        self.iterating_regexp
    }

    #[inline(always)]
    pub const fn string(&self) -> StringRef {
        self.iterated_string
    }

    #[inline(always)]
    pub const fn is_global(&self) -> bool {
        self.global
    }

    #[inline(always)]
    pub const fn is_unicode(&self) -> bool {
        self.unicode
    }

    #[inline(always)]
    pub const fn done(&self) -> bool {
        self.done.get()
    }

    #[inline(always)]
    pub fn set_done(&self, v: bool) {
        self.done.set(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum RegExpExecMode {
    CaptureOnce,
    Match,
}

#[derive(Clone, Debug)]
pub struct RegExpData {
    pub(crate) pattern: StringData,
    pub(crate) flags_str: StringData,
    pub(crate) flags: u8,
    pub(super) regex: Rc<Regex>,
}

impl GcTrace for RegExpData {
    #[inline(always)]
    fn trace(&self, _: &mut gc_lite::GcTracer) {}
}

impl RegExpData {
    pub(crate) fn compile_re(
        ctx: &mut ContextImpl,
        pattern: &str,
        flags: impl Iterator<Item = char>,
    ) -> Result<(Regex, u8), i32> {
        let mut ff = 0u8;
        let mut opts = RegexOptions::empty();
        let mut is_unicode = false;

        for c in flags {
            match c {
                'd' => {
                    ff |= LRE_FLAG_INDICES as u8;
                    //  opts|= RegexOptions::;
                }
                'g' => {
                    ff |= LRE_FLAG_GLOBAL as u8;
                }
                'i' => {
                    ff |= LRE_FLAG_IGNORECASE as u8;
                    opts |= RegexOptions::REGEX_OPTION_IGNORECASE;
                }
                'm' => {
                    ff |= LRE_FLAG_MULTILINE as u8;
                    opts |= RegexOptions::REGEX_OPTION_MULTILINE;
                }
                's' => {
                    ff |= LRE_FLAG_DOTALL as u8;
                    opts |= RegexOptions::REGEX_OPTION_SINGLELINE;
                }
                'u' | 'v' => {
                    ff |= LRE_FLAG_UTF16 as u8;
                    is_unicode = true;
                }
                'y' => {
                    ff |= LRE_FLAG_STICKY as u8;
                }
                _ => {
                    ctx.throw_syntax_error("Invalid regular expression flags");
                    return Err(-1);
                }
            }
        }

        // in some cases, regexp is supposed to enable unicode mode by default:
        // 1. pattern contains non-ascii characters
        // 2. pattern has \xFF or \uFFFF pattern char.
        if !pattern.is_ascii() {
            ff |= LRE_FLAG_UTF16 as u8;
        }

        let re = Regex::with_options_and_encoding(
            EncodedBytes::from_parts(
                pattern.as_bytes(),
                &raw mut OnigEncodingUTF8 as OnigEncoding,
            ),
            opts,
            Syntax::emacs(),
        )
        .map_err(|e| {
            ctx.throw_syntax_error(&e.to_string());
            -1
        })?;

        Ok((re, ff))
    }

    pub(crate) fn create(
        ctx: &mut ContextImpl,
        pattern: StringData,
        flags: StringData,
    ) -> Result<Self, i32> {
        let (re, ff) = Self::compile_re(ctx, &pattern.to_utf8(false), flags.chars_lossy())?;
        Ok(Self {
            pattern,
            flags_str: flags,
            regex: Rc::new(re),
            flags: ff,
        })
    }

    pub fn find(&self, content: &StringData, from: usize) -> Box<[Range<usize>]> {
        if from > content.len() {
            return Box::new([]);
        }

        let utf8_str = content.to_utf8_range(from..content.len(), false);

        let is_global = self.flags & LRE_FLAG_GLOBAL as u8 != 0;
        let is_unicode = self.flags & LRE_FLAG_UTF16 as u8 != 0;
        let is_sticky = self.flags & LRE_FLAG_STICKY as u8 != 0;

        let mut cursor = 0;
        let mut last_range = usize::MAX..usize::MAX;
        let mut res = Vec::<Range<usize>>::with_capacity(32);

        for (start, end) in
            self.regex
                .find_iter(utf8_str.as_ref())
                .take(if is_global { usize::MAX } else { 1 })
        {
            if is_sticky && start != cursor {
                break;
            }

            cursor = end;

            let range = if let Some(w) = content.utf16() {
                let s16 = &w[from..];
                map_utf8_to_utf16_range(utf8_str.as_bytes(), s16, start..end)
            } else {
                start..end
            };

            let start_index = range.start + from;
            let end_index = range.end + from;
            let match_range = start_index..end_index;

            if last_range != match_range {
                res.push(match_range.clone());
                last_range = match_range;
            }
        }

        res.into()
    }

    pub fn capture(
        &self,
        content: &StringData,
        from: usize,
    ) -> Option<Box<[(Option<Range<usize>>, Option<String>)]>> {
        if from > content.len() {
            return None;
        }

        let utf8_str = content.to_utf8_range(from..content.len(), false);

        if let Some(caps) = self.regex.captures(utf8_str.as_ref()) {
            if self.flags & LRE_FLAG_STICKY as u8 != 0 && caps.pos(0).is_none_or(|(s, _)| s != 0) {
                return None;
            }

            let cnt = caps.len();
            let mut res = Vec::<(Option<Range<usize>>, Option<String>)>::with_capacity(cnt);

            for i in 0..cnt {
                if let Some((start, end)) = caps.pos(i) {
                    let range = if let Some(w) = content.utf16() {
                        let s16 = &w[from..];
                        map_utf8_to_utf16_range(utf8_str.as_bytes(), s16, start..end)
                    } else {
                        start..end
                    };

                    let start_index = range.start + from;
                    let end_index = range.end + from;
                    res.push((Some(start_index..end_index), None));
                } else {
                    res.push((None, None))
                }
            }

            // update group names
            self.regex.foreach_name(|name, idxs| {
                for i in idxs {
                    let i = *i as usize;
                    debug_assert!(i < res.len());
                    res[i].1 = Some(name.into());
                }
                true
            });

            Some(res.into())
        } else {
            None
        }
    }
}

/// the regexp object
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct RegExp(GcRef<ObjectImpl>);

object_cast!(RegExp, JS_CLASS_REGEXP);

impl RegExp {
    /// create RegExp object with given RegExpData, and constructor.
    pub(crate) fn from(
        ctx: &mut ContextImpl,
        ctor: JSValue,
        re: Box<RegExpData>,
    ) -> Result<Self, i32> {
        let mut o = Self(
            ctx.create_object_from_ctor(ctor, ObjectPayload::REGEXP(re))
                .map_err(|_| -1)?
                .0,
        );
        o.define_property_value(
            ctx,
            Atom::CONST_lastIndex,
            JSValue::Int(0),
            JS_PROP_WRITABLE,
        )?;
        Ok(o)
    }

    #[inline(always)]
    pub fn create(
        ctx: &mut ContextImpl,
        ctor: JSValue,
        pattern: StringData,
        flags: StringData,
    ) -> Result<Self, i32> {
        RegExpData::create(ctx, pattern, flags).and_then(|re| Self::from(ctx, ctor, Box::new(re)))
    }

    pub fn class_init(mut ctx: Context, class_id: ClassID) -> Result<Option<Object>, i32> {
        match class_id {
            JS_CLASS_REGEXP => {
                let mut obj = ctx.create_base_object()?;
                obj.install_field_defs(ctx, &js_regexp_proto_funcs)
                    .unwrap();

                let mut ctor = ctx.create_global_constructor_or_func(
                    "RegExp",
                    js_regexp_constructor,
                    2,
                    obj.into(),
                    0,
                )?;
                ctor.install_field_defs(ctx, &js_regexp_funcs).unwrap();

                ctx.regexp_ctor = ctor;
                Ok(Some(obj))
            }
            JS_CLASS_REGEXP_STRING_ITERATOR => {
                let iter_proto = ctx.iterator_proto()?;
                let mut iter = ctx.create_base_object_with_proto(Some(iter_proto))?;
                iter.install_field_defs(ctx, &js_regexp_string_iterator_proto_funcs)?;

                // let mut cx = ctx.0.borrow_mut();
                // cx.regexp_ctor = ctor;
                // cx.class_proto[JS_CLASS_REGEXP as usize] = obj.into();
                // cx.class_proto[JS_CLASS_REGEXP_STRING_ITERATOR as usize] = iter.into();
                Ok(Some(iter))
            }
            _ => {
                unreachable!()
            }
        }
    }
}

/// build Array object to hold regexp matches.
pub(super) fn make_regexp_capture_array(
    ctx: &mut ContextImpl,
    text: StringRef,
    range_and_name: &[(Option<Range<usize>>, Option<String>)],
    with_indices: bool,
) -> Result<(JSValue, usize), i32> {
    let mut start_index = 0;
    let mut last_index = 0;

    let mut lst = Vec::<JSValue>::new();
    lst.try_reserve(range_and_name.len()).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    let mut indices = if with_indices {
        let mut idx_lst = Vec::<JSValue>::new();
        idx_lst.try_reserve(range_and_name.len()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        Some((idx_lst, JSValue::UNDEFINED))
    } else {
        None
    };

    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
    let mut group = JSValue::UNDEFINED;

    for (i, (range, name)) in range_and_name.iter().enumerate() {
        let val: JSValue = if let Some(range) = range {
            if i == 0 {
                start_index = range.start;
                last_index = range.end;
            }
            text.sub_string(ctx, range.clone())?.into()
        } else {
            JSValue::UNDEFINED
        };

        lst.push(val);

        if let Some(name) = name {
            if group.is_undefined() {
                group = ctx.create_base_object()?.into();
            }
            group.define_property_value_str(ctx, name, val, prop_flags)?;
        }

        if let Some((idxlst, idxgroup)) = &mut indices {
            let idx = if let Some(r) = range {
                crate::array::Array::from_values(
                    ctx,
                    vec![JSValue::Int(r.start as i32), JSValue::Int(r.end as i32)],
                )?
                .into()
            } else {
                JSValue::UNDEFINED
            };
            idxlst.push(idx);

            if let Some(name) = name {
                if idxgroup.is_undefined() {
                    *idxgroup = ctx.create_base_object()?.into();
                }
                idxgroup.define_property_value_str(ctx, name, idx, prop_flags)?;
            }
        }
    }

    let mut arr = crate::array::Array::from_values(ctx, lst)?;
    arr.define_property_value(
        ctx,
        Atom::CONST_index,
        JSValue::Int(start_index as i32),
        prop_flags,
    )?;
    arr.define_property_value(ctx, Atom::CONST_groups, group, prop_flags)?;
    arr.define_property_value(ctx, Atom::CONST_input, text.into(), prop_flags)?;

    if let Some((idxlst, idxgrp)) = indices {
        let mut idxarr = crate::array::Array::from_values(ctx, idxlst)?;
        idxarr.define_property_value(ctx, Atom::CONST_groups, idxgrp, prop_flags)?;
        arr.define_property_value(ctx, Atom::CONST_indices, idxarr.into(), prop_flags)?;
    }

    Ok((arr.into(), last_index))
}
