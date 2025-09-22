use std::{ops::Range, rc::Rc};

use regex::bytes::{Regex, RegexBuilder};

use crate::{
    atom::consts::*,
    context::JSContext,
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JSObject, ObjectPayload},
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
pub struct JSRegExp {
    pub(crate) pattern: StringContent,
    pub(crate) flags_str: StringContent,
    pub(crate) flags: u8,
    pub(super) regex: Rc<Regex>,
}

impl JSRegExp {
    pub(crate) fn build_re(
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
        let pat = match pattern {
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
        };

        let re = RegexBuilder::new(pat)
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

    pub fn new(ctx: &JSContext, pattern: StringContent, flags: StringContent) -> Result<Self, i32> {
        let (re, ff) = Self::build_re(ctx, &pattern.to_utf8(), flags.chars_lossy())?;
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

        let utf8_str = content.to_utf8_range(from..content.len());

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

        let utf8_str = content.to_utf8_range(from..content.len());

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

pub const fn from_hex(c: u8) -> Result<u8, i32> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(-1),
    }
}

impl JSContext {
    /// create RegExp object
    pub fn create_regexp(
        &self,
        ctor: &JSValue,
        pattern: &JSString,
        flags: &JSString,
    ) -> Result<JSObject, i32> {
        let obj = self
            .create_object_apply_ctor(
                ctor,
                ObjectPayload::REGEXP(Box::new(JSRegExp::new(
                    self,
                    pattern.content().clone(),
                    flags.content().clone(),
                )?)),
            )
            .map_err(|_| -1)?;

        obj.define_property_value(self, JS_ATOM_lastIndex, JSValue::Int(0), JS_PROP_WRITABLE)?;
        Ok(obj)
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
