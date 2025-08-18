use std::ops::DerefMut;

use unicode_normalization::UnicodeNormalization;
use widestring::U16Str;

use crate::{
    JS_ALIAS_DEF, JS_CFUNC_NATIVE_DEF, JS_NATIVE_ITERATOR_NEXT_DEF, JS_PROP_INT32_DEF,
    JS_PROP_STRING_DEF,
    atom::{JSAtom, consts::*},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState, JS_CallConstructor},
    iterator::{JSIteratorKindEnum, js_create_array_iterator},
    object::{JS_PROP_CONFIGURABLE, ObjectDetail, js_get_length64},
    regexp::js_is_regexp,
    string::{
        JS_ConcatString, JS_ConcatString3, JS_STRING_LEN_MAX, JSString, StringBuf, StringChar,
    },
    value::JSValue,
};

const MAGIC_INCLUDES: i32 = 1;
const MAGIC_STARTS_WITH: i32 = 2;
const MAGIC_ENDS_WITH: i32 = 3;

pub fn js_string_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let str_val = if cc.length() == 0 {
        JS_ATOM_empty_string.to_string(ctx)?.into()
    } else {
        if let JSValue::Symbol(p) = cc.arg(0)
            && new_target.is_undefined()
        {
            // "Symbol(desc)"
            JS_ConcatString3(
                ctx,
                c"Symbol(",
                p.atom_info()
                    .unwrap()
                    .atom()
                    .to_string_or_symbol(ctx, true)?,
                c")",
            )?
        } else {
            cc.arg(0).to_string(ctx)?
        }
    };

    if !new_target.is_undefined() {
        // make String object using `new_target` as prototype.
        str_val.to_object(ctx, new_target).map(JSValue::Object)
    } else {
        Ok(str_val.into())
    }
}

/// impl for `charAt()`, `at()`, `chatCodeAt()`
pub fn js_string_at(ctx: &JSContext, cc: CallContext) -> JSResult {
    let str = cc.this().to_string_non_undefined_or_null(ctx)?;
    let what = cc.magic(); // 0: charAt, 1: at, 2: charCodeAt
    let buf_len = str.buf_len() as i32;

    let mut buf_idx: i32 = cc.arg(0).to_i32_saturating(ctx)?;
    if buf_idx < 0 && what == 1 {
        // at() allows minus index
        buf_idx += buf_len;
    }

    if buf_idx < 0 || buf_idx >= buf_len {
        match what {
            0 => JS_ATOM_empty_string.to_string(ctx).map(JSValue::String),
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
            ctx.create_string_with_buf(if (0..=255).contains(&code) {
                StringBuf::Ascii(vec![code as u8])
            } else {
                StringBuf::Utf16(vec![code])
            })
            .map(JSValue::String)
        }
    }
}

pub fn js_string_codePointAt(ctx: &JSContext, cc: CallContext) -> JSResult {
    let s = cc.this().to_string_non_undefined_or_null(ctx)?;
    let buf_idx: i32 = cc.arg(0).to_i32_saturating(ctx)?;
    if buf_idx < 0 || buf_idx >= s.buf_len() as i32 {
        Ok(JSValue::UNDEFINED)
    } else {
        let (_, c) = s.content().iter_since(buf_idx as usize).next().unwrap();
        Ok(JSValue::from(c.to_u32()))
    }
}

pub fn js_string_concat(ctx: &JSContext, cc: CallContext) -> JSResult {
    // TODO: Use more efficient method
    // XXX: should use string_buffer?
    let mut s = cc.this().to_string_non_undefined_or_null(ctx)?;
    for a in cc.arguments() {
        s = JS_ConcatString(ctx, s.into(), a.clone())?;
    }
    Ok(s.into())
}

// return the position of the first invalid character in the string or
// -1 if none
pub(crate) unsafe fn js_string_find_invalid_codepoint(p: &JSString) -> i32 {
    if let StringBuf::Utf16(ws) = p.content().as_ref() {
        U16Str::from_slice(ws)
            .chars()
            .position(|c| c.is_err())
            .map_or(-1, |i| i as i32)
    } else {
        -1
    }
}

pub fn js_string_isWellFormed(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let ret = unsafe { js_string_find_invalid_codepoint(&p) } < 0;
    Ok(JSValue::Bool(ret))
}

