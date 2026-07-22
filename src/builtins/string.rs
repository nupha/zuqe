// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#[cfg(feature = "all_unicode")]
use unicode_normalization::UnicodeNormalization;

use crate::{
    Array, ClassDef, JS_CLASS_STRING_ITERATOR, ScopedContext,
    atom::Atom,
    builtins::iterator::{JSIteratorKindEnum, js_create_array_iterator},
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_THROW, ObjectPayload},
    string::{
        CharCode, CharCodeDecoder, JS_ConcatString3, JS_STRING_LEN_MAX, StringBuf, StringData,
        StringRef, Utf16Stream, js_string_exotic_methods,
    },
    value::JSValue,
};

const MAGIC_INCLUDES: i32 = 1;
const MAGIC_STARTS_WITH: i32 = 2;
const MAGIC_ENDS_WITH: i32 = 3;

pub fn js_string_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let str_val = if cc.length() == 0 {
        Atom::CONST_empty_string.force_string(ctx)?
    } else {
        #[cfg(feature = "builtin_symbol")]
        if let JSValue::Symbol(s) = cc.arg(0)
            && new_target.is_undefined()
        {
            // "Symbol(desc)"
            JS_ConcatString3(ctx, c"Symbol(", JSValue::make_string(s), c")")?
        } else {
            cc.arg(0).to_string(ctx)?
        }

        #[cfg(not(feature = "builtin_symbol"))]
        cc.arg(0).to_string(ctx)?
    };

    if !new_target.is_undefined() {
        // make String object using `new_target` as prototype.
        str_val.objectify(ctx, new_target).map(JSValue::from)
    } else {
        Ok(JSValue::make_string(str_val))
    }
}

/// impl for `charAt()`, `at()`, `chatCodeAt()`
fn js_string_at(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let str = cc.this().to_string_non_undefined_or_null(ctx)?;
    let what = cc.magic(); // 0: charAt, 1: at, 2: charCodeAt
    let buf_len = str.len() as i32;

    let mut buf_idx: i32 = cc.arg(0).to_number(ctx)?.to_i32_saturating();
    if buf_idx < 0 && what == 1 {
        // at() allows minus index
        buf_idx += buf_len;
    }

    if buf_idx < 0 || buf_idx >= buf_len {
        match what {
            0 => Atom::CONST_empty_string
                .force_string(ctx)
                .map(JSValue::make_string),
            1 => Ok(JSValue::UNDEFINED),
            2 => Ok(JSValue::NAN),
            _ => unreachable!(),
        }
    } else {
        let code = str.get(buf_idx as usize).unwrap();
        if what == 2 {
            // charCodeAt() returns int
            Ok(JSValue::Int(code as i32))
        } else {
            // charAt(), at() returns string
            ctx.create_string_from_char(code.into())
                .map(JSValue::make_string)
        }
    }
}

fn js_string_codePointAt(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let s = this.to_string_non_undefined_or_null(ctx)?;
    let i: i32 = cc.arg(0).to_number(ctx)?.to_i32_saturating();

    if (0..s.len() as i32).contains(&i) {
        let (_, c) = s.content().chars_since(i as usize).next().unwrap();
        Ok(JSValue::from(c.to_u32()))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

fn js_string_concat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // `to_string_non_undefined_or_null` preserves a rope (it no longer flattens
    // by default), so repeated `s = s.concat(x)` accumulation stays O(1)
    // amortized via the `concat2` rope path + consolidation, matching `+`.
    let mut s = cc.this().to_string_non_undefined_or_null(ctx)?;
    for a in cc.arguments() {
        s = s.concat(ctx, *a)?;
    }
    Ok(JSValue::make_string(s))
}

fn js_string_isWellFormed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let p = this.to_string_non_undefined_or_null(ctx)?;
    Ok(p.chars().is_well_formed().into())
}

fn js_string_toWellFormed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;

    if let Some(s16) = p.content().utf16() {
        let stream = Utf16Stream::new(s16);
        if !stream.is_well_formed() {
            let mut buf = Vec::<u16>::new();
            buf.try_reserve_exact(s16.len() * 3 / 2).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            for (_, cc) in stream {
                // invalid code point is replaced with U+FFFD
                buf.extend_from_slice(cc.char().unwrap_or('\u{FFFD}').encode_utf16(&mut [0; 2]));
            }

            return ctx
                .create_string_with_buf(buf.into())
                .map(JSValue::make_string);
        }
    }

    Ok(JSValue::make_string(p))
}

