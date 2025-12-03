use std::ops::Deref;

use unicode_normalization::UnicodeNormalization;
use widestring::U16Str;

use crate::{
    JS_CLASS_STRING_ITERATOR,
    atom::Atom,
    builtins::iterator::{JSIteratorKindEnum, js_create_array_iterator},
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState, JS_CallConstructor},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_THROW, ObjectPayload},
    string::{
        JS_ConcatString, JS_ConcatString3, JS_STRING_LEN_MAX, StringBuf, StringChar, StringData,
        StringImpl, StringRef,
    },
    utils::unlikely,
    value::JSValue,
};

use super::regexp::js_is_regexp;

const MAGIC_INCLUDES: i32 = 1;
const MAGIC_STARTS_WITH: i32 = 2;
const MAGIC_ENDS_WITH: i32 = 3;

pub fn js_string_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let new_target = *cc.this();

    let str_val = if cc.length() == 0 {
        Atom::CONST_empty_string.force_string(ctx)?
    } else if let JSValue::Symbol(a) = cc.arg(0)
        && new_target.is_undefined()
    {
        // "Symbol(desc)"
        JS_ConcatString3(ctx, c"Symbol(", a.string().into(), c")")?
    } else {
        cc.arg(0).to_string(ctx)?
    };

    if !new_target.is_undefined() {
        // make String object using `new_target` as prototype.
        str_val.objectify(ctx, new_target).map(JSValue::Object)
    } else {
        Ok(str_val.into())
    }
}

/// impl for `charAt()`, `at()`, `chatCodeAt()`
pub fn js_string_at(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let str = cc.this().to_string_non_undefined_or_null(cx)?;
    let what = cc.magic(); // 0: charAt, 1: at, 2: charCodeAt
    let buf_len = str.len() as i32;

    let mut buf_idx: i32 = cc.arg(0).to_i32_saturating(cx)?;
    if buf_idx < 0 && what == 1 {
        // at() allows minus index
        buf_idx += buf_len;
    }

    if buf_idx < 0 || buf_idx >= buf_len {
        match what {
            0 => Atom::CONST_empty_string
                .force_string(cx)
                .map(JSValue::String),
            1 => Ok(JSValue::UNDEFINED),
            2 => Ok(JSValue::Float64(f64::NAN)),
            _ => unreachable!(),
        }
    } else {
        let code = str.get(buf_idx as usize).unwrap();
        if what == 2 {
            // charCodeAt() returns int
            Ok(JSValue::Int(code as i32))
        } else {
            // charAt(), at() returns string
            cx.create_string_from_char(code.into()).map(JSValue::String)
        }
    }
}

pub fn js_string_codePointAt(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let s = cc.this().to_string_non_undefined_or_null(cx)?;
    let buf_idx: i32 = cc.arg(0).to_i32_saturating(cx)?;
    if buf_idx < 0 || buf_idx >= s.len() as i32 {
        Ok(JSValue::UNDEFINED)
    } else {
        let (_, c) = s.content().iter_since(buf_idx as usize).next().unwrap();
        Ok(JSValue::from(c.to_u32()))
    }
}

pub fn js_string_concat(ctx: ContextRef, cc: CallContext) -> JSResult {
    // TODO: Use more efficient method
    // XXX: should use string_buffer?
    let mut s = cc.this().to_string_non_undefined_or_null(&ctx)?;
    for a in cc.arguments() {
        s = JS_ConcatString(&ctx, s.into(), *a)?;
    }
    Ok(s.into())
}

// return the position of the first invalid character in the string or
// -1 if none
pub(crate) unsafe fn js_string_find_invalid_codepoint(p: &StringImpl) -> i32 {
    if let Some(ws) = p.content().utf16() {
        U16Str::from_slice(ws)
            .chars()
            .position(|c| c.is_err())
            .map_or(-1, |i| i as i32)
    } else {
        -1
    }
}

pub fn js_string_isWellFormed(ctx: ContextRef, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(&ctx)?;
    let ret = unsafe { js_string_find_invalid_codepoint(&p) } < 0;
    Ok(JSValue::Bool(ret))
}

