use std::rc::Rc;

use regex::Regex;
use smallvec::SmallVec;

use crate::{
    atom::consts::*,
    context::JSContext,
    error::ErrorKind,
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JSObject, ObjectDetail},
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
    Capture,
    Match,
    Test,
}

#[derive(Debug, Clone)]
pub struct JSRegExp {
    pub(super) pattern: StringContent,
    pub(super) flags_str: StringContent,
    pub(super) flags: u8,
    pub(super) regex: Rc<Regex>,
}

impl JSRegExp {
    pub fn new(ctx: &JSContext, pattern: &JSString, flags_str: &JSString) -> Result<Self, i32> {
        let mut flags = 0u8;
        for c in flags_str.content().chars_lossy() {
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
                'u' => {
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

        let regex = Rc::new(Regex::new(&pattern.content().to_utf8()).map_err(|e| {
            ctx.throw_error(ErrorKind::Syntax, &e.to_string());
            -1
        })?);

        Ok(Self {
            pattern: pattern.content().clone(),
            flags_str: flags_str.content().clone(),
            regex,
            flags,
        })
    }

    pub fn exec(
        &self,
        ctx: &JSContext,
        str: &JSString,
        start: usize,
        mode: RegExpExecMode,
    ) -> Result<Option<(JSValue, usize)>, i32> {
        let is_global = self.flags & LRE_FLAG_GLOBAL as u8 != 0;
        let is_sticky = self.flags & LRE_FLAG_STICKY as u8 != 0;

        let sbuf = str.content();
        let sbuf_slice = sbuf.slice(start..sbuf.buf_len());
        let utf8_text = sbuf_slice.to_utf8();

        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

        // iterator to map utf8 index to str buf index
        let mut chars_iter = if sbuf.is_wide() {
            Some(sbuf.iter_since(start).zip(utf8_text.chars()))
        } else {
            None
        };

        let ret = match (mode, is_global) {
            (RegExpExecMode::Capture, _) | (RegExpExecMode::Match, false) => {
                if let Some(caps) = self.regex.captures(&utf8_text)
                    && (!is_sticky || caps.get(0).unwrap().start() == start)
                {
                    let mut names = SmallVec::<[String; 10]>::new();
                    for n in self.regex.capture_names().filter_map(|n| n) {
                        names.push(n.to_string());
                    }

                    let mut index = 0;
                    let mut full_text: JSString = JS_ATOM_empty_string.to_string(ctx).unwrap();
                    let arr = ctx.create_array()?;

                    for (i, cap) in caps.iter().enumerate() {
                        arr.define_property_value_u32(
                            ctx,
                            i as _,
                            if let Some(m) = cap {
                                let s = ctx.create_string_from_str(m.as_str())?;
                                if i == 0 {
                                    full_text = s.clone();
                                    index = m.start();
                                }
                                s.into()
                            } else {
                                JSValue::UNDEFINED
                            },
                            prop_flags,
                        )?;
                    }

                    // groups
                    let groups = if !names.is_empty() {
                        let gg = ctx.create_base_object()?;
                        for name in names {
                            if let Some(m) = caps.name(&name) {
                                gg.define_property_value_str(
                                    ctx,
                                    &name,
                                    ctx.create_string_from_str(m.as_str())?,
                                    prop_flags,
                                )?;
                            }
                        }
                        gg.into()
                    } else {
                        JSValue::UNDEFINED
                    };
                    arr.define_property_value_str(ctx, "groups", groups, prop_flags)?;

                    // Note: convert utf8 index to string buf index
                    let start_index = if index == 0 {
                        start
                    } else if let Some(mut it) = chars_iter.clone() {
                        let char_count = utf8_text[..index].chars().count();
                        let ((i, _), _) = it.nth(char_count).unwrap();
                        i + start
                    } else {
                        index
                    };
                    let last_index = start_index + full_text.buf_len();

                    arr.define_property_value_str(
                        ctx,
                        "index",
                        JSValue::Int(start_index as _),
                        prop_flags,
                    )?;

                    arr.define_property_value_str(ctx, "input", str.clone().into(), prop_flags)?;

                    Some((arr, last_index))
                } else {
                    None
                }
            }
            (RegExpExecMode::Match, true) => {
                // global match returns array match #0
                let mut arr: Option<JSValue> = None;
                let mut cursor = 0;
                let mut n = 0;
                while let Some(m) = self.regex.find_at(&utf8_text, cursor)
                    && (!is_sticky || m.start() == cursor)
                {
                    if arr.is_none() {
                        let a = ctx.create_array()?;
                        arr.replace(a);
                    }

                    println!("[O.o] match global {n}. cursor={cursor}, m={m:?}");

                    arr.as_ref().unwrap().define_property_value_u32(
                        ctx,
                        n,
                        ctx.create_string_from_str(m.as_str())?.into(),
                        prop_flags,
                    )?;

                    n += 1;
                    cursor = m.end();
                }

                if let Some(arr) = arr {
                    let last_index = if cursor == utf8_text.len() {
                        sbuf.buf_len()
                    } else if let Some(it) = &mut chars_iter {
                        let char_count = utf8_text[..cursor].chars().count();
                        let ((i, _), _) = it.nth(char_count).unwrap();
                        i + start
                    } else {
                        cursor + start
                    };
                    Some((arr, last_index))
                } else {
                    None
                }
            }
            (RegExpExecMode::Test, _) => {
                todo!()
            }
        };

        Ok(ret)
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
                ObjectDetail::REGEXP(Box::new(JSRegExp::new(self, pattern, flags)?)),
            )
            .map_err(|_| -1)?;

        obj.define_property_value(self, JS_ATOM_lastIndex, 0, JS_PROP_WRITABLE)?;
        Ok(obj)
    }
}