pub fn js_string_indexOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let lastIndexOf = cc.magic();

    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let p1 = cc.arg(0).to_string(ctx)?;

    let len = p.len() as i32;
    let v_len = p1.len() as i32;

    let start: i32;
    let stop: i32;
    let inc: i32;

    if lastIndexOf != 0 {
        let mut pos = len - v_len;
        if cc.length() > 1 {
            let d: f64 = cc.arg(1).to_number(ctx)?.to_f64();
            if !d.is_nan() {
                if d <= 0.0 {
                    pos = 0;
                } else if d < pos as f64 {
                    pos = d as i32;
                }
            }
        }
        start = pos;
        stop = 0;
        inc = -1;
    } else {
        start = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i32_clamp(0, len, 0)
        } else {
            0
        };
        stop = len - v_len;
        inc = 1;
    }

    let mut ret = -1;
    if len >= v_len && inc * (stop - start) >= 0 {
        let mut i = start;
        loop {
            if string_cmp(&p, &p1, i as usize, 0, v_len as usize) == 0 {
                ret = i;
                break;
            }
            if i == stop {
                break;
            }
            i += inc;
        }
    }

    Ok(JSValue::Int(ret))
}

fn js_string_replace(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_replace_all = cc.magic() != 0;

    let this = cc.this();

    if this.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let search = cc.arg(0);
    let replacement = cc.arg(1);

    if !search.is_undefined_or_null() {
        #[cfg(feature = "builtin_regexp")]
        if is_replace_all {
            check_regexp_g_flag(ctx, search)?;
        }

        #[cfg(feature = "builtin_symbol")]
        if let Some(o) = search.as_object() {
            let replacer = o.get_property(ctx, Atom::CONST_Symbol_replace)?;
            if !replacer.is_undefined_or_null() {
                return replacer.call(ctx, search, &[this, replacement]);
            }
        }
    }

    let str = this.to_string_non_undefined_or_null(ctx)?;
    let cc = str.content();
    let buf_len = cc.len();

    let search_for = search.to_string(ctx)?;
    let (replace_func, replace_str) = if replacement.is_callable() {
        (true, None)
    } else {
        (false, Some(replacement.to_string(ctx)?))
    };

    let mut is_first = true;
    let mut endOfLastMatch = 0_usize;
    let mut res_string = StringBuf::Empty;

    loop {
        let found = if search_for.is_empty() {
            if is_first {
                Some(0)
            } else if endOfLastMatch >= buf_len {
                None
            } else {
                Some(endOfLastMatch + 1)
            }
        } else {
            cc.index_of(search_for.content(), endOfLastMatch as _)
        };

        if let Some(pos) = found {
            // calculate replacement
            let replacement = if replace_func {
                replacement
                    .call(
                        ctx,
                        JSValue::UNDEFINED,
                        &[
                            JSValue::make_string(search_for),
                            JSValue::from(pos as u32),
                            JSValue::make_string(str),
                        ],
                    )
                    .and_then(|v| v.to_string(ctx))
            } else {
                let sb = StringData::substitution_buf(
                    search_for.content(),
                    cc,
                    pos,
                    replace_str.as_ref().unwrap().content(),
                    |_| Ok(None),
                    0,
                    false,
                    |_| Ok(None),
                )
                .map_err(|e| {
                    if e.is_some() {
                        ctx.throw_out_of_memory();
                    }
                    -1
                })?;

                ctx.create_string_with_buf(sb)
            }?;

            res_string
                .push_content_range(cc, endOfLastMatch..pos)
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

            res_string
                .push_content(replacement.content())
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

            endOfLastMatch = pos + search_for.len();
            is_first = false;

            if !is_replace_all {
                break;
            }
        } else if is_first {
            return Ok(JSValue::make_string(str));
        } else {
            break;
        }
    }

    // concat remainder part of string
    if endOfLastMatch < str.len() {
        res_string
            .push_chars_iter(cc.chars_since(endOfLastMatch).map(|(_, c)| c))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    }

    // create result string
    ctx.create_string_with_buf(res_string)
        .map(JSValue::make_string)
}

