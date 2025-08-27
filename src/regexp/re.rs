use std::{ops::Range, rc::Rc};

use regex::bytes::{Regex, RegexBuilder};

use crate::{
    atom::consts::*,
    context::JSContext,
    error::ErrorKind,
    object::{JS_PROP_WRITABLE, JSObject, ObjectPayload},
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
pub struct JSRegExpStringIteratorData {
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
    pub(crate) fn new_re(
        ctx: &JSContext,
        pattern: &str,
        flags_str: impl Iterator<Item = char>,
    ) -> Result<(Regex, u8), i32> {
        let mut flags = 0u8;

        for c in flags_str {
            match c {
                'g' => {
                    flags |= LRE_FLAG_GLOBAL as u8;
                }
                'i' => {
                    flags |= LRE_FLAG_IGNORECASE as u8;
                }
                'm' => {
                    flags |= LRE_FLAG_MULTILINE as u8;
                }
                'u' | 'v' => {
                    flags |= LRE_FLAG_UTF16 as u8;
                }
                's' => {
                    flags |= LRE_FLAG_DOTALL as u8;
                }
                'y' => {
                    flags |= LRE_FLAG_STICKY as u8;
                }
                _ => {}
            }
        }

        let re = RegexBuilder::new(match pattern {
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
        })
        .unicode(flags & LRE_FLAG_UTF16 as u8 != 0)
        .multi_line(flags & LRE_FLAG_MULTILINE as u8 != 0)
        .case_insensitive(flags & LRE_FLAG_IGNORECASE as u8 != 0)
        .build()
        .map_err(|e| {
            ctx.throw_error(ErrorKind::Syntax, &e.to_string());
            -1
        })?;

        Ok((re, flags))
    }

    pub fn new(
        ctx: &JSContext,
        pattern: StringContent,
        flags_str: StringContent,
    ) -> Result<Self, i32> {
        let (re, flags) = Self::new_re(ctx, &pattern.to_utf8(), flags_str.chars_lossy())?;
        let regex = Rc::new(re);

        Ok(Self {
            pattern,
            flags_str,
            regex,
            flags,
        })
    }

    pub fn find(&self, text: &JSString, from: usize) -> Box<[Range<usize>]> {
        let sbuf = text.content();
        if from > sbuf.buf_len() {
            return Box::new([]);
        }

        let sbuf_slice = sbuf.slice(from..sbuf.buf_len());
        let utf8_text = sbuf_slice.to_utf8();

        let is_global = self.flags & LRE_FLAG_GLOBAL as u8 != 0;
        let is_unicode = self.flags & LRE_FLAG_UTF16 as u8 != 0;
        let is_sticky = self.flags & LRE_FLAG_STICKY as u8 != 0;

        let mut cursor = 0;
        let mut last_range = usize::MAX..usize::MAX;
        let mut res = Vec::<Range<usize>>::with_capacity(32);

        for m in self
            .regex
            .find_iter(utf8_text.as_bytes())
            .take(if is_global { usize::MAX } else { 1 })
        {
            if is_sticky && m.start() != cursor {
                break;
            }

            cursor = m.end();

            let range = if sbuf_slice.is_wide() {
                use crate::string::map_utf8_to_utf16_range;

                let s16 = {
                    let bb = sbuf_slice.as_bytes();
                    unsafe { std::slice::from_raw_parts(bb.as_ptr().cast::<u16>(), bb.len() / 2) }
                };
                map_utf8_to_utf16_range(utf8_text.as_bytes(), s16, m.range())
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
        text: &JSString,
        from: usize,
    ) -> Option<Box<[(Option<Range<usize>>, Option<&str>)]>> {
        let sbuf = text.content();
        if from > sbuf.buf_len() {
            return None;
        }

        let sbuf_slice = sbuf.slice(from..sbuf.buf_len());
        let utf8_text = sbuf_slice.to_utf8();

        if let Some(caps) = self.regex.captures(utf8_text.as_bytes()) {
            if self.flags & LRE_FLAG_STICKY as u8 != 0 && caps.get(0).unwrap().start() != 0 {
                return None;
            }

            let mut res = Vec::<(Option<Range<usize>>, Option<&str>)>::new();

            for (cap, name) in caps.iter().zip(self.regex.capture_names()) {
                if let Some(m) = cap {
                    let range = if sbuf_slice.is_wide() {
                        let s16 = unsafe {
                            let bb = sbuf_slice.as_bytes();
                            std::slice::from_raw_parts(bb.as_ptr().cast::<u16>(), bb.len() / 2)
                        };
                        map_utf8_to_utf16_range(utf8_text.as_bytes(), s16, m.range())
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