pub fn js_string_toWellFormed(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;

    // avoid reallocating the string if it is well-formed
    let i = unsafe { js_string_find_invalid_codepoint(&p) };
    if i < 0 {
        return Ok(p.into());
    }

    if let StringBuf::Utf16(ws) = p.content().as_ref() {
        let mut buf = Vec::<u16>::new();
        buf.try_reserve_exact(ws.len() * 3 / 2).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        for c in U16Str::from_slice(ws).chars_lossy() {
            buf.extend_from_slice(c.encode_utf16(&mut [0; 2]));
        }
        buf.shrink_to_fit();
        ctx.create_string_with_buf(StringBuf::Utf16(buf))
            .map(JSValue::String)
    } else {
        unreachable!()
    }
}

pub fn js_string_indexOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let lastIndexOf = cc.magic();

    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let p1 = cc.arg(0).to_string(ctx)?;

    let len = p.buf_len() as i32;
    let v_len = p1.buf_len() as i32;

    let start: i32;
    let stop: i32;
    let inc: i32;

    if lastIndexOf != 0 {
        let mut pos = len - v_len;
        if cc.length() > 1 {
            let d: f64 = cc.arg(1).coerce_to_f64(ctx)?;
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
            cc.arg(1).to_i32_clamp(ctx, 0, len, 0)?
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

pub fn js_string_replace(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_replace_all = cc.magic() != 0;

    let this = cc.this().clone();
    if this.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let search = cc.arg(0);
    let replace = cc.arg(1);

    if !search.is_undefined_or_null() {
        if is_replace_all {
            if check_regexp_g_flag(ctx, &search) < 0 {
                return Err(-1);
            }
        }

        let replacer = search.get_property(ctx, JS_ATOM_Symbol_replace)?;

        if !replacer.is_undefined_or_null() {
            return replacer.call(ctx, search, &[this, replace.clone()]);
        }
    }

    let str = this.to_string_non_undefined_or_null(ctx)?;
    let buf_len = str.buf_len();

    let search_for = search.to_string(ctx)?;
    let (replace_func, replace_str) = if replace.is_callable() {
        (true, None)
    } else {
        (false, Some(replace.to_string(ctx)?))
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
            str.content()
                .index_of(search_for.content(), endOfLastMatch as _)
        };

        if let Some(pos) = found {
            // calculate replacement
            let replacement = {
                let res = if replace_func {
                    replace
                        .call(
                            ctx,
                            &JSValue::UNDEFINED,
                            &[
                                search_for.clone().into(),
                                JSValue::from(pos as u32),
                                str.clone().into(),
                            ],
                        )
                        .and_then(|v| v.to_string(ctx))
                } else {
                    JSString::get_substitution(
                        ctx,
                        &search_for,
                        &str,
                        pos as usize,
                        &JSValue::UNDEFINED,
                        &JSValue::UNDEFINED,
                        replace_str.as_ref().unwrap(),
                    )
                }?;

                res
            };

            res_string
                .push_content_slice(&str.content().slice(endOfLastMatch as usize..pos as usize))
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            res_string.push(replacement.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            endOfLastMatch = pos + search_for.buf_len();
            is_first = false;

            if !is_replace_all {
                break;
            }
        } else {
            if is_first {
                return Ok(str.into());
            } else {
                break;
            }
        }
    }

    // concat remainder part of string
    if endOfLastMatch < str.buf_len() {
        res_string
            .push_chars_iter(str.content().iter_since(endOfLastMatch).map(|(_, c)| c))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    }

    // create result string
    ctx.create_string_with_buf(res_string).map(JSValue::String)
}

pub fn js_string_split(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    if this.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let separator = cc.arg(0);
    let limit = cc.arg(1);
    let lim = if limit.is_undefined() {
        u32::MAX
    } else {
        limit.coerce_to_u32(ctx)?
    };

    if !separator.is_undefined_or_null() {
        let splitter = separator.get_property(ctx, JS_ATOM_Symbol_split)?;
        if !splitter.is_undefined_or_null() {
            return separator.call(ctx, &separator, &[this.clone(), limit.clone()]);
        }
    }

    let sep = separator.to_string(ctx)?;
    let sep_len = sep.buf_len();

    let str = this.to_string_non_undefined_or_null(ctx)?;
    let str_len = str.buf_len();

    let ret_arr = ctx.create_array()?;
    if lim == 0 {
        return Ok(ret_arr);
    }
    if separator.is_undefined() {
        ret_arr.create_data_property_u32(ctx, 0, cc.this().clone(), 0)?;
        return Ok(ret_arr);
    }

    if sep_len == 0 {
        for (i, (_, c)) in str.content().iter().enumerate().take(lim as usize) {
            let t = ctx.create_string_from_char(c)?;
            ret_arr.create_data_property_u32(ctx, i as _, t.into(), 0)?;
        }
    } else {
        let mut remainder: Option<usize> = Some(0);
        let mut cursor = 0;
        let mut count = 0;

        while let Some(i) = str.content().index_of(sep.content(), cursor) {
            let t = str.sub_string(ctx, cursor..cursor + sep_len)?.into();
            ret_arr.create_data_property_u32(ctx, count as _, t, 0)?;

            count += 1;
            cursor = i + sep_len;

            match (cursor + sep_len).cmp(&str_len) {
                std::cmp::Ordering::Greater => {
                    // no more separator found, break
                    break;
                }
                std::cmp::Ordering::Equal => {
                    // reached the end of the string, and is seperator, no remainder
                    remainder = None;
                    break;
                }
                _ => {
                    remainder = Some(cursor);
                }
            }
        }

        if let Some(i) = remainder {
            // remaining part
            let t = str.sub_string(ctx, i..str_len)?.into();
            ret_arr.create_data_property_u32(ctx, count as _, t, 0)?;
        }
    }

    Ok(ret_arr)
}

pub fn js_string_substring(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;

    let len = p.buf_len() as i32;
    let a = cc.arg(0).to_i32_clamp(ctx, 0, len, 0)?;
    let b = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(ctx, 0, len, 0)?
    } else {
        len
    };
    let (start, end) = if a < b { (a, b) } else { (b, a) };

    p.sub_string(ctx, start as usize..end as usize)
        .map(JSValue::String)
}

pub fn js_string_substr(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let len = p.buf_len() as i32;

    let a: i32 = cc.arg(0).to_i32_clamp(ctx, 0, len, len)?;
    let n = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(ctx, 0, len - a, 0)?
    } else {
        len - a
    };

    p.sub_string(ctx, a as usize..(a + n) as usize)
        .map(JSValue::String)
}