fn js_string_split(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    if this.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let separator = cc.arg(0);
    let limit = cc.arg(1);

    #[cfg(feature = "builtin_symbol")]
    if !separator.is_undefined_or_null()
        && let Some(o) = separator.as_object()
        && let Some(r) = o.invoke_defined(ctx, Atom::CONST_Symbol_split, &[this, limit])?
    {
        return r;
    }

    // don't change following parse order of str, limit, sep
    let str = this.to_string_non_undefined_or_null(ctx)?;
    let str_len = str.len();

    let lim = if limit.is_undefined() {
        0xffff_ffff
    } else {
        limit.to_number(ctx)?.to_u32()
    };

    let sep = separator.to_string(ctx)?;
    let sep_len = sep.len();

    // create result array
    let mut ret_arr = Array::create(ctx)?;

    if lim == 0 {
        return Ok(ret_arr.into());
    }

    if separator.is_undefined() {
        ret_arr.define_property_value_u32(
            ctx,
            0,
            JSValue::make_string(str),
            JS_PROP_C_W_E | JS_PROP_THROW,
        )?;
        return Ok(ret_arr.into());
    }

    if sep_len == 0 {
        for (i, (_, c)) in str.content().chars().enumerate().take(lim as usize) {
            let t = ctx.create_string_from_char(c)?;
            ret_arr.define_property_value_u32(
                ctx,
                i as _,
                JSValue::make_string(t),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;
        }
    } else {
        let mut cursor = 0;
        let mut idx = 0u32;

        let sc = sep.content();
        let cc = str.content();

        while let Some(i) = cc.index_of(sc, cursor) {
            let t = str.sub_string(ctx, cursor..i)?;
            ret_arr.define_property_value_u32(
                ctx,
                idx,
                JSValue::make_string(t),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;
            idx += 1;
            if idx == lim {
                break;
            }
            cursor = i + sep_len;
        }

        if idx < lim {
            cursor = std::cmp::min(cursor, str_len);

            // remaining part of str
            let t = str.sub_string(ctx, cursor..str_len)?;
            ret_arr.define_property_value_u32(
                ctx,
                idx as _,
                JSValue::make_string(t),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;
        }
    }

    Ok(ret_arr.into())
}

fn js_string_substring(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;

    let len = p.len() as i32;
    let a = cc.arg(0).to_number(ctx)?.to_i32_clamp(0, len, 0);
    let b = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_number(ctx)?.to_i32_clamp(0, len, 0)
    } else {
        len
    };
    let (start, end) = if a < b { (a, b) } else { (b, a) };

    p.sub_string(ctx, start as usize..end as usize)
        .map(JSValue::make_string)
}

fn js_string_substr(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let len = p.len() as i32;

    let a: i32 = cc.arg(0).to_number(ctx)?.to_i32_clamp(0, len, len);
    let n = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_number(ctx)?.to_i32_clamp(0, len - a, 0)
    } else {
        len - a
    };

    p.sub_string(ctx, a as usize..(a + n) as usize)
        .map(JSValue::make_string)
}

fn js_string_slice(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let len = p.len() as i32;
    let start = cc.arg(0).to_number(ctx)?.to_i32_clamp(0, len, len);
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_number(ctx)?.to_i32_clamp(0, len, len)
    } else {
        len
    };

    p.sub_string(ctx, start as usize..std::cmp::max(end, start) as usize)
        .map(JSValue::make_string)
}

