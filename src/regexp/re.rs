use std::{borrow::Cow, ops::Range, rc::Rc};

use regex::bytes::{Regex, RegexBuilder};

use crate::{
    JS_CLASS_REGEXP,
    atom::consts::*,
    context::JSContext,
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JSObject, ObjectPayload},
    object_cast,
    string::{JSString, StringContent, map_utf8_to_utf16_range},
    value::JSValue,
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
    pub iterating_regexp: JSObject,
    pub iterated_string: StringContent,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum RegExpExecMode {
    CaptureOnce,
    Match,
}

#[derive(Clone, Debug)]
pub struct RegExpData {
    pub(crate) pattern: StringContent,
    pub(crate) flags_str: StringContent,
    pub(crate) flags: u8,
    pub(super) regex: Rc<Regex>,
}

impl RegExpData {
    pub(crate) fn compile_re(
        ctx: &JSContext,
        pattern: &str,
        flags: impl Iterator<Item = char>,
    ) -> Result<(Regex, u8), i32> {
        let mut ff = 0u8;
        for c in flags {
            match c {
                'd' => {
                    ff |= LRE_FLAG_INDICES as u8;
                }
                'g' => {
                    ff |= LRE_FLAG_GLOBAL as u8;
                }
                'i' => {
                    ff |= LRE_FLAG_IGNORECASE as u8;
                }
                'm' => {
                    ff |= LRE_FLAG_MULTILINE as u8;
                }
                's' => {
                    ff |= LRE_FLAG_DOTALL as u8;
                }
                'u' | 'v' => {
                    ff |= LRE_FLAG_UTF16 as u8;
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

        // Note: pattern `[]`， `[^]` is valid for js, but is invalid for Regex.
        // should replace with equivalent pattern for Regex.
        let mut pat: Cow<str> = match pattern {
            "[]" => {
                // pattern `[]` is not allowed in regex.
                // use `$^` to mimic equivelent effect.
                "$^"
            }
            "[^]" => {
                // pattern `[^]` is not allowed in regex.
                // use `.` to mimic equivelent effect.
                "."
            }
            _ => pattern,
        }
        .into();

        // in some cases, regexp is supposed to enable unicode mode by default:
        // 1. pattern contains non-ascii characters
        // 2. pattern has \xFF or \uFFFF pattern char.
        if !pat.is_ascii() {
            ff |= LRE_FLAG_UTF16 as u8;
        }

        let cnt_slash = pat.chars().filter(|x| *x == '\\').count();
        if cnt_slash > 0 {
            // has escape
            let mut pat2 = String::with_capacity(pat.len() + cnt_slash);
            let mut has_unicode = ff & LRE_FLAG_UTF16 as u8 != 0;
            let mut expect_hex: Option<i32> = None;
            let mut it = pattern.chars().peekable();

            while let Some(c) = it.next() {
                pat2.push(c);
                if c == '\\' {
                    pat2.push('\\');
                }

                // match c {
                //     '\\' => {
                //         if let Some(c1) = it.peek() {
                //             match *c1 {
                //                 'x' if !has_unicode => {
                //                     // \xFF
                //                     expect_hex = Some(2);
                //                     it.next();
                //                     pat2.push('x');
                //                 }
                //                 'u' if !has_unicode => {
                //                     // \uFFFF
                //                     expect_hex = Some(4);
                //                     it.next();
                //                     pat2.push('u');
                //                 }
                //                 '>' | '<' => {
                //                     // replace `\>` with `>`, `\\<` with `<`
                //                     pat2.pop(); // chop `\`
                //                 }
                //                 _ => {}
                //             }
                //         }
                //     }
                //     '0'..='9' | 'A'..='F' | 'a'..='f' if !has_unicode => {
                //         if let Some(cnt) = &mut expect_hex {
                //             *cnt -= 1;
                //             if *cnt == 0 {
                //                 ff |= LRE_FLAG_UTF16 as u8;
                //                 has_unicode = true;
                //                 expect_hex = None;
                //             }
                //         }
                //     }
                //     _ => {}
                // }
            }

            pat = Cow::Owned(pat2);
        }

        let re = RegexBuilder::new(pat.as_ref())
            .unicode(ff & LRE_FLAG_UTF16 as u8 != 0)
            .multi_line(ff & LRE_FLAG_MULTILINE as u8 != 0)
            .case_insensitive(ff & LRE_FLAG_IGNORECASE as u8 != 0)
            .build()
            .map_err(|e| {
                ctx.throw_syntax_error(&e.to_string());
                -1
            })?;

        Ok((re, ff))
    }

    pub(crate) fn create(
        ctx: &JSContext,
        pattern: StringContent,
        flags: StringContent,
    ) -> Result<Self, i32> {
        let (re, ff) = Self::compile_re(ctx, &pattern.to_utf8(false), flags.chars_lossy())?;
        Ok(Self {
            pattern,
            flags_str: flags,
            regex: Rc::new(re),
            flags: ff,
        })
    }

    pub fn find(&self, content: &StringContent, from: usize) -> Box<[Range<usize>]> {
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

        for m in self
            .regex
            .find_iter(utf8_str.as_bytes())
            .take(if is_global { usize::MAX } else { 1 })
        {
            if is_sticky && m.start() != cursor {
                break;
            }

            cursor = m.end();

            let range = if let Some(w) = content.utf16() {
                let s16 = &w[from..];
                map_utf8_to_utf16_range(utf8_str.as_bytes(), s16, m.range())
            } else {
                m.range()
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
        content: &StringContent,
        from: usize,
    ) -> Option<Box<[(Option<Range<usize>>, Option<&str>)]>> {
        if from > content.len() {
            return None;
        }

        let utf8_str = content.to_utf8_range(from..content.len(), false);

        if let Some(caps) = self.regex.captures(utf8_str.as_bytes()) {
            if self.flags & LRE_FLAG_STICKY as u8 != 0 && caps.get(0).unwrap().start() != 0 {
                return None;
            }

            let mut res = Vec::<(Option<Range<usize>>, Option<&str>)>::new();

            for (cap, name) in caps.iter().zip(self.regex.capture_names()) {
                if let Some(m) = cap {
                    let range = if let Some(w) = content.utf16() {
                        let s16 = &w[from..];
                        map_utf8_to_utf16_range(utf8_str.as_bytes(), s16, m.range())
                    } else {
                        m.range()
                    };

                    let start_index = range.start + from;
                    let end_index = range.end + from;
                    res.push((Some(start_index..end_index), name));
                } else {
                    res.push((None, name))
                }
            }

            Some(res.into())
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct RegExp(JSObject);

object_cast!(RegExp, JS_CLASS_REGEXP);

impl RegExp {
    pub(crate) fn from(ctx: &JSContext, ctor: &JSValue, re: Box<RegExpData>) -> Result<Self, i32> {
        let o = Self(
            ctx.create_object_apply_ctor(ctor, ObjectPayload::REGEXP(re))
                .map_err(|_| -1)?,
        );
        o.define_property_value(ctx, JS_ATOM_lastIndex, JSValue::Int(0), JS_PROP_WRITABLE)?;
        Ok(o)
    }

    #[inline(always)]
    pub fn create(
        ctx: &JSContext,
        ctor: &JSValue,
        pattern: StringContent,
        flags: StringContent,
    ) -> Result<Self, i32> {
        RegExpData::create(ctx, pattern, flags).and_then(|re| Self::from(ctx, ctor, Box::new(re)))
    }
}

/// build Array object to hold regexp matches.
pub(super) fn make_regexp_capture_array(
    ctx: &JSContext,
    text: JSString,
    range_and_name: &[(Option<Range<usize>>, Option<&str>)],
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

        lst.push(val.clone());

        if let Some(name) = name {
            if group.is_undefined() {
                group = ctx.create_base_object()?.into();
            }
            group.define_property_value_str(ctx, name, val, prop_flags)?;
        }

        if let Some((idxlst, idxgroup)) = &mut indices {
            let idx = if let Some(r) = range {
                ctx.create_array_with_values(vec![
                    JSValue::Int(r.start as i32),
                    JSValue::Int(r.end as i32),
                ])?
                .into()
            } else {
                JSValue::UNDEFINED
            };
            idxlst.push(idx.clone());

            if let Some(name) = name {
                if idxgroup.is_undefined() {
                    *idxgroup = ctx.create_base_object()?.into();
                }
                idxgroup.define_property_value_str(ctx, name, idx, prop_flags)?;
            }
        }
    }

    let arr = ctx.create_array_with_values(lst)?;
    arr.define_property_value(
        ctx,
        JS_ATOM_index,
        JSValue::Int(start_index as i32),
        prop_flags,
    )?;
    arr.define_property_value(ctx, JS_ATOM_groups, group, prop_flags)?;
    arr.define_property_value(ctx, JS_ATOM_input, text.into(), prop_flags)?;

    if let Some((idxlst, idxgrp)) = indices {
        let idxarr = ctx.create_array_with_values(idxlst)?;
        idxarr.define_property_value(ctx, JS_ATOM_groups, idxgrp, prop_flags)?;
        arr.define_property_value(ctx, JS_ATOM_indices, idxarr.into(), prop_flags)?;
    }

    Ok((arr.into(), last_index))
}