pub fn js_string_slice(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let len = p.buf_len() as i32;
    let start = cc.arg(0).to_i32_clamp(ctx, 0, len, len)?;
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    p.sub_string(ctx, start as usize..std::cmp::max(end, start) as usize)
        .map(JSValue::String)
}

pub fn js_string_pad(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_pad_end = cc.magic() != 0;

    // don't change parse args order.
    let str = cc.this().to_string_non_undefined_or_null(ctx)?;
    let len = str.buf_len() as i32;

    let mut ttl_len: i32 = cc.arg(0).to_i32_saturating(ctx)?;
    if len >= ttl_len {
        return Ok(str.into());
    }
    ttl_len -= len;

    let mut fill_str: Option<JSString> = None;
    let mut fill_u16 = StringChar::Char(' ');

    if cc.length() > 1 && !cc.arg(1).is_undefined() {
        let s = cc.arg(1).to_string(ctx)?;
        if s.is_empty() {
            return Ok(str.into());
        }

        if s.buf_len() == 1 {
            fill_u16 = StringChar::Code(s.content().get(0).unwrap());
        } else {
            fill_str.replace(s);
        }
    }

    if ttl_len > JS_STRING_LEN_MAX as i32 {
        ctx.throw_internal_error("string too long");
        return Err(-1);
    }

    let mut sbuf = if is_pad_end {
        str.content().copy_to_new().map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?
    } else {
        StringBuf::Empty
    };

    if let Some(s) = fill_str {
        // fill with str
        let src = std::iter::repeat_n(
            s.content().iter_codepoint(),
            (ttl_len as usize).div_ceil(s.buf_len()),
        )
        .flatten()
        .take(ttl_len as usize);

        sbuf.push_u16_iter(src)
    } else {
        // fill with char
        sbuf.push_chars_iter(std::iter::repeat_n(fill_u16, ttl_len as usize))
    }
    .map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    if !is_pad_end {
        sbuf.push(str.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    println!(
        "[O.o] padEnd sbuf: {:?}",
        sbuf.iter_codepoint().collect::<Vec<_>>()
    );

    ctx.create_string_with_buf(sbuf).map(JSValue::String)
}

pub fn js_string_repeat(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;

    let repeat: i64 = cc.arg(0).coerce_to_i64_saturating(ctx)?;
    if !(0..=2147483647).contains(&repeat) {
        ctx.throw_range_error("invalid repeat count");
        return Err(-1);
    }

    let n = repeat as usize;
    let len = p.buf_len();
    if len == 0 || n == 1 {
        return Ok(p.into());
    }
    if repeat as usize * len > JS_STRING_LEN_MAX {
        ctx.throw_internal_error("string too long");
        return Err(-1);
    }

    let mut res_string = StringBuf::with_capacity(n * len, p.is_wide()).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    res_string
        .push_chars_iter(std::iter::repeat_n(p.content().iter().map(|(_, c)| c), n).flatten())
        .unwrap();
    res_string.shrink_to_fit();

    ctx.create_string_with_buf(res_string).map(JSValue::String)
}

pub fn js_string_includes(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;

    if js_is_regexp(ctx, cc.arg(0))? {
        ctx.throw_type_error("regex not supported");
        return Err(-1);
    }

    let pat = cc.arg(0).to_string(ctx)?;

    let mut str_len = p.buf_len();
    let pat_len = pat.buf_len();

    let mut pos = if cc.magic() == MAGIC_ENDS_WITH {
        str_len
    } else {
        0
    };
    if cc.length() > 1 {
        let a = cc.arg(1);
        if !a.is_undefined() {
            pos = a.to_i32_clamp(ctx, 0, str_len as _, 0)? as usize;
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
                .index_of(&pat.content(), pos)
                .map_or(JSValue::Bool(false), |_| JSValue::Bool(true))),

            m @ (MAGIC_STARTS_WITH | MAGIC_ENDS_WITH) => {
                let it = p.content().iter_range(if m == MAGIC_ENDS_WITH {
                    (str_len - pat_len)..str_len
                } else {
                    0..pos
                });
                Ok(it
                    .zip(pat.content().iter())
                    .all(|((_, c1), (_, c2))| c1 == c2)
                    .then(|| JSValue::Bool(true))
                    .unwrap_or(JSValue::Bool(false)))
            }

            _ => unreachable!(),
        }
    }
}

fn js_string_localeCompare(ctx: &JSContext, cc: CallContext) -> JSResult {
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

pub fn check_regexp_g_flag(ctx: &JSContext, regexp: &JSValue) -> i32 {
    let ret = if let Ok(b) = js_is_regexp(ctx, &regexp) {
        b
    } else {
        return -1;
    };

    if ret {
        let flags = if let Ok(x) = regexp.get_property(ctx, JS_ATOM_flags) {
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

pub fn js_string_match(ctx: &JSContext, cc: CallContext) -> JSResult {
    let magic_method = JSAtom(cc.magic() as u32); // match, matchAll, search
    let this = cc.this();

    if this.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let regexp = cc.arg(0);

    if !regexp.is_undefined_or_null() {
        let matcher = regexp.get_property(ctx, magic_method)?;
        if magic_method == JS_ATOM_Symbol_matchAll {
            if check_regexp_g_flag(ctx, regexp) < 0 {
                return Err(-1);
            }
        }
        if !matcher.is_undefined_or_null() {
            return matcher.call(ctx, regexp, std::slice::from_ref(this));
        }
    }

    let str = this.to_string_non_undefined_or_null(ctx)?;

    // make RegExp object
    let mut args_len = 1;
    let mut args = [regexp.clone(), JSValue::UNDEFINED];
    if magic_method == JS_ATOM_Symbol_matchAll {
        args[args_len as usize] = ctx.create_string_from_char('g'.into())?.into();
        args_len += 1;
    }

    let re_obj = {
        let ctor = ctx.0.borrow().regexp_ctor.clone(); // avoid borrow for long
        JS_CallConstructor(ctx, &ctor, &args[0..args_len]).err()?
    };
    re_obj.invoke(ctx, magic_method, &[str.into()])
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
#[deprecated(note = "use JSString::get_substitution() instead")]
pub fn js_string___GetSubstitution(ctx: &JSContext, cc: CallContext) -> JSResult {
    let matched = cc.arg(0);
    let str = cc.arg(1);
    let position = cc.arg(2).coerce_to_u32(ctx)?;
    let captures = cc.arg(3);
    let namedCaptures = cc.arg(4);
    let replacement = cc.arg(5);

    let sp = unsafe { str.string_unchecked() };
    let rp = unsafe { replacement.string_unchecked() };

    if !replacement.is_string() || !str.is_string() {
        ctx.throw_type_error("not a string");
        return Err(-1);
    }

    JSString::get_substitution(
        ctx,
        unsafe { matched.string_unchecked() },
        sp,
        position as _,
        captures,
        namedCaptures,
        rp,
    )
    .map(JSValue::String)
}

pub fn js_string_trim(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    let mut s = p.content().copy_to_new().map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    s.trim(cc.magic() & 1 != 0, cc.magic() & 2 != 0);
    ctx.create_string_with_buf(s).map(JSValue::String)
}

pub fn string_cmp(p1: &JSString, p2: &JSString, x1: usize, x2: usize, len: usize) -> i32 {
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

#[deprecated(note = "use StringBuf::find_char() instead")]
pub unsafe fn string_indexof_char(p: *mut JSString, c: i32, from: i32) -> i32 {
    (*p).content()
        .index_of_char(char::from_u32(c as u32).unwrap(), from as _)
        .map_or(-1, |i| i as i32)

    // let len = (*p).len() as i32;
    // if (0..len).contains(&from) {
    //     let mut it = (*p).content().chars().enumerate().skip(from as _);

    //     if (*p).is_wide_char() || c & !0xff == 0 {
    //         it.find(|(_, ch)| (*ch) as i32 == c)
    //             .map(|(i, _)| i as i32)
    //             .unwrap_or(-1)
    //     } else {
    //         -1
    //     }
    // } else {
    //     -1
    // }
}

#[deprecated(note = "use StringBuf::find() instead")]
pub unsafe fn string_indexof(p1: *mut JSString, p2: *mut JSString, from: i32) -> i32 {
    debug_assert!(from >= 0);
    (*p1)
        .content()
        .index_of((*p2).content(), from as _)
        .map_or(-1, |i| i as i32)
}

/* also used for String.prototype.valueOf */
pub fn js_string_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    js_thisStringValue(ctx, cc.this().clone())
}

pub fn js_thisStringValue(ctx: &JSContext, this_val: JSValue) -> JSResult {
    if this_val.is_string() {
        Ok(this_val)
    } else if let Some(d) = this_val
        .object()
        .and_then(|p| p.0.payload.borrow().value())
        .and_then(|x| if x.is_string() { Some(x) } else { None })
    {
        Ok(d)
    } else {
        ctx.throw_type_error("not a string");
        Err(-1)
    }
}

pub fn js_string_toLowerCase(ctx: &JSContext, cc: CallContext) -> JSResult {
    let to_lower = cc.magic();

    let p = cc.this().to_string_non_undefined_or_null(ctx)?;
    if p.is_empty() {
        return Ok(p.into());
    }

    let sc = match p.content().as_ref() {
        StringBuf::Empty | StringBuf::Null => unreachable!(),
        StringBuf::Ascii(s) => {
            let mut d = Vec::<u8>::new();
            d.try_reserve_exact(s.len()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            d.extend_from_slice(s);
            if to_lower != 0 {
                d.make_ascii_lowercase();
            } else {
                d.make_ascii_uppercase();
            }
            StringBuf::Ascii(d)
        }
        StringBuf::Utf16(s) => {
            let mut w = Vec::<u16>::new();
            w.try_reserve(s.len() * 3 / 2).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            let it: Box<dyn Iterator<Item = char>> = if to_lower != 0 {
                Box::new(
                    U16Str::from_slice(s)
                        .chars_lossy()
                        .flat_map(|c| c.to_lowercase()),
                )
            } else {
                Box::new(
                    U16Str::from_slice(s)
                        .chars_lossy()
                        .flat_map(|c| c.to_uppercase()),
                )
            };
            for c in it {
                w.extend_from_slice(c.encode_utf16(&mut [0; 2]));
            }

            w.shrink_to_fit();
            StringBuf::Utf16(w)
        }
    };

    ctx.create_string_with_buf(sc).map(JSValue::String)
}

fn js_string_normalize(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = cc.this().to_string_non_undefined_or_null(ctx)?;

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
            form => match form.to_string(ctx)?.content().to_utf8().as_ref() {
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
        .map(JSValue::String)
}

pub static js_string_proto_funcs: [FieldDef; 39] = [
    JS_PROP_INT32_DEF!("length", 0, JS_PROP_CONFIGURABLE),
    JS_CFUNC_NATIVE_DEF!("charAt", 1, js_string_at, 0),
    JS_CFUNC_NATIVE_DEF!("at", 1, js_string_at, 1),
    JS_CFUNC_NATIVE_DEF!("charCodeAt", 1, js_string_at, 2),
    JS_CFUNC_NATIVE_DEF!("concat", 1, js_string_concat, 0),
    JS_CFUNC_NATIVE_DEF!("codePointAt", 1, js_string_codePointAt, 0),
    JS_CFUNC_NATIVE_DEF!("isWellFormed", 0, js_string_isWellFormed, 0),
    JS_CFUNC_NATIVE_DEF!("toWellFormed", 0, js_string_toWellFormed, 0),
    JS_CFUNC_NATIVE_DEF!("indexOf", 1, js_string_indexOf, 0),
    JS_CFUNC_NATIVE_DEF!("lastIndexOf", 1, js_string_indexOf, 1),
    JS_CFUNC_NATIVE_DEF!("includes", 1, js_string_includes, MAGIC_INCLUDES as _),
    JS_CFUNC_NATIVE_DEF!("endsWith", 1, js_string_includes, MAGIC_ENDS_WITH as _),
    JS_CFUNC_NATIVE_DEF!("startsWith", 1, js_string_includes, MAGIC_STARTS_WITH as _),
    JS_CFUNC_NATIVE_DEF!("match", 1, js_string_match, JS_ATOM_Symbol_match.0 as _),
    JS_CFUNC_NATIVE_DEF!(
        "matchAll",
        1,
        js_string_match,
        JS_ATOM_Symbol_matchAll.0 as _
    ),
    JS_CFUNC_NATIVE_DEF!("search", 1, js_string_match, JS_ATOM_Symbol_search.0 as _),
    JS_CFUNC_NATIVE_DEF!("split", 2, js_string_split, 0),
    JS_CFUNC_NATIVE_DEF!("substring", 2, js_string_substring, 0),
    JS_CFUNC_NATIVE_DEF!("substr", 2, js_string_substr, 0),
    JS_CFUNC_NATIVE_DEF!("slice", 2, js_string_slice, 0),
    JS_CFUNC_NATIVE_DEF!("repeat", 1, js_string_repeat, 0),
    JS_CFUNC_NATIVE_DEF!("replace", 2, js_string_replace, 0),
    JS_CFUNC_NATIVE_DEF!("replaceAll", 2, js_string_replace, 1),
    JS_CFUNC_NATIVE_DEF!("padEnd", 1, js_string_pad, 1),
    JS_CFUNC_NATIVE_DEF!("padStart", 1, js_string_pad, 0),
    JS_CFUNC_NATIVE_DEF!("trim", 0, js_string_trim, 3),
    JS_CFUNC_NATIVE_DEF!("trimEnd", 0, js_string_trim, 2),
    JS_ALIAS_DEF!("trimRight", "trimEnd"),
    JS_CFUNC_NATIVE_DEF!("trimStart", 0, js_string_trim, 1),
    JS_ALIAS_DEF!("trimLeft", "trimStart"),
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_string_toString, 0),
    JS_CFUNC_NATIVE_DEF!("valueOf", 0, js_string_toString, 0),
    JS_CFUNC_NATIVE_DEF!("localeCompare", 1, js_string_localeCompare, 0),
    JS_CFUNC_NATIVE_DEF!("toLowerCase", 0, js_string_toLowerCase, 1),
    JS_CFUNC_NATIVE_DEF!("toUpperCase", 0, js_string_toLowerCase, 0),
    JS_CFUNC_NATIVE_DEF!("toLocaleLowerCase", 0, js_string_toLowerCase, 1),
    JS_CFUNC_NATIVE_DEF!("toLocaleUpperCase", 0, js_string_toLowerCase, 0),
    JS_CFUNC_NATIVE_DEF!("normalize", 0, js_string_normalize, 0),
    JS_CFUNC_NATIVE_DEF!(
        "[Symbol.iterator]",
        0,
        js_create_array_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) | 4
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

pub fn js_string_iterator_next(
    ctx: &JSContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let mut isa = unsafe { cc.this().object_unchecked().0.payload.borrow_mut() };

    if let ObjectDetail::STRING_ITERATOR(it) = isa.deref_mut() {
        let ss = unsafe { it.obj.string_unchecked() };

        // iterate over string buf to get char or code at `idx`
        if let Some((cursor, c)) = ss.content().iter_since(it.idx as usize).next() {
            it.idx += cursor as u32;
            *state = IteratorState::NotDone;
            match c {
                StringChar::Char(_) => ctx.create_string_from_char(c),
                StringChar::Code(c) => ctx.create_string_with_buf(if (0..=255).contains(&c) {
                    StringBuf::Ascii(vec![c as u8])
                } else {
                    StringBuf::Utf16(vec![c])
                }),
            }
            .map(JSValue::String)
        } else {
            it.obj = JSValue::UNDEFINED;
            *state = IteratorState::Done;
            Ok(JSValue::UNDEFINED)
        }
    } else {
        Err(-1)
    }
}

pub static js_string_iterator_proto_funcs: [FieldDef; 2] = [
    JS_NATIVE_ITERATOR_NEXT_DEF!("next", 0, js_string_iterator_next, 0),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "String Iterator",
        JS_PROP_CONFIGURABLE
    ),
];

pub fn js_string_fromCharCode(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut res_string = StringBuf::Empty;

    for a in cc.arguments() {
        let c = a.to_i32(ctx)?;
        res_string.push_u16(c as u16).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    ctx.create_string_with_buf(res_string).map(JSValue::String)
}

pub fn js_string_fromCodePoint(ctx: &JSContext, cc: CallContext) -> JSResult {
    // XXX: could pre-compute string length if all arguments are JS_TAG_INT
    let mut res_string = StringBuf::Empty;

    let n = cc.length();
    if n > 0 {
        res_string
            .try_reserve_exact(cc.length() as usize)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        for a in cc.arguments() {
            let code: i32 = if let JSValue::Int(i) = a {
                *i
            } else {
                let d: f64 = a.coerce_to_f64(ctx)?;
                if d.fract() == 0.0 {
                    let n = d.trunc() as i64;
                    if n < 0 || n > 0x10ffff {
                        -1 // bad code
                    } else {
                        n as i32
                    }
                } else {
                    -1 // bad code
                }
            };

            if let Some(c) = char::from_u32(code as u32) {
                res_string.push_char(c).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            } else {
                ctx.throw_range_error("invalid code point");
                return Err(-1);
            }
        }
    }

    res_string.shrink_to_fit();
    ctx.create_string_with_buf(res_string).map(JSValue::String)
}

pub fn js_string_raw(ctx: &JSContext, cc: CallContext) -> JSResult {
    // raw(temp,...a)
    let cooked = cc.arg(0).to_object(ctx)?;
    let raw = cooked.get_property(ctx, JS_ATOM_raw)?.to_object(ctx)?;

    let n = js_get_length64(ctx, &raw.clone().into())?;
    let mut res_string = StringBuf::Empty;

    for i in 0..n {
        let str = raw.get_property_i64(ctx, i)?.to_string(ctx)?;
        res_string.push(str.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        if i < n - 1 && i + 1 < cc.length() as i64 {
            let val = cc.arg(i as usize + 1);
            let s = val.to_string(ctx)?;
            res_string.push(s.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
    }

    ctx.create_string_with_buf(res_string).map(JSValue::String)
}

pub const js_string_funcs: [FieldDef; 3] = [
    JS_CFUNC_NATIVE_DEF!("fromCharCode", 1, js_string_fromCharCode, 0),
    JS_CFUNC_NATIVE_DEF!("fromCodePoint", 1, js_string_fromCodePoint, 0),
    JS_CFUNC_NATIVE_DEF!("raw", 1, js_string_raw, 0),
];