fn js_string_pad(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_pad_end = cc.magic() != 0;

    // don't change parse args order.
    let str = cc.this().to_string_non_undefined_or_null(ctx)?;
    let len = str.len() as i32;

    let mut ttl_len: i32 = cc.arg(0).to_number(ctx)?.to_i32_saturating();
    if len >= ttl_len {
        return Ok(JSValue::make_string(str));
    }
    ttl_len -= len;

    let mut fill_str: Option<StringRef> = None;
    let mut fill_char = CharCode::Char(' ');

    if cc.length() > 1 && !cc.arg(1).is_undefined() {
        let s = cc.arg(1).to_string(ctx)?;
        if s.is_empty() {
            return Ok(JSValue::make_string(str));
        }

        if s.len() == 1 {
            fill_char = CharCode::from(s.content().get(0).unwrap());
        } else {
            fill_str.replace(s);
        }
    }

    if ttl_len > JS_STRING_LEN_MAX as i32 {
        ctx.throw_internal_error("string too long");
        return Err(-1);
    }

    let mut sbuf = if is_pad_end {
        str.content().to_buf().map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?
    } else {
        StringBuf::Empty
    };

    if let Some(s) = fill_str {
        // fill with str
        let cnt = (ttl_len as usize).div_ceil(s.len());

        if let Some(bb) = s.ascii() {
            let fill = std::iter::repeat_n(bb, cnt)
                .flatten()
                .copied()
                .take(ttl_len as usize);
            sbuf.push_u8_iter(fill)
        } else {
            let fill = std::iter::repeat_n(s.content().chars(), cnt)
                .flatten()
                .flat_map(|(_, cc)| {
                    let mut a = [0; 2];
                    cc.encode_utf16(&mut a);
                    a
                })
                .take(ttl_len as usize);
            sbuf.push_u16_iter(fill)
        }
    } else {
        // fill with char
        sbuf.push_chars_iter(std::iter::repeat_n(fill_char, ttl_len as usize))
    }
    .map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    if !is_pad_end {
        sbuf.push_content(str.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

fn js_string_repeat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;

    let repeat: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
    if !(0..=0x7fffffff).contains(&repeat) {
        ctx.throw_range_error("invalid repeat count");
        return Err(-1);
    }

    let repeat = repeat as usize;
    let len = p.len();
    if len == 0 || repeat == 1 {
        return Ok(JSValue::make_string(p));
    }
    if repeat * len > JS_STRING_LEN_MAX {
        ctx.throw_internal_error("string too long");
        return Err(-1);
    }

    let mut res_string = StringBuf::with_capacity(repeat * len, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    let sdata = p.content();
    for _ in 0..repeat {
        res_string
            .push_chars_iter(sdata.chars().map(|(_, c)| c))
            .unwrap();
    }
    res_string.shrink_to_fit();

    ctx.create_string_with_buf(res_string)
        .map(JSValue::make_string)
}

fn js_string_includes(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let arg0 = cc.arg(0);

    #[cfg(feature = "builtin_regexp")]
    if super::regexp::is_regexp(ctx, arg0)? {
        ctx.throw_type_error("regex not supported");
        return Err(-1);
    }

    let pat = arg0.to_string(ctx)?;
    let mut str_len = p.len();
    let pat_len = pat.len();

    let mut pos = if cc.magic() == MAGIC_ENDS_WITH {
        str_len
    } else {
        0
    };
    if cc.length() > 1 {
        let a = cc.arg(1);
        if !a.is_undefined() {
            pos = a.to_number(ctx)?.to_i32_clamp(0, str_len as _, 0) as usize;
            if cc.magic() == MAGIC_ENDS_WITH {
                str_len = pos;
            }
        }
    }

    if pat_len == 0 {
        Ok(JSValue::Bool(true))
    } else if pat_len > str_len {
        Ok(JSValue::Bool(false))
    } else {
        match cc.magic() {
            MAGIC_INCLUDES => Ok(p
                .content()
                .index_of(pat.content(), pos)
                .map_or(JSValue::Bool(false), |_| JSValue::Bool(true))),

            m @ (MAGIC_STARTS_WITH | MAGIC_ENDS_WITH) => {
                let mut it1 = p.content().chars_range(if m == MAGIC_ENDS_WITH {
                    (str_len - pat_len)..str_len
                } else if pos > 0 {
                    pos..str_len
                } else {
                    0..str_len
                });
                let mut it2 = pat.content().chars();

                let b = loop {
                    match (it1.next(), it2.next()) {
                        (Some((_, c1)), Some((_, c2))) => {
                            if c1 != c2 {
                                break false;
                            }
                        }
                        (_, None) => {
                            break true;
                        }
                        (None, _) => {
                            break false;
                        }
                    }
                };
                Ok(b.into())
            }

            _ => unreachable!(),
        }
    }
}

/// ensure regexp `obj` must have g flag.
#[cfg(feature = "builtin_regexp")]
fn check_regexp_g_flag(ctx: &mut ScopedContext, obj: JSValue) -> Result<(), i32> {
    if super::regexp::is_regexp(ctx, obj)? {
        let flags = obj.get_property(ctx, Atom::CONST_flags)?;
        if flags.is_undefined_or_null() {
            ctx.throw_type_error("cannot convert to object");
            return Err(-1);
        } else {
            let s = flags.to_string(ctx)?;
            if s.index_of_char('g', 0).is_none() {
                ctx.throw_type_error("regexp must have the 'g' flag");
                return Err(-1);
            }
        }
    }
    Ok(())
}

const MAGIC_MATCH: i32 = 1;
const MAGIC_MATCH_ALL: i32 = 2;
const MAGIC_MATCH_SEARCH: i32 = 3;

fn js_string_match(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let this = cc.this();

    if this.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let method_name: Atom = {
        #[cfg(feature = "builtin_symbol")]
        match magic {
            MAGIC_MATCH => Atom::CONST_Symbol_match,
            MAGIC_MATCH_ALL => Atom::CONST_Symbol_matchAll,
            MAGIC_MATCH_SEARCH => Atom::CONST_Symbol_search,
            _ => {
                unreachable!();
            }
        }

        #[cfg(not(feature = "builtin_symbol"))]
        match magic {
            MAGIC_MATCH => Atom::new(ctx.rt_ptr(), "match")?,
            MAGIC_MATCH_ALL => Atom::new(ctx.rt_ptr(), "matchAll")?,
            MAGIC_MATCH_SEARCH => Atom::new(ctx.rt_ptr(), "search")?,
            _ => {
                unreachable!();
            }
        }
    };

    let regexp_val = cc.arg(0);

    if let Some(o) = regexp_val.as_object() {
        let func = o.get_property(ctx, method_name)?;

        #[cfg(feature = "builtin_regexp")]
        if magic == MAGIC_MATCH_ALL {
            check_regexp_g_flag(ctx, regexp_val)?;
        }

        if !func.is_undefined_or_null() {
            return func.call(ctx, regexp_val, std::slice::from_ref(&this));
        }
    }

    // create RegExp instance, invoke method
    #[cfg(feature = "builtin_regexp")]
    {
        let mut args = [
            regexp_val,         // pattern
            JSValue::UNDEFINED, // flags
        ];
        let mut args_len = 1;
        if magic == MAGIC_MATCH_ALL {
            args[args_len] = JSValue::make_string(ctx.create_string_from_char('g'.into())?);
            args_len += 1;
        }
        let regexp_obj = {
            let f = ctx.regexp_ctor();
            f.call_ctor(ctx, &args[0..args_len])?
        };

        // NOTE: `regexp_obj` does NOT need an explicit `gc_protect_transient_force`.
        // It is the freshly-allocated result of `regexp_ctor().call_ctor(...)`, and a
        // freshly-allocated node is already protected from collection within the
        // current GcScope (the engine's "new allocation = young root" invariant: a
        // fresh node is either a scope-local young root or a permanent root). So it
        // stays alive across the `to_string_non_undefined_or_null` safepoint and
        // through `regexp_obj.invoke(...)` with no extra guard. The explicit call
        // would in fact be a no-op here, because `add_non_local` skips any node that
        // is already a root or scope-local.
        //
        // This line is kept only as a harmless backstop: it would only matter if
        // `call_ctor` ever returned a *non-fresh* / aliased node (e.g. a constructor
        // that does `return existingObject`), which the built-in RegExp constructor
        // never does. In the normal flow it is never triggered, hence commented out.
        // ctx.gc_protect_transient_force(&regexp_obj);

        let s = this.to_string_non_undefined_or_null(ctx)?;
        regexp_obj.invoke(
            ctx,
            method_name,
            std::slice::from_ref(&JSValue::make_string(s)),
        )
    }

    #[cfg(not(feature = "builtin_regexp"))]
    {
        ctx.throw_internal_error("regex not supported");
        Err(-1)
    }
}

fn js_string_trim(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let str = this.to_string_non_undefined_or_null(ctx)?;
    let mut s = str.content().to_buf().map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    s.trim(cc.magic() & 1 != 0, cc.magic() & 2 != 0);
    ctx.create_string_with_buf(s).map(JSValue::make_string)
}

pub fn string_cmp(p1: &StringRef, p2: &StringRef, x1: usize, x2: usize, len: usize) -> i32 {
    let mut it1 = p1.content().chars_since(x1);
    let mut it2 = p2.content().chars_since(x2);

    for _ in 0..len {
        match (it1.next(), it2.next()) {
            (Some((_, c1)), Some((_, c2))) => {
                let c1 = c1.to_u32();
                let c2 = c2.to_u32();
                if c1 > c2 {
                    return 1;
                } else if c1 < c2 {
                    return -1;
                }
            }
            (None, None) => {
                break;
            }
            (Some(_), None) => {
                return 1;
            }
            (None, Some(_)) => {
                return -1;
            }
        }
    }

    0
}

/// also used for String.prototype.valueOf
fn js_string_toString(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    js_thisStringValue(ctx, cc.this()).map(JSValue::make_string)
}

#[inline]
fn js_thisStringValue(ctx: &mut ScopedContext, this_val: JSValue) -> Result<StringRef, i32> {
    this_val.as_string().ok_or_else(|| {
        ctx.throw_type_error("not a string");
        -1
    })
}

fn js_string_toLowerCase(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let to_lower = cc.magic() != 0;

    let this = cc.this();
    let p = this.to_string_non_undefined_or_null(ctx)?;
    if p.is_empty() {
        return Ok(JSValue::make_string(p));
    }

    let sbuf = match p.content() {
        StringData::Ascii(s) => {
            let s8 = unsafe { str::from_utf8_unchecked(s.as_ref()) };
            StringBuf::Ascii(
                if to_lower {
                    s8.to_ascii_lowercase()
                } else {
                    s8.to_ascii_uppercase()
                }
                .into_bytes(),
            )
        }
        StringData::Utf16(s) => {
            let mut buf = StringBuf::with_capacity(s.len(), false).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            let s8 = String::from_utf16_lossy(s);
            buf.push_u16_iter(
                if to_lower {
                    s8.to_lowercase()
                } else {
                    s8.to_uppercase()
                }
                .encode_utf16(),
            )
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            buf
        }
        StringData::AsciiStatic(s) => {
            let t = if to_lower {
                s.to_lowercase()
            } else {
                s.to_uppercase()
            };
            if t.eq(s) {
                return Ok(JSValue::make_string(p));
            }
            StringBuf::from_utf8(t.into_bytes())?
        }
        s => {
            unreachable!("{:?}", s);
        }
    };

    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

#[cfg(feature = "all_unicode")]
fn js_string_localeCompare(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let a = cc.this().to_string_non_undefined_or_null(ctx)?;
    let b = cc.arg(0).to_string(ctx)?;

    // locale normalize
    let mut u1 = a.content().chars_lossy().nfc();
    let mut u2 = b.content().chars_lossy().nfc();

    let cmp = loop {
        match (u1.next(), u2.next()) {
            (Some(c1), Some(c2)) if c1 == c2 => {
                continue;
            }
            (Some(c1), Some(c2)) => {
                break c1.cmp(&c2);
            }
            (None, None) => break std::cmp::Ordering::Equal,
            (None, Some(_)) => break std::cmp::Ordering::Less,
            (Some(_), None) => break std::cmp::Ordering::Greater,
        }
    };

    match cmp {
        std::cmp::Ordering::Less => Ok(JSValue::Int(-1)),
        std::cmp::Ordering::Equal => Ok(JSValue::Int(0)),
        std::cmp::Ordering::Greater => Ok(JSValue::Int(1)),
    }
}

#[cfg(feature = "all_unicode")]
fn js_string_normalize(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = cc.this().to_string_non_undefined_or_null(ctx)?;

    #[allow(clippy::upper_case_acronyms)]
    enum UnicodeNormalizationEnum {
        NFC,
        NFD,
        NFKC,
        NFKD,
    }
    let n_type = if cc.length() == 0 || cc.arg(0).is_undefined() {
        UnicodeNormalizationEnum::NFC
    } else {
        match cc.arg(0) {
            JSValue::NULL => {
                ctx.throw_range_error("bad normalization form");
                return Err(-1);
            }
            form => match form.to_string(ctx)?.content().encode_utf8(false).as_ref() {
                "NFC" => UnicodeNormalizationEnum::NFC,
                "NFD" => UnicodeNormalizationEnum::NFD,
                "NFKC" => UnicodeNormalizationEnum::NFKC,
                "NFKD" => UnicodeNormalizationEnum::NFKD,
                _ => {
                    ctx.throw_range_error("bad normalization form");
                    return Err(-1);
                }
            },
        }
    };

    let chars_it = val.content().chars_lossy();
    let ns = match n_type {
        UnicodeNormalizationEnum::NFC => String::from_iter(chars_it.nfc()),
        UnicodeNormalizationEnum::NFD => String::from_iter(chars_it.nfd()),
        UnicodeNormalizationEnum::NFKC => String::from_iter(chars_it.nfkc()),
        UnicodeNormalizationEnum::NFKD => String::from_iter(chars_it.nfkd()),
    };

    ctx.create_string_from_utf8(ns.into_bytes())
        .map(JSValue::make_string)
}

// non-standard
fn js_string__quote(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = cc.this();
    if val.is_undefined_or_null() {
        ctx.throw_type_error("undefined or null is not allowed");
        Err(-1)
    } else {
        let s = val.to_string(ctx)?;
        let s = s.content().encode_utf8(true);
        ctx.create_string_from_utf8(format!("\"{}\"", s).into_bytes())
            .map(JSValue::make_string)
    }
}

pub const js_string_proto_funcs: &[FieldDef] = &[
    FieldDef::INT32("length", 0, JS_PROP_CONFIGURABLE),
    FieldDef::FUNC("charAt", 1, js_string_at, 0),
    FieldDef::FUNC("at", 1, js_string_at, 1),
    FieldDef::FUNC("charCodeAt", 1, js_string_at, 2),
    FieldDef::FUNC("concat", 1, js_string_concat, 0),
    FieldDef::FUNC("codePointAt", 1, js_string_codePointAt, 0),
    FieldDef::FUNC("isWellFormed", 0, js_string_isWellFormed, 0),
    FieldDef::FUNC("toWellFormed", 0, js_string_toWellFormed, 0),
    FieldDef::FUNC("indexOf", 1, js_string_indexOf, 0),
    FieldDef::FUNC("lastIndexOf", 1, js_string_indexOf, 1),
    FieldDef::FUNC("includes", 1, js_string_includes, MAGIC_INCLUDES as _),
    FieldDef::FUNC("endsWith", 1, js_string_includes, MAGIC_ENDS_WITH as _),
    FieldDef::FUNC("startsWith", 1, js_string_includes, MAGIC_STARTS_WITH as _),
    FieldDef::FUNC("match", 1, js_string_match, MAGIC_MATCH as _),
    FieldDef::FUNC("matchAll", 1, js_string_match, MAGIC_MATCH_ALL as _),
    FieldDef::FUNC("search", 1, js_string_match, MAGIC_MATCH_SEARCH as _),
    FieldDef::FUNC("split", 2, js_string_split, 0),
    FieldDef::FUNC("substring", 2, js_string_substring, 0),
    FieldDef::FUNC("substr", 2, js_string_substr, 0),
    FieldDef::FUNC("slice", 2, js_string_slice, 0),
    FieldDef::FUNC("repeat", 1, js_string_repeat, 0),
    FieldDef::FUNC("replace", 2, js_string_replace, 0),
    FieldDef::FUNC("replaceAll", 2, js_string_replace, 1),
    FieldDef::FUNC("padEnd", 1, js_string_pad, 1),
    FieldDef::FUNC("padStart", 1, js_string_pad, 0),
    FieldDef::FUNC("trim", 0, js_string_trim, 3),
    FieldDef::FUNC("trimEnd", 0, js_string_trim, 2),
    FieldDef::ALIAS("trimRight", "trimEnd"),
    FieldDef::FUNC("trimStart", 0, js_string_trim, 1),
    FieldDef::ALIAS("trimLeft", "trimStart"),
    FieldDef::FUNC("toString", 0, js_string_toString, 0),
    FieldDef::FUNC("valueOf", 0, js_string_toString, 0),
    FieldDef::FUNC("toLowerCase", 0, js_string_toLowerCase, 1),
    FieldDef::FUNC("toUpperCase", 0, js_string_toLowerCase, 0),
    FieldDef::FUNC("toLocaleLowerCase", 0, js_string_toLowerCase, 1),
    FieldDef::FUNC("toLocaleUpperCase", 0, js_string_toLowerCase, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::FUNC(
        "[Symbol.iterator]",
        0,
        js_create_array_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) | 4,
    ),
    //
    // all unicode
    #[cfg(feature = "all_unicode")]
    FieldDef::FUNC("normalize", 0, js_string_normalize, 0),
    #[cfg(feature = "all_unicode")]
    FieldDef::FUNC("localeCompare", 1, js_string_localeCompare, 0),
    // DEBUG: for repl only
    FieldDef::FUNC("__quote", 0, js_string__quote, 0),
];

fn js_string_iterator_next(
    ctx: &mut ScopedContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    if let ObjectPayload::STRING_ITERATOR(it) = cc
        .this()
        .retrive_object_payload(ctx, JS_CLASS_STRING_ITERATOR)?
    {
        match it.value() {
            JSValue::UNDEFINED => {
                *state = IteratorState::Done;
                Ok(JSValue::UNDEFINED)
            }
            JSValue::String(ss) => {
                // iterate over string buf to get char or code at `idx`
                if let Some((span, c)) = ss.content().chars_since(it.index() as usize).next() {
                    it.set_index(span.end as u32);
                    *state = IteratorState::NotDone;
                    ctx.create_string_from_char(c).map(JSValue::make_string)
                } else {
                    it.set_value(JSValue::UNDEFINED);
                    *state = IteratorState::Done;
                    Ok(JSValue::UNDEFINED)
                }
            }
            _ => {
                unreachable!();
            }
        }
    } else {
        unreachable!()
    }
}

pub const js_string_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 0, js_string_iterator_next, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "String Iterator",
        JS_PROP_CONFIGURABLE,
    ),
];

fn js_string_fromCharCode(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut sbuf = StringBuf::Empty;
    for a in cc.arguments() {
        let cp = a.to_number(ctx)?.to_u16();
        sbuf.push_u16(cp).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }
    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

fn js_string_fromCodePoint(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut sbuf = StringBuf::Empty;

    if cc.length() > 0 {
        sbuf.try_reserve_exact(cc.length()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        for a in cc.arguments() {
            let code: i32 = if let JSValue::Int(i) = a {
                if (0..=0x10ffff).contains(i) {
                    *i
                } else {
                    -1 // bad code
                }
            } else {
                let d: f64 = a.to_number(ctx)?.to_f64();
                if d.fract() == 0.0 {
                    let n = d.trunc() as i64;
                    if !(0..=0x10ffff).contains(&n) {
                        -1 // bad code
                    } else {
                        n as i32
                    }
                } else {
                    -1 // bad code
                }
            };

            if code >= 0 {
                sbuf.push_u32(code as u32).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            } else {
                ctx.throw_range_error("invalid code point");
                return Err(-1);
            }
        }
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

fn js_string_raw(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // raw(temp,...a)

    let cooked = cc.arg(0).objectify(ctx)?;
    let raw = cooked.get_property(ctx, Atom::CONST_raw)?.objectify(ctx)?;

    let n = raw.get_length_prop(ctx)?;
    let mut sbuf = StringBuf::Empty;

    for i in 0..n {
        let str = raw.get_property_i64(ctx, i)?.to_string(ctx)?;
        sbuf.push_content(str.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        if i < n - 1 && i + 1 < cc.length() as i64 {
            let val = cc.arg(i as usize + 1);
            let s = val.to_string(ctx)?;
            sbuf.push_content(s.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

pub const js_string_funcs: &[FieldDef] = &[
    FieldDef::FUNC("fromCharCode", 1, js_string_fromCharCode, 0),
    FieldDef::FUNC("fromCodePoint", 1, js_string_fromCodePoint, 0),
    FieldDef::FUNC("raw", 1, js_string_raw, 0),
];

pub(crate) static CLASS_DEF_STRING: ClassDef = ClassDef::builtin(
    crate::Atom::CONST_String,
    js_string_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_string_constructor, 1, true)
            .entries(js_string_funcs)
            .expose_to_global(),
    ),
)
.exotic(&js_string_exotic_methods);

pub(crate) static CLASS_DEF_STRING_ITERATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_String_Iterator,
    js_string_iterator_proto_funcs,
    None,
);
