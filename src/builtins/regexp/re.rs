// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, ops::Range, rc::Rc};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};
use lre::{Regex, RegexFlags};
use zerocopy::IntoBytes;

use crate::{
    JS_CLASS_REGEXP, JS_CLASS_REGEXP_STRING_ITERATOR,
    array::{Array, alloc_fast_array_slots},
    atom::Atom,
    class::ClassID,
    context::ScopedContext,
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, Object, ObjectImpl, ObjectPayload},
    object_cast,
    string::{CharCode, StringData, StringRef},
    value::JSValue,
};

use super::js_regexp_string_iterator_proto_funcs;

pub const RE_HEADER_FLAGS: usize = 0;
pub const RE_HEADER_CAPTURE_COUNT: usize = 1;
pub const RE_HEADER_STACK_SIZE: usize = 2;
pub const RE_HEADER_LEN: usize = 7;

#[derive(Debug)]
pub struct RegExpStringIterPayload {
    iterating_regexp: Object,
    iterated_string: StringRef,
    global: bool,
    unicode: bool,
    done: Cell<bool>,
}

impl GcTrace for RegExpStringIterPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.iterated_string.trace(tr);
        self.iterating_regexp.trace(tr);
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
    pub(super) regex: Rc<Regex>,
}

impl GcTrace for RegExpData {
    #[inline(always)]
    fn trace(&self, _: &mut GcTraceCtx) {}
}

impl RegExpData {
    #[inline(always)]
    pub(crate) fn create(
        ctx: &mut ScopedContext,
        pattern: StringData,
        flags: StringData,
    ) -> Result<Self, i32> {
        // Note: 1) can duplicate same flag; 2) 'u' and 'v' cannot be both set
        let mut opts = RegexFlags::empty();
        for (_, cc) in flags.chars() {
            if let CharCode::Char(c) = cc {
                match c {
                    'd' if !opts.has_indices() => {
                        opts = opts.indices();
                    }
                    'g' if !opts.has_global() => {
                        opts = opts.global();
                    }
                    'i' if !opts.has_ignore_case() => {
                        opts = opts.ignore_case();
                    }
                    'm' if !opts.has_multi_line() => {
                        opts = opts.multi_line();
                    }
                    's' if !opts.has_dotall() => {
                        opts = opts.dotall();
                    }
                    'u' if !opts.has_unicode() && !opts.has_unicode_sets() => {
                        // 'u' and 'v' cannot be both set
                        opts = opts.unicode();
                    }
                    'v' if !opts.has_unicode() && !opts.has_unicode_sets() => {
                        // 'u' and 'v' cannot be both set
                        opts = opts.unicode_sets();
                    }
                    'y' if !opts.has_sticky() => {
                        opts = opts.sticky();
                    }
                    _ => {
                        return Err(ctx.throw_syntax_error("Invalid regular expression flags"));
                    }
                }
            }
        }

        let regex = Regex::from_bytes(
            &pattern.encode_utf8_bytes_internal(0..pattern.len(), true, !opts.is_unicode()),
            opts,
        )
        .map_err(|e| ctx.throw_syntax_error(&e.to_string()))?;

        Ok(Self {
            regex: Rc::new(regex),
            pattern,
            flags_str: flags,
        })
    }

    #[inline(always)]
    pub fn flags(&self) -> RegexFlags {
        self.regex.flags()
    }

