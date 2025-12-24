use std::{borrow::Cow, cell::Cell, ops::Range, rc::Rc};

use gc_lite::{GcRef, GcTracable};
use pcre2::bytes::{Regex, RegexBuilder};

use crate::{
    JS_CLASS_REGEXP, JS_CLASS_REGEXP_STRING_ITERATOR,
    atom::Atom,
    class::ClassID,
    context::{Context, ContextImpl},
    object::{
        JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, ObjectImpl, ObjectPayload, ObjectRef,
    },
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
    iterating_regexp: ObjectRef,
    iterated_string: StringRef,
    pub global: bool,
    pub unicode: bool,
    done: Cell<bool>,
}

unsafe impl GcTracable for RegExpStringIterPayload {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.iterated_string.trace(tracer);
        self.iterating_regexp.trace(tracer);
    }
}

impl RegExpStringIterPayload {
    pub fn new(regexp: ObjectRef, str: StringRef, global: bool, unicode: bool) -> Box<Self> {
        Box::new(Self {
            iterating_regexp: regexp,
            iterated_string: str,
            global,
            unicode,
            done: Cell::new(false),
        })
    }

    #[inline(always)]
    pub const fn regexp(&self) -> ObjectRef {
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

unsafe impl GcTracable for RegExpData {
    #[inline(always)]
    fn trace(&self, _: &mut gc_lite::GcTracer) {}
}

impl RegExpData {
    pub(crate) fn compile_re(
        ctx: &ContextImpl,
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

        // in some cases, regexp is supposed to enable unicode mode by default:
        // 1. pattern contains non-ascii characters
        // 2. pattern has \xFF or \uFFFF pattern char.
        if !pattern.is_ascii() {
            ff |= LRE_FLAG_UTF16 as u8;
        }

        //
        // fix behaviour difference between ECMAScript and PCRE2 regexp.
        //
        if pattern.eq("\\") {
            ctx.throw_syntax_error("Invalid RegExp pattern");
            return Err(-1);
        }
        if pattern.starts_with('^') || pattern.ends_with('$') {
            ff |= LRE_FLAG_MULTILINE as u8;
        }

        let mut is_unicode = ff & LRE_FLAG_UTF16 as u8 != 0;

        let pat: Cow<'_, str> = if pattern
            .as_bytes()
            .iter()
            .any(|x| matches!(*x, b'\\' | b'['))
        {
            let mut buf = String::with_capacity(pattern.len() * 3 / 2);
            let mut sio = Utf8Stream::new(pattern.as_bytes());

            while let Some((_, cc)) = sio.next() {
                match cc {
                    CharCode::Char('\\') => {
                        if let Some(b) = sio.current_byte() {
                            match b {
                                b'd' => {
                                    // \d -> [0-9]
                                    buf.push_str("[0-9]");
                                    sio.move_cursor_by(1);
                                }
                                b'w' => {
                                    // \w -> [0-9A-Za-z_]
                                    buf.push_str("[0-9A-Za-z_]");
                                    sio.move_cursor_by(1);
                                }
                                b's' => {
                                    // \s -> [ \t\n\r]
                                    buf.push_str(r#"[ \t\n\r]"#);
                                    sio.move_cursor_by(1);
                                }
                                b'S' => {
                                    // \S -> ...
                                    ff |= LRE_FLAG_UTF16 as u8;
                                    is_unicode = true;
                                    buf.push_str(r#"[\S&&[^\xFEFF]]"#);
                                    sio.move_cursor_by(1);
                                }
                                _ => {
                                    buf.push('\\');
                                }
                            }
                        }
                    }
                    CharCode::Char('[') => {
                        let mut bb = [0u8; 2];
                        sio.peek_bytes(&mut bb);
                        match (bb[0], bb[1]) {
                            (b']', _) => {
                                // [] -> (?!)
                                buf.push_str("(?!)");
                                sio.move_cursor_by(1);
                            }
                            (b'^', b']') => {
                                // [^] -> [\s\S]
                                buf.push_str("[\\s\\S]");
                                sio.move_cursor_by(2);
                            }
                            _ => {
                                buf.push('[');
                            }
                        }
                    }
                    CharCode::Char(c) => {
                        buf.push(c);
                    }
                    CharCode::Code(_) => {
                        ctx.throw_syntax_error("invalid UTF-8 sequence");
                        return Err(-1);
                    }
                }
            }

            // let mut expect_hex: Option<i32> = None;
            // let mut it = pattern.chars().peekable();
            // while let Some(c) = it.next() {
            //     match c {
            //         '\\' => {
            //             if let Some(c1) = it.peek() {
            //                 match *c1 {
            //                     'd' if is_unicode => {
            //                         // \d -> [0-9]
            //                         pat2.push_str("[0-9]");
            //                         it.next();
            //                     }
            //                     'w' => {
            //                         // \w -> [0-9A-Za-z_]
            //                         pat2.push_str("[0-9A-Za-z_]");
            //                         it.next();
            //                     }
            //                     's' => {
            //                         // \s -> [ \t\n\r]
            //                         pat2.push_str("[ \t\n\r]");
            //                         it.next();
            //                     }
            //                     // 'x' if !is_unicode => {
            //                     //     // \xFF
            //                     //     expect_hex = Some(2);
            //                     //     it.next();
            //                     //     pat2.push('x');
            //                     // }
            //                     // 'u' if !is_unicode => {
            //                     //     // \uFFFF
            //                     //     expect_hex = Some(4);
            //                     //     it.next();
            //                     //     pat2.push('u');
            //                     // }
            //                     // '>' | '<' => {
            //                     //     // replace `\>` with `>`, `\\<` with `<`
            //                     //     pat2.pop(); // chop `\`
            //                     // }
            //                     _ => {
            //                         pat2.push('\\');
            //                     }
            //                 }
            //             }
            //         }
            //         '0'..='9' | 'A'..='F' | 'a'..='f' if !is_unicode => {
            //             if let Some(cnt) = &mut expect_hex {
            //                 *cnt -= 1;
            //                 if *cnt == 0 {
            //                     ff |= LRE_FLAG_UTF16 as u8;
            //                     is_unicode = true;
            //                     expect_hex = None;
            //                 }
            //             }
            //         }
            //         _ => {
            //             pat2.push(c);
            //         }
            //     }
            // }

            buf.into()
        } else {
            pattern.into()
        };

        let re = RegexBuilder::new()
            .utf(is_unicode)
            .ucp(ff & LRE_FLAG_UTF16 as u8 != 0)
            .multi_line(ff & LRE_FLAG_MULTILINE as u8 != 0)
            .caseless(ff & LRE_FLAG_IGNORECASE as u8 != 0)
            .dotall(ff & LRE_FLAG_DOTALL as u8 != 0)
            .crlf(true)
            .build(pat.as_ref())
            .map_err(|e| {
                ctx.throw_syntax_error(&e.to_string());
                -1
            })?;

        Ok((re, ff))
    }

    pub(crate) fn create(
        ctx: &ContextImpl,
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

        for m in self
            .regex
            .find_iter(utf8_str.as_bytes())
            .take(if is_global { usize::MAX } else { 1 })
        {
            let m = m
                .map_err(|e| {
                    eprintln!("[O.o] RegExp error: {e}");
                    -1
                })
                .unwrap();

            if is_sticky && m.start() != cursor {
                break;
            }

            cursor = m.end();

            let range = if let Some(w) = content.utf16() {
                let s16 = &w[from..];
                map_utf8_to_utf16_range(utf8_str.as_bytes(), s16, m.start()..m.end())
            } else {
                m.start()..m.end()
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
    ) -> Option<Box<[(Option<Range<usize>>, Option<&str>)]>> {
        if from > content.len() {
            return None;
        }

        let utf8_str = content.to_utf8_range(from..content.len(), false);

        if let Some(caps) = self
            .regex
            .captures(utf8_str.as_bytes())
            .map_err(|e| {
                eprintln!("[O.o] RegExp error: {e}");
                -1
            })
            .unwrap()
        {
            if self.flags & LRE_FLAG_STICKY as u8 != 0 && caps.get(0).unwrap().start() != 0 {
                return None;
            }

            let mut res = Vec::<(Option<Range<usize>>, Option<&str>)>::new();
            let names = self.regex.capture_names();

            for i in 0..caps.len() {
                let name = if let Some(n) = names.get(i) {
                    n.as_deref()
                } else {
                    None
                };

                if let Some(m) = caps.get(i) {
                    let range = if let Some(w) = content.utf16() {
                        let s16 = &w[from..];
                        map_utf8_to_utf16_range(utf8_str.as_bytes(), s16, m.start()..m.end())
                    } else {
                        m.start()..m.end()
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

/// the regexp object
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct RegExp(GcRef<ObjectImpl>);

object_cast!(RegExp, JS_CLASS_REGEXP);

impl RegExp {
    /// create RegExp object with given RegExpData, and constructor.
    pub(crate) fn from(ctx: &ContextImpl, ctor: JSValue, re: Box<RegExpData>) -> Result<Self, i32> {
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
        ctx: &ContextImpl,
        ctor: JSValue,
        pattern: StringData,
        flags: StringData,
    ) -> Result<Self, i32> {
        RegExpData::create(ctx, pattern, flags).and_then(|re| Self::from(ctx, ctor, Box::new(re)))
    }

    pub fn class_initialize(mut ctx: Context, class_id: ClassID) -> Result<Option<ObjectRef>, i32> {
        match class_id {
            JS_CLASS_REGEXP => {
                let mut obj = ctx.create_base_object()?;
                obj.install_field_defs(&ctx, &js_regexp_proto_funcs)
                    .unwrap();

                let mut ctor = ctx.create_global_constructor_or_func(
                    "RegExp",
                    js_regexp_constructor,
                    2,
                    obj.into(),
                    0,
                )?;
                ctor.install_field_defs(&ctx, &js_regexp_funcs).unwrap();

                ctx.regexp_ctor = ctor;
                Ok(Some(obj))
            }
            JS_CLASS_REGEXP_STRING_ITERATOR => {
                let iter_proto = ctx.iterator_proto()?;
                let mut iter = ctx.create_base_object_with_proto(Some(iter_proto))?;
                iter.install_field_defs(&ctx, &js_regexp_string_iterator_proto_funcs)?;

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
    ctx: &ContextImpl,
    text: StringRef,
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

        lst.push(val);

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
            idxlst.push(idx);

            if let Some(name) = name {
                if idxgroup.is_undefined() {
                    *idxgroup = ctx.create_base_object()?.into();
                }
                idxgroup.define_property_value_str(ctx, name, idx, prop_flags)?;
            }
        }
    }

    let mut arr = ctx.create_array_with_values(lst)?;
    arr.define_property_value(
        ctx,
        Atom::CONST_index,
        JSValue::Int(start_index as i32),
        prop_flags,
    )?;
    arr.define_property_value(ctx, Atom::CONST_groups, group, prop_flags)?;
    arr.define_property_value(ctx, Atom::CONST_input, text.into(), prop_flags)?;

    if let Some((idxlst, idxgrp)) = indices {
        let mut idxarr = ctx.create_array_with_values(idxlst)?;
        idxarr.define_property_value(ctx, Atom::CONST_groups, idxgrp, prop_flags)?;
        arr.define_property_value(ctx, Atom::CONST_indices, idxarr.into(), prop_flags)?;
    }

    Ok((arr.into(), last_index))
}
