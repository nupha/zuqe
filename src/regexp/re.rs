#[cfg(debug_assertions)]
use std::ops::Range;
use std::rc::Rc;

use regex::bytes::{Regex, RegexBuilder};

use crate::{
    atom::consts::*,
    context::JSContext,
    error::ErrorKind,
    object::{JS_PROP_WRITABLE, JSObject, ObjectDetail},
    string::{JSString, StringContent},
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
    pub(crate) regex: Rc<Regex>,
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

        let re = RegexBuilder::new(pattern)
            .unicode(flags & LRE_FLAG_UTF16 as u8 != 0)
            .multi_line(flags & LRE_FLAG_MULTILINE as u8 != 0)
            .case_insensitive(flags & LRE_FLAG_IGNORECASE as u8 == 0)
            .build()
            .map_err(|e| {
                ctx.throw_error(ErrorKind::Syntax, &e.to_string());
                -1
            })?;

        Ok((re, flags))
    }

    pub fn new(ctx: &JSContext, pattern: &JSString, flags_str: &JSString) -> Result<Self, i32> {
        let (re, flags) = Self::new_re(
            ctx,
            &pattern.content().to_utf8(),
            flags_str.content().chars_lossy(),
        )?;
        let regex = Rc::new(re);

        Ok(Self {
            pattern: pattern.content().clone(),
            flags_str: flags_str.content().clone(),
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
        let mut last_range = 0..0;
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
                // println!(
                //     "[O.o] match: {m:?}, sbuf_range={start_index}..{end_index}, sbuf_content={:?}",
                //     text.content()
                //         .iter_range(start_index..end_index)
                //         .map(|(_, c)| c)
                //         .collect::<Vec<_>>()
                // );
                last_range = match_range;
            }
        }

        res.into()
    }

    pub fn capture(
        &self,
        text: &JSString,
        from: usize,
    ) -> Option<Box<[(Range<usize>, Option<&str>)]>> {
        let sbuf = text.content();
        if from > sbuf.buf_len() {
            return None;
        }

        let sbuf_slice = sbuf.slice(from..sbuf.buf_len());
        let utf8_text = sbuf_slice.to_utf8();

        if let Some(caps) = self.regex.captures(utf8_text.as_bytes()) {
            let mut res = Vec::<(Range<usize>, Option<&str>)>::new();

            for (cap, name) in caps.iter().zip(self.regex.capture_names()) {
                if let Some(m) = cap {
                    let range = if sbuf_slice.is_wide() {
                        use crate::string::map_utf8_to_utf16_range;

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
                    res.push(((start_index..end_index), name));
                }
            }

            Some(res.into())
        } else {
            None
        }
    }

    // #[deprecated]
    // pub fn exec(
    //     &self,
    //     ctx: &JSContext,
    //     text: &JSString,
    //     from: usize,
    //     mode: RegExpExecMode,
    // ) -> Result<Option<(JSValue, usize)>, i32> {
    //     let is_global = self.flags & LRE_FLAG_GLOBAL as u8 != 0;
    //     let is_sticky = self.flags & LRE_FLAG_STICKY as u8 != 0;

    //     let sbuf = text.content();
    //     if from > sbuf.buf_len() {
    //         return Ok(None);
    //     }

    //     let sbuf_slice = sbuf.slice(from..sbuf.buf_len());
    //     let utf8_text = sbuf_slice.to_utf8();

    //     let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

    //     // iterator to map utf8 index to str buf index
    //     let mut chars_iter = if sbuf.is_wide() {
    //         Some(sbuf.iter_since(from))
    //     } else {
    //         None
    //     };

    //     let ret = match (mode, is_global) {
    //         (RegExpExecMode::CaptureOnce, _) | (RegExpExecMode::Match, false) => {
    //             if let Some(caps) = self.regex.captures(utf8_text.as_bytes())
    //                 && (!is_sticky || caps.get(0).unwrap().start() == from)
    //             {
    //                 let mut names = SmallVec::<[&str; 8]>::new();
    //                 for n in self.regex.capture_names().filter_map(|n| n) {
    //                     names.push(n);
    //                 }

    //                 let mut range = 0..0;
    //                 let mut matched: StringContent = Rc::new(StringBuf::Empty);

    //                 let arr = ctx.create_array()?;

    //                 for (i, cap) in caps.iter().enumerate() {
    //                     arr.define_property_value_u32(
    //                         ctx,
    //                         i as _,
    //                         if let Some(m) = cap {
    //                             let s = Rc::new(StringBuf::from_str(
    //                                 std::str::from_utf8(m.as_bytes()).unwrap(),
    //                             )?);
    //                             if i == 0 {
    //                                 matched = s.clone();
    //                                 range = m.start()..m.end();

    //                                 #[cfg(debug_assertions)]
    //                                 debug_assert_eq!(
    //                                     matched.chars_count(),
    //                                     utf8_text[range.clone()].chars().count()
    //                                 );
    //                             }
    //                             ctx.create_string_with_content(s)?.into()
    //                         } else {
    //                             JSValue::UNDEFINED
    //                         },
    //                         prop_flags,
    //                     )?;
    //                 }

    //                 // groups
    //                 let groups = if !names.is_empty() {
    //                     let gg = ctx.create_base_object()?;
    //                     for name in names {
    //                         if let Some(m) = caps.name(&name) {
    //                             gg.define_property_value_str(
    //                                 ctx,
    //                                 &name,
    //                                 ctx.create_string_from_str(
    //                                     std::str::from_utf8(m.as_bytes()).unwrap(),
    //                                 )?,
    //                                 prop_flags,
    //                             )?;
    //                         }
    //                     }
    //                     gg.into()
    //                 } else {
    //                     JSValue::UNDEFINED
    //                 };
    //                 arr.define_property_value_str(ctx, "groups", groups, prop_flags)?;

    //                 // Note: convert utf8 start index to string buf index
    //                 let start_index = if range.start == 0 {
    //                     range.start
    //                 } else if let Some(it) = chars_iter.as_mut() {
    //                     let nc = utf8_text[0..range.start].chars().count();
    //                     debug_assert!(nc > 0);
    //                     let (i, _) = it.nth(nc - 1).unwrap();
    //                     i
    //                 } else {
    //                     range.start
    //                 } + from;

    //                 let last_index = if range.start == range.end {
    //                     range.start
    //                 } else if range.end == utf8_text.len() {
    //                     text.buf_len()
    //                 } else if let Some(it) = chars_iter.as_mut() {
    //                     let nc = matched.chars_count();
    //                     debug_assert!(nc > 0);
    //                     let (i, _) = it.nth(nc - 1).unwrap();
    //                     i
    //                 } else {
    //                     range.end
    //                 } + from;

    //                 arr.define_property_value_str(
    //                     ctx,
    //                     "index",
    //                     JSValue::Int(start_index as _),
    //                     prop_flags,
    //                 )?;

    //                 arr.define_property_value_str(ctx, "input", text.clone().into(), prop_flags)?;

    //                 Some((arr, last_index))
    //             } else {
    //                 None
    //             }
    //         }

    //         (RegExpExecMode::Match, true) => {
    //             // global match returns array match #0
    //             let mut arr: Option<JSValue> = None;
    //             let mut cursor = 0;
    //             let mut n = 0;

    //             let mut find = self.regex.find_iter(utf8_text.as_bytes());
    //             while let Some(m) = find.next()
    //                 && (!is_sticky || m.start() == cursor)
    //             {
    //                 if arr.is_none() {
    //                     let a = ctx.create_array()?;
    //                     arr.replace(a);
    //                 }

    //                 arr.as_ref().unwrap().define_property_value_u32(
    //                     ctx,
    //                     n,
    //                     ctx.create_string_from_str(std::str::from_utf8(m.as_bytes()).unwrap())?
    //                         .into(),
    //                     prop_flags,
    //                 )?;

    //                 n += 1;
    //                 cursor = m.end();
    //             }

    //             if let Some(arr) = arr {
    //                 let last_index = if cursor == utf8_text.len() {
    //                     text.buf_len()
    //                 } else if let Some(it) = &mut chars_iter {
    //                     let nc = utf8_text[..cursor].chars().count();
    //                     let (i, _) = it.nth(nc).unwrap();
    //                     i + from
    //                 } else {
    //                     cursor + from
    //                 };
    //                 Some((arr, last_index))
    //             } else {
    //                 None
    //             }
    //         }
    //     };

    //     Ok(ret)
    // }
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
                ObjectDetail::REGEXP(Box::new(JSRegExp::new(self, pattern, flags)?)),
            )
            .map_err(|_| -1)?;

        obj.define_property_value(self, JS_ATOM_lastIndex, 0, JS_PROP_WRITABLE)?;
        Ok(obj)
    }
}