    pub fn find(&self, content: &StringData, from: usize) -> Box<[Range<usize>]> {
        if from > content.len() {
            return Box::new([]);
        }

        let is_wide = content.is_wide();
        let bytes = if is_wide {
            content.utf16().unwrap().as_bytes()
        } else {
            content.ascii().unwrap()
        };

        let is_sticky = self.flags().has_sticky();
        let mut cursor = 0;
        let mut last_range = usize::MAX..usize::MAX;
        let mut res = Vec::<Range<usize>>::with_capacity(32);

        for m in self
            .regex
            .find_iter(bytes, if is_wide { from << 1 } else { from }, is_wide)
            .take(if self.flags().has_global() {
                usize::MAX
            } else {
                1
            })
        {
            if is_sticky && m.start() != cursor {
                break;
            }

            cursor = m.end();

            let match_range = if is_wide {
                m.start() >> 1..m.end() >> 1
            } else {
                m.start()..m.end()
            };

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

        let is_wide = content.is_wide();
        let bytes = if is_wide {
            content.utf16().unwrap().as_bytes()
        } else {
            content.ascii().unwrap()
        };

        if let Some(caps) = self
            .regex
            .captures_at(bytes, if is_wide { from << 1 } else { from }, is_wide)
            .map_err(|e| {
                eprintln!("[O.o] regexp error {e}");
                -1
            })
            .unwrap()
        {
            let cnt = caps.len();
            let mut res = Vec::<(Option<Range<usize>>, Option<String>)>::with_capacity(cnt);

            for i in 0..cnt {
                if let Some(m) = caps.get(i) {
                    let range = if is_wide {
                        m.start() >> 1..m.end() >> 1
                    } else {
                        m.start()..m.end()
                    };
                    res.push((Some(range), None));
                } else {
                    res.push((None, None))
                }
            }

            // set group names. index #0 don't have capture name.
            if let Some(names) = self.regex.group_names() {
                for (i, name) in names.iter().enumerate() {
                    // Note: #0 is while match, captures starting from index #1
                    let i = i + 1;
                    debug_assert!(i < res.len());
                    res[i].1 = Some(name.into());
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
    pub(crate) fn from(
        ctx: &mut ScopedContext,
        ctor: JSValue,
        re: Box<RegExpData>,
    ) -> Result<Self, i32> {
        let mut o = Self(
            ctx.create_instance_with_ctor(ctor, ObjectPayload::REGEXP(re))
                .map_err(|_| -1)?
                .0,
        );
        o.define_property_value(
            ctx,
            Atom::CONST_lastIndex,
            JSValue::from(0),
            JS_PROP_WRITABLE,
        )?;
        Ok(o)
    }

    #[inline(always)]
    pub fn create(
        ctx: &mut ScopedContext,
        ctor: JSValue,
        pattern: StringData,
        flags: StringData,
    ) -> Result<Self, i32> {
        RegExpData::create(ctx, pattern, flags).and_then(|re| Self::from(ctx, ctor, Box::new(re)))
    }

    pub fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        debug_assert_eq!(class_id, JS_CLASS_REGEXP_STRING_ITERATOR);
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        let it_proto = ctx.iterator_proto()?;
        let proto = ctx.create_base_object_with_proto(Some(it_proto))?;
        ctx.init_class_proto_internal(class_id, proto, js_regexp_string_iterator_proto_funcs)
    }
}

/// build Array object to hold regexp matches.
pub(super) fn make_regexp_capture_array(
    ctx: &mut ScopedContext,
    text: StringRef,
    range_and_name: &[(Option<Range<usize>>, Option<String>)],
    with_indices: bool,
) -> Result<(Array, usize), i32> {
    let mut items = alloc_fast_array_slots(ctx, range_and_name.len())?;

    let mut indices: Option<(Vec<JSValue>, Option<Object>)> = if with_indices {
        let mut lst = Vec::new();
        lst.try_reserve(range_and_name.len()).map_err(|_| ctx.throw_out_of_memory())?;
        Some((lst, None))
    } else {
        None
    };

    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
    let mut groups = JSValue::UNDEFINED;

    let mut start_index = 0;
    let mut last_index = 0;

    for (i, (range, name)) in range_and_name.iter().enumerate() {
        let substr: JSValue = if let Some(span) = range.clone() {
            if i == 0 {
                start_index = span.start;
                last_index = span.end;
            }
            text.sub_string(ctx, span).map(JSValue::make_string)?
        } else {
            JSValue::UNDEFINED
        };
        items.push(substr);

        if let Some(name) = name {
            if groups.is_undefined() {
                groups = ctx.create_base_object_with_proto(None)?.into();
            }
            groups.define_property_value_str(ctx, name, substr, prop_flags)?;
        }

        if let Some((idxlst, idxgroup)) = &mut indices {
            let idx = if let Some(r) = range {
                Array::from_values(
                    ctx,
                    vec![JSValue::from(r.start as i32), JSValue::from(r.end as i32)],
                )?
                .into()
            } else {
                JSValue::UNDEFINED
            };
            idxlst.push(idx);

            if let Some(n) = name {
                if let Some(g) = idxgroup {
                    g.define_property_value_str(ctx, n, idx, prop_flags)?;
                } else {
                    let mut g = ctx.create_base_object_with_proto(None)?;
                    g.define_property_value_str(ctx, n, idx, prop_flags)?;
                    idxgroup.replace(g);
                }
            }
        }
    }

    let mut res = Array::from_values(ctx, items)?;

    res.define_property_value(
        ctx,
        Atom::CONST_index,
        JSValue::from(start_index as i32),
        prop_flags,
    )?;
    res.define_property_value(
        ctx,
        Atom::CONST_input,
        JSValue::make_string(text),
        prop_flags,
    )?;
    res.define_property_value(ctx, Atom::CONST_groups, groups, prop_flags)?;

    if let Some((idx_lst, idx_grp)) = indices {
        let mut indices = Array::from_values(ctx, idx_lst)?;
        indices.define_property_value(
            ctx,
            Atom::CONST_groups,
            idx_grp.map_or(JSValue::UNDEFINED, JSValue::from),
            prop_flags,
        )?;
        res.define_property_value(ctx, Atom::CONST_indices, indices.into(), prop_flags)?;
    }

    Ok((res, last_index))
}