pub fn js_string_toWellFormed(ctx: ContextRef, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(&ctx)?;

    // avoid reallocating the string if it is well-formed
    let i = unsafe { js_string_find_invalid_codepoint(&p) };
    if i < 0 {
        return Ok(p.into());
    }

    if let Some(ws) = p.content().utf16() {
        let mut buf = Vec::<u16>::new();
        buf.try_reserve_exact(ws.len() * 3 / 2).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        for c in U16Str::from_slice(ws).chars_lossy() {
            buf.extend_from_slice(c.encode_utf16(&mut [0; 2]));
        }
        ctx.create_string_with_buf(buf.into()).map(JSValue::String)
    } else {
        unreachable!()
    }
}

pub fn js_string_indexOf(ctx: ContextRef, cc: CallContext) -> JSResult {
    let lastIndexOf = cc.magic();
    let cx = ctx.deref();

    let p = cc.this().to_string_non_undefined_or_null(cx)?;
    let p1 = cc.arg(0).to_string(cx)?;

    let len = p.len() as i32;
    let v_len = p1.len() as i32;

    let start: i32;
    let stop: i32;
    let inc: i32;

    if lastIndexOf != 0 {
        let mut pos = len - v_len;
        if cc.length() > 1 {
            let d: f64 = cc.arg(1).to_f64(cx)?;
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
            cc.arg(1).to_i32_clamp(cx, 0, len, 0)?
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

pub fn js_string_replace(ctx: ContextRef, cc: CallContext) -> JSResult {
    let is_replace_all = cc.magic() != 0;
    let cx = ctx.deref();

    let this = *cc.this();
    if this.is_undefined_or_null() {
        cx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let search = *cc.arg(0);
    let replacement = *cc.arg(1);

    if !search.is_undefined_or_null() {
        if is_replace_all && check_regexp_g_flag(cx, &search) < 0 {
            return Err(-1);
        }

        let replacer = search.get_property(cx, Atom::CONST_Symbol_replace)?;
        if !replacer.is_undefined_or_null() {
            return replacer.call(&ctx, search, &[this, replacement]);
        }
    }

    let str = this.to_string_non_undefined_or_null(cx)?;
    let cc = str.content();
    let buf_len = cc.len();

    let search_for = search.to_string(cx)?;
    let (replace_func, replace_str) = if replacement.is_function() {
        (true, None)
    } else {
        (false, Some(replacement.to_string(cx)?))
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
                        &ctx,
                        JSValue::UNDEFINED,
                        &[search_for.into(), JSValue::from(pos as u32), str.into()],
                    )
                    .and_then(|v| v.to_string(cx))
            } else {
                let sb = StringData::substitution_buf(
                    search_for.content(),
                    cc,
                    pos,
                    replace_str.as_ref().unwrap().content(),
                    |_| Ok(None),
                    0,
                    |_| Ok(None),
                )
                .map_err(|e| {
                    if e.is_some() {
                        cx.throw_out_of_memory();
                    }
                    -1
                })?;

                cx.create_string_with_buf(sb)
            }?;

            res_string
                .push_content_range(cc, endOfLastMatch..pos)
                .map_err(|_| {
                    cx.throw_out_of_memory();
                    -1
                })?;

            res_string
                .push_content(replacement.content())
                .map_err(|_| {
                    cx.throw_out_of_memory();
                    -1
                })?;

            endOfLastMatch = pos + search_for.len();
            is_first = false;

            if !is_replace_all {
                break;
            }
        } else if is_first {
            return Ok(str.into());
        } else {
            break;
        }
    }

    // concat remainder part of string
    if endOfLastMatch < str.len() {
        res_string
            .push_chars_iter(cc.iter_since(endOfLastMatch).map(|(_, c)| c))
            .map_err(|_| {
                cx.throw_out_of_memory();
                -1
            })?;
    }

    // create result string
    cx.create_string_with_buf(res_string).map(JSValue::String)
}

pub fn js_string_split(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let this = *cc.this();
    if this.is_undefined_or_null() {
        cx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let separator = cc.arg(0);
    let limit = *cc.arg(1);

    if !separator.is_undefined_or_null()
        && let Some(r) = separator.invoke_defined(&ctx, Atom::CONST_Symbol_split, &[this, limit])?
    {
        return r;
    }

    // don't change following parse order of str, limit, sep
    let str = this.to_string_non_undefined_or_null(cx)?;
    let str_len = str.len();

    let lim = if limit.is_undefined() {
        0xffff_ffff
    } else {
        limit.to_u32(cx)?
    };

    let sep = separator.to_string(cx)?;
    let sep_len = sep.len();

    // create result array
    let mut ret_arr = cx.create_array()?;

    if lim == 0 {
        return Ok(ret_arr.into());
    }

    if separator.is_undefined() {
        ret_arr.define_property_value_u32(cx, 0, str.into(), JS_PROP_C_W_E | JS_PROP_THROW)?;
        return Ok(ret_arr.into());
    }

    if sep_len == 0 {
        for (i, (_, c)) in str.content().iter().enumerate().take(lim as usize) {
            let t = cx.create_string_from_char(c)?;
            ret_arr.define_property_value_u32(
                cx,
                i as _,
                t.into(),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;
        }
    } else {
        let mut cursor = 0;
        let mut idx = 0u32;

        let sc = sep.content();
        let cc = str.content();

        while let Some(i) = cc.index_of(sc, cursor) {
            let t = str.sub_string(cx, cursor..i)?;
            ret_arr.define_property_value_u32(cx, idx, t.into(), JS_PROP_C_W_E | JS_PROP_THROW)?;
            idx += 1;
            if idx == lim {
                break;
            }
            cursor = i + sep_len;
        }

        if idx < lim {
            cursor = std::cmp::min(cursor, str_len);

            // remaining part of str
            let t = str.sub_string(cx, cursor..str_len)?;
            ret_arr.define_property_value_u32(
                cx,
                idx as _,
                t.into(),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;
        }
    }

    Ok(ret_arr.into())
}

pub fn js_string_substring(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();
    let p = cc.this().to_string_non_undefined_or_null(cx)?;

    let len = p.len() as i32;
    let a = cc.arg(0).to_i32_clamp(cx, 0, len, 0)?;
    let b = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(cx, 0, len, 0)?
    } else {
        len
    };
    let (start, end) = if a < b { (a, b) } else { (b, a) };

    p.sub_string(cx, start as usize..end as usize)
        .map(JSValue::String)
}

pub fn js_string_substr(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let p = cc.this().to_string_non_undefined_or_null(cx)?;
    let len = p.len() as i32;

    let a: i32 = cc.arg(0).to_i32_clamp(cx, 0, len, len)?;
    let n = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(cx, 0, len - a, 0)?
    } else {
        len - a
    };

    p.sub_string(cx, a as usize..(a + n) as usize)
        .map(JSValue::String)
}

pub fn js_string_slice(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let p = cc.this().to_string_non_undefined_or_null(cx)?;
    let len = p.len() as i32;
    let start = cc.arg(0).to_i32_clamp(cx, 0, len, len)?;
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(cx, 0, len, len)?
    } else {
        len
    };

    p.sub_string(cx, start as usize..std::cmp::max(end, start) as usize)
        .map(JSValue::String)
}

pub fn js_string_pad(ctx: ContextRef, cc: CallContext) -> JSResult {
    let is_pad_end = cc.magic() != 0;
    let cx = ctx.deref();

    // don't change parse args order.
    let str = cc.this().to_string_non_undefined_or_null(cx)?;
    let len = str.len() as i32;

    let mut ttl_len: i32 = cc.arg(0).to_i32_saturating(cx)?;
    if len >= ttl_len {
        return Ok(str.into());
    }
    ttl_len -= len;

    let mut fill_str: Option<StringRef> = None;
    let mut fill_char = StringChar::Char(' ');

    if cc.length() > 1 && !cc.arg(1).is_undefined() {
        let s = cc.arg(1).to_string(cx)?;
        if s.is_empty() {
            return Ok(str.into());
        }

        if s.len() == 1 {
            fill_char = StringChar::from(s.content().get(0).unwrap());
        } else {
            fill_str.replace(s);
        }
    }

    if ttl_len > JS_STRING_LEN_MAX as i32 {
        cx.throw_internal_error("string too long");
        return Err(-1);
    }

    let mut sbuf = if is_pad_end {
        str.content().to_buf().map_err(|_| {
            cx.throw_out_of_memory();
            -1
        })?
    } else {
        StringBuf::Empty
    };

    if let Some(s) = fill_str {
        // fill with str
        let src = std::iter::repeat_n(
            s.content().iter_codepoint(),
            (ttl_len as usize).div_ceil(s.len()),
        )
        .flatten()
        .take(ttl_len as usize);

        sbuf.push_u16_iter(src)
    } else {
        // fill with char
        sbuf.push_chars_iter(std::iter::repeat_n(fill_char, ttl_len as usize))
    }
    .map_err(|_| {
        cx.throw_out_of_memory();
        -1
    })?;

    if !is_pad_end {
        sbuf.push_content(str.content()).map_err(|_| {
            cx.throw_out_of_memory();
            -1
        })?;
    }

    cx.create_string_with_buf(sbuf).map(JSValue::String)
}

pub fn js_string_repeat(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let p = cc.this().to_string_non_undefined_or_null(cx)?;

    let repeat: i64 = cc.arg(0).to_i64_saturating(cx)?;
    if !(0..=2147483647).contains(&repeat) {
        cx.throw_range_error("invalid repeat count");
        return Err(-1);
    }

    let n = repeat as usize;
    let len = p.len();
    if len == 0 || n == 1 {
        return Ok(p.into());
    }
    if repeat as usize * len > JS_STRING_LEN_MAX {
        cx.throw_internal_error("string too long");
        return Err(-1);
    }

    let mut res_string = StringBuf::with_capacity(n * len, false).map_err(|_| {
        cx.throw_out_of_memory();
        -1
    })?;
    res_string
        .push_chars_iter(std::iter::repeat_n(p.content().iter().map(|(_, c)| c), n).flatten())
        .unwrap();
    res_string.shrink_to_fit();

    cx.create_string_with_buf(res_string).map(JSValue::String)
}

pub fn js_string_includes(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let p = cc.this().to_string_non_undefined_or_null(cx)?;

    if js_is_regexp(cx, cc.arg(0))? {
        cx.throw_type_error("regex not supported");
        return Err(-1);
    }

    let pat = cc.arg(0).to_string(cx)?;

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
            pos = a.to_i32_clamp(cx, 0, str_len as _, 0)? as usize;
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
                let mut it1 = p.content().iter_range(if m == MAGIC_ENDS_WITH {
                    (str_len - pat_len)..str_len
                } else if pos > 0 {
                    pos..str_len
                } else {
                    0..str_len
                });
                let mut it2 = pat.content().iter();

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

fn js_string_localeCompare(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let a = cc.this().to_string_non_undefined_or_null(cx)?;
    let b = cc.arg(0).to_string(cx)?;

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

pub fn check_regexp_g_flag(ctx: &ContextImpl, regexp: &JSValue) -> i32 {
    let ret = if let Ok(b) = js_is_regexp(ctx, regexp) {
        b
    } else {
        return -1;
    };

    if ret {
        let flags = if let Ok(x) = regexp.get_property(ctx, Atom::CONST_flags) {
            x
        } else {
            return -1;
        };
        if flags.is_undefined_or_null() {
            ctx.throw_type_error("cannot convert to object");
            return -1;
        }

        let flags = if let Ok(s) = flags.to_string(ctx) {
            s
        } else {
            return -1;
        };

        let idx = flags.content().index_of_char('g', 0);
        if idx.is_none() {
            ctx.throw_type_error("regexp must have the 'g' flag");
            return -1;
        }
    }

    0
}

pub fn js_string_match(ctx: ContextRef, cc: CallContext) -> JSResult {
    let magic_method = Atom::from_id(cc.magic() as u32); // match, matchAll, search
    let cx = ctx.deref();

    let this = cc.this();
    if this.is_undefined_or_null() {
        cx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let regexp = *cc.arg(0);

    if !regexp.is_undefined_or_null() {
        let matcher = regexp.get_property(cx, magic_method)?;
        if unlikely(
            magic_method == Atom::CONST_Symbol_matchAll && check_regexp_g_flag(cx, &regexp) < 0,
        ) {
            return Err(-1);
        }
        if !matcher.is_undefined_or_null() {
            return matcher.call(&ctx, regexp, std::slice::from_ref(this));
        }
    }

    let str = this.to_string_non_undefined_or_null(cx)?;

    // make RegExp object
    let mut args_len = 1;
    let mut args = [regexp, JSValue::UNDEFINED];
    if magic_method == Atom::CONST_Symbol_matchAll {
        args[args_len] = cx.create_string_from_char('g'.into())?.into();
        args_len += 1;
    }

    let re_obj = {
        let ctor = cx.regexp_ctor; // avoid borrow for long
        JS_CallConstructor(&ctx, ctor, &args[0..args_len]).err()?
    };

    re_obj.invoke(&ctx, magic_method, &[str.into()])
}

pub fn js_string_trim(ctx: ContextRef, cc: CallContext) -> JSResult {
    let str = cc.this().to_string_non_undefined_or_null(&ctx)?;
    let mut s = str.content().to_buf().map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    s.trim(cc.magic() & 1 != 0, cc.magic() & 2 != 0);
    ctx.create_string_with_buf(s).map(JSValue::String)
}

pub fn string_cmp(p1: &StringImpl, p2: &StringImpl, x1: usize, x2: usize, len: usize) -> i32 {
    let mut it1 = p1.content().iter_since(x1);
    let mut it2 = p2.content().iter_since(x2);

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
pub fn js_string_toString(ctx: ContextRef, cc: CallContext) -> Result<JSValue, i32> {
    js_thisStringValue(ctx, *cc.this()).map(JSValue::String)
}

#[inline]
fn js_thisStringValue(ctx: ContextRef, this_val: JSValue) -> Result<StringRef, i32> {
    StringRef::try_from(this_val).inspect_err(|_| {
        ctx.throw_type_error("not a string");
    })
}

pub fn js_string_toLowerCase(ctx: ContextRef, cc: CallContext) -> JSResult {
    let to_lower = cc.magic() != 0;

    let p = cc.this().to_string_non_undefined_or_null(&ctx)?;
    if p.is_empty() {
        return Ok(p.into());
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
                return Ok(JSValue::String(p));
            }
            StringBuf::from_utf8(t.into_bytes())?
        }
        s => {
            unreachable!("{:?}", s);
        }
    };

    ctx.create_string_with_buf(sbuf).map(JSValue::String)
}

fn js_string_normalize(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let val = cc.this().to_string_non_undefined_or_null(cx)?;

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
                cx.throw_range_error("bad normalization form");
                return Err(-1);
            }
            form => match form.to_string(cx)?.content().to_utf8(false).as_ref() {
                "NFC" => UnicodeNormalizationEnum::NFC,
                "NFD" => UnicodeNormalizationEnum::NFD,
                "NFKC" => UnicodeNormalizationEnum::NFKC,
                "NFKD" => UnicodeNormalizationEnum::NFKD,
                _ => {
                    cx.throw_range_error("bad normalization form");
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

    cx.create_string_from_utf8(ns.into_bytes())
        .map(JSValue::String)
}

pub static js_string_proto_funcs: [FieldDef; 39] = [
    FieldDef::INT32("length", 0, JS_PROP_CONFIGURABLE),
    FieldDef::FUNCTION_V1("charAt", 1, js_string_at, 0),
    FieldDef::FUNCTION_V1("at", 1, js_string_at, 1),
    FieldDef::FUNCTION_V1("charCodeAt", 1, js_string_at, 2),
    FieldDef::FUNCTION_V1("concat", 1, js_string_concat, 0),
    FieldDef::FUNCTION_V1("codePointAt", 1, js_string_codePointAt, 0),
    FieldDef::FUNCTION_V1("isWellFormed", 0, js_string_isWellFormed, 0),
    FieldDef::FUNCTION_V1("toWellFormed", 0, js_string_toWellFormed, 0),
    FieldDef::FUNCTION_V1("indexOf", 1, js_string_indexOf, 0),
    FieldDef::FUNCTION_V1("lastIndexOf", 1, js_string_indexOf, 1),
    FieldDef::FUNCTION_V1("includes", 1, js_string_includes, MAGIC_INCLUDES as _),
    FieldDef::FUNCTION_V1("endsWith", 1, js_string_includes, MAGIC_ENDS_WITH as _),
    FieldDef::FUNCTION_V1("startsWith", 1, js_string_includes, MAGIC_STARTS_WITH as _),
    FieldDef::FUNCTION_V1(
        "match",
        1,
        js_string_match,
        Atom::CONST_Symbol_match.id() as _,
    ),
    FieldDef::FUNCTION_V1(
        "matchAll",
        1,
        js_string_match,
        Atom::CONST_Symbol_matchAll.id() as _,
    ),
    FieldDef::FUNCTION_V1(
        "search",
        1,
        js_string_match,
        Atom::CONST_Symbol_search.id() as _,
    ),
    FieldDef::FUNCTION_V1("split", 2, js_string_split, 0),
    FieldDef::FUNCTION_V1("substring", 2, js_string_substring, 0),
    FieldDef::FUNCTION_V1("substr", 2, js_string_substr, 0),
    FieldDef::FUNCTION_V1("slice", 2, js_string_slice, 0),
    FieldDef::FUNCTION_V1("repeat", 1, js_string_repeat, 0),
    FieldDef::FUNCTION_V1("replace", 2, js_string_replace, 0),
    FieldDef::FUNCTION_V1("replaceAll", 2, js_string_replace, 1),
    FieldDef::FUNCTION_V1("padEnd", 1, js_string_pad, 1),
    FieldDef::FUNCTION_V1("padStart", 1, js_string_pad, 0),
    FieldDef::FUNCTION_V1("trim", 0, js_string_trim, 3),
    FieldDef::FUNCTION_V1("trimEnd", 0, js_string_trim, 2),
    FieldDef::ALIAS("trimRight", "trimEnd"),
    FieldDef::FUNCTION_V1("trimStart", 0, js_string_trim, 1),
    FieldDef::ALIAS("trimLeft", "trimStart"),
    FieldDef::FUNCTION_V1("toString", 0, js_string_toString, 0),
    FieldDef::FUNCTION_V1("valueOf", 0, js_string_toString, 0),
    FieldDef::FUNCTION_V1("localeCompare", 1, js_string_localeCompare, 0),
    FieldDef::FUNCTION_V1("toLowerCase", 0, js_string_toLowerCase, 1),
    FieldDef::FUNCTION_V1("toUpperCase", 0, js_string_toLowerCase, 0),
    FieldDef::FUNCTION_V1("toLocaleLowerCase", 0, js_string_toLowerCase, 1),
    FieldDef::FUNCTION_V1("toLocaleUpperCase", 0, js_string_toLowerCase, 0),
    FieldDef::FUNCTION_V1("normalize", 0, js_string_normalize, 0),
    FieldDef::FUNCTION_V1(
        "[Symbol.iterator]",
        0,
        js_create_array_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) | 4,
    ),
    // ES6 Annex B 2.3.2 etc.
    // JS_CFUNC_MAGIC_DEF("anchor", 1, js_string_CreateHTML, magic_string_anchor),
    // JS_CFUNC_MAGIC_DEF("big", 0, js_string_CreateHTML, magic_string_big),
    // JS_CFUNC_MAGIC_DEF("blink", 0, js_string_CreateHTML, magic_string_blink),
    // JS_CFUNC_MAGIC_DEF("bold", 0, js_string_CreateHTML, magic_string_bold),
    // JS_CFUNC_MAGIC_DEF("fixed", 0, js_string_CreateHTML, magic_string_fixed),
    // JS_CFUNC_MAGIC_DEF("fontcolor", 1, js_string_CreateHTML, magic_string_fontcolor),
    // JS_CFUNC_MAGIC_DEF("fontsize", 1, js_string_CreateHTML, magic_string_fontsize),
    // JS_CFUNC_MAGIC_DEF("italics", 0, js_string_CreateHTML, magic_string_italics),
    // JS_CFUNC_MAGIC_DEF("link", 1, js_string_CreateHTML, magic_string_link),
    // JS_CFUNC_MAGIC_DEF("small", 0, js_string_CreateHTML, magic_string_small),
    // JS_CFUNC_MAGIC_DEF("strike", 0, js_string_CreateHTML, magic_string_strike),
    // JS_CFUNC_MAGIC_DEF("sub", 0, js_string_CreateHTML, magic_string_sub),
    // JS_CFUNC_MAGIC_DEF("sup", 0, js_string_CreateHTML, magic_string_sup),
];

fn js_string_iterator_next(
    ctx: ContextRef,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let cx = ctx.deref();

    if let ObjectPayload::STRING_ITERATOR(it) = cc
        .this()
        .retrive_object_payload(cx, JS_CLASS_STRING_ITERATOR)?
    {
        match it.value() {
            JSValue::UNDEFINED => {
                *state = IteratorState::Done;
                Ok(JSValue::UNDEFINED)
            }
            JSValue::String(ss) => {
                // iterate over string buf to get char or code at `idx`
                if let Some((next_idx, c)) = ss.content().iter_since(it.index() as usize).next() {
                    it.set_index(next_idx as u32);
                    *state = IteratorState::NotDone;
                    cx.create_string_from_char(c).map(JSValue::String)
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

pub static js_string_iterator_proto_funcs: [FieldDef; 2] = [
    FieldDef::ITERATOR_NEXT("next", 0, js_string_iterator_next, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "String Iterator",
        JS_PROP_CONFIGURABLE,
    ),
];

pub fn js_string_fromCharCode(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();
    let mut buf = StringBuf::Empty;

    for a in cc.arguments() {
        let c = a.to_i32(cx)?;
        buf.push_u16(c as u16).map_err(|_| {
            cx.throw_out_of_memory();
            -1
        })?;
    }

    cx.create_string_with_buf(buf).map(JSValue::String)
}

pub fn js_string_fromCodePoint(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let mut sbuf = StringBuf::Empty;

    if cc.length() > 0 {
        sbuf.try_reserve_exact(cc.length()).map_err(|_| {
            cx.throw_out_of_memory();
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
                let d: f64 = a.to_f64(cx)?;
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
                    cx.throw_out_of_memory();
                    -1
                })?;
            } else {
                cx.throw_range_error("invalid code point");
                return Err(-1);
            }
        }
    }

    cx.create_string_with_buf(sbuf).map(JSValue::String)
}

pub fn js_string_raw(ctx: ContextRef, cc: CallContext) -> JSResult {
    // raw(temp,...a)
    let cx = ctx.deref();

    let cooked = cc.arg(0).objectify(cx)?;
    let raw = cooked.get_property(cx, Atom::CONST_raw)?.objectify(cx)?;

    let n = raw.get_length_prop(cx)?;
    let mut sbuf = StringBuf::Empty;

    for i in 0..n {
        let str = raw.get_property_i64(cx, i)?.to_string(cx)?;
        sbuf.push_content(str.content()).map_err(|_| {
            cx.throw_out_of_memory();
            -1
        })?;

        if i < n - 1 && i + 1 < cc.length() as i64 {
            let val = cc.arg(i as usize + 1);
            let s = val.to_string(cx)?;
            sbuf.push_content(s.content()).map_err(|_| {
                cx.throw_out_of_memory();
                -1
            })?;
        }
    }

    cx.create_string_with_buf(sbuf).map(JSValue::String)
}

pub static js_string_funcs: [FieldDef; 3] = [
    FieldDef::FUNCTION_V1("fromCharCode", 1, js_string_fromCharCode, 0),
    FieldDef::FUNCTION_V1("fromCodePoint", 1, js_string_fromCodePoint, 0),
    FieldDef::FUNCTION_V1("raw", 1, js_string_raw, 0),
];
