use std::ops::DerefMut;

use {fallible_collections::FallibleVec, widestring::U16Str};

use crate::{
    JS_ALIAS_DEF, JS_CFUNC_NATIVE_DEF, JS_NATIVE_ITERATOR_NEXT_DEF, JS_PROP_INT32_DEF,
    JS_PROP_STRING_DEF,
    atom::{JSAtom, consts::*},
    context::JSContext,
    error::{JS_ThrowRangeError, JSResult},
    function::{CallContext, IteratorState, JS_Call, JS_CallConstructor, JS_Invoke, NativeField},
    iterator::{JSIteratorKindEnum, js_create_array_iterator},
    object::{JS_PROP_CONFIGURABLE, ObjectDetail, js_get_length64},
    regexp::js_is_regexp,
    string::{
        JS_ConcatString, JS_ConcatString3, JS_STRING_LEN_MAX, JS_ToStringCheckObject, JSString,
        StringBuf, StringChar,
    },
    value::{JSValue, JSValueConst},
};

pub fn js_string_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let str_val: JSValue = if cc.length() == 0 {
        JS_ATOM_empty_string.to_string_or_symbol(ctx, true)?
    } else {
        if new_target.is_undefined() && cc.arg(0).is_symbol() {
            if let JSValue::Symbol(p) = cc.arg(0) {
                JS_ConcatString3(
                    ctx,
                    c"Symbol(",
                    p.atom_info()
                        .unwrap()
                        .atom()
                        .to_string_or_symbol(ctx, true)?,
                    c")",
                )?
                .into()
            } else {
                unreachable!()
            }
        } else {
            cc.arg(0).coerce_to_string(ctx)?.into()
        }
    };

    if !new_target.is_undefined() {
        ctx.create_object_apply_ctor(new_target, ObjectDetail::STRING(str_val))
            .map(JSValue::Object)
            .map_err(|_| -1)
    } else {
        Ok(str_val)
    }
}

/// impl for `charAt()`, `at()`, `chatCodeAt()`
pub fn js_string_at(ctx: &JSContext, cc: CallContext) -> JSResult {
    let str = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    let what = cc.magic(); // 0: charAt, 1: at, 2: charCodeAt

    let mut idx: i32 = cc.arg(0).coerce_to_i32_saturating(ctx)?;
    if idx < 0 && what == 1 {
        // at() allows minus index
        idx += str.len() as i32;
    }

    if idx < 0 || idx >= str.len() as i32 {
        match what {
            0 => JS_ATOM_empty_string.to_string(ctx).map(JSValue::String),
            1 => Ok(JSValue::UNDEFINED),
            2 => Ok(JSValue::Float64(f64::NAN)),
            _ => unreachable!(),
        }
    } else {
        let code = str.get(idx as usize).unwrap();
        if what == 2 {
            // charCodeAt() returns int
            Ok(JSValue::Int(code as i32))
        } else {
            // charAt(), at() returns string
            ctx.create_string_from_content(if (0..=255).contains(&code) {
                StringBuf::Utf8(vec![code as u8])
            } else {
                StringBuf::Utf16(vec![code])
            })
            .map(JSValue::String)
        }
    }
}

#[deprecated(note = "use js_string_at() instead")]
fn js_string_charCodeAt(ctx: &JSContext, cc: CallContext) -> JSResult {
    let idx: i32 = cc.arg(0).coerce_to_i32_saturating(ctx)?;
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    if idx < 0 || idx >= p.len() as i32 {
        Ok(JSValue::Float64(f64::NAN))
    } else {
        let c = p.get(idx as usize).unwrap();
        Ok(JSValue::Int(c as i32))
    }
}

/// impl for `charAt()`, `at()`
#[deprecated(note = "use js_string_at() instead")]
fn js_string_charAt(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_at = cc.magic();

    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;

    let mut idx: i32 = cc.arg(0).coerce_to_i32_saturating(ctx)?;
    if idx < 0 && is_at != 0 {
        idx += p.len() as i32;
    }

    if idx < 0 || idx >= p.len() as i32 {
        if is_at != 0 {
            Ok(JSValue::UNDEFINED)
        } else {
            ctx.create_string("").map(JSValue::String)
        }
    } else {
        let code = p.get(idx as usize).unwrap();
        ctx.create_string_from_content(if (0..=255).contains(&code) {
            StringBuf::Utf8(vec![code as u8])
        } else {
            StringBuf::Utf16(vec![code])
        })
        .map(JSValue::String)
    }
}

pub fn js_string_codePointAt(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;

    let idx: i32 = cc.arg(0).coerce_to_i32_saturating(ctx)?;
    if idx < 0 || idx >= p.len() as i32 {
        Ok(JSValue::UNDEFINED)
    } else {
        let c = p.content().chars().nth(idx as usize).unwrap();
        Ok(JSValue::Int(c as u32 as i32))
    }
}

pub fn js_string_concat(ctx: &JSContext, cc: CallContext) -> JSResult {
    // TODO: Use more efficient method
    // XXX: should use string_buffer?
    let mut r = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    for a in cc.arguments() {
        r = JS_ConcatString(ctx, r.into(), a.clone())?;
    }
    Ok(r.into())
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
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    let ret = unsafe { js_string_find_invalid_codepoint(&p) } < 0;
    Ok(JSValue::Bool(ret))
}

pub fn js_string_toWellFormed(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;

    // avoid reallocating the string if it is well-formed
    let i = unsafe { js_string_find_invalid_codepoint(&p) };
    if i < 0 {
        return Ok(p.into());
    }

    if let StringBuf::Utf16(ws) = p.content().as_ref() {
        if let Ok(mut buf) = Vec::<u16>::try_with_capacity(ws.len() * 3 / 2) {
            for c in U16Str::from_slice(ws).chars_lossy() {
                buf.extend_from_slice(c.encode_utf16(&mut [0; 2]));
            }
            buf.shrink_to_fit();
            ctx.create_string_from_content(StringBuf::Utf16(buf))
                .map(JSValue::String)
        } else {
            Ok(JSValue::EXCEPTION)
        }
    } else {
        unreachable!()
    }
}

pub fn js_string_indexOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let lastIndexOf = cc.magic();

    let p = cc.this().coerce_to_string(ctx)?;
    let p1 = cc.arg(0).coerce_to_string(ctx)?;

    let len = p.len() as i32;
    let v_len = p1.len() as i32;

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
            cc.arg(1).coerce_to_i32_clamp(ctx, 0, len, 0)?
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
            if string_cmp(&p, &p1, i, 0, v_len) == 0 {
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
            return JS_Call(ctx, &replacer, search, &[this, replace.clone()]).check_exception();
        }
    }

    let str = this.coerce_to_string(ctx)?;
    let search_str = search.coerce_to_string(ctx)?;

    let replace_func = replace.is_callable();
    let replace_str = if !replace_func {
        Some(replace.coerce_to_string(ctx)?)
    } else {
        None
    };

    let sp = str;
    let searchp = search_str;

    let mut is_first = true;
    let mut endOfLastMatch = 0_usize;
    let mut res_string = StringBuf::Empty;

    loop {
        let found = if searchp.is_empty() {
            if is_first {
                Some(0)
            } else if endOfLastMatch >= sp.len() {
                None
            } else {
                Some(endOfLastMatch + 1)
            }
        } else {
            sp.content().find(searchp.content(), endOfLastMatch as _)
        };

        if let Some(pos) = found {
            // calculate replacement
            let replacement = {
                let res = if replace_func {
                    JS_Call(
                        ctx,
                        replace,
                        &JSValue::UNDEFINED,
                        &[
                            searchp.clone().into(),
                            JSValue::from(pos as u32),
                            sp.clone().into(),
                        ],
                    )
                    .check_exception()
                    .and_then(|v| v.coerce_to_string(ctx))
                } else {
                    JSString::get_substitution(
                        ctx,
                        &searchp,
                        &sp,
                        pos as usize,
                        &JSValue::UNDEFINED,
                        &JSValue::UNDEFINED,
                        replace_str.as_ref().unwrap(),
                    )
                }?;

                res
            };

            res_string
                .push_content_slice(&sp.content().slice(endOfLastMatch as usize..pos as usize))
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            res_string.push(replacement.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            endOfLastMatch = pos + searchp.len();
            is_first = false;

            if !is_replace_all {
                break;
            }
        } else {
            if is_first {
                return Ok(sp.into());
            } else {
                break;
            }
        }
    }

    // concat remainder part of `str`
    let rem_len = sp.len() - endOfLastMatch as usize;
    if rem_len > 0 {
        res_string
            .push_chars_iter(
                sp.content()
                    .iter()
                    .skip(endOfLastMatch as usize)
                    .take(rem_len),
            )
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    }

    // create result string
    ctx.create_string_from_content(res_string)
        .map(JSValue::String)
}

pub fn js_string_split(ctx: &JSContext, cc: CallContext) -> JSResult {
    // split(sep, limit)
    let O = cc.this().clone();

    if O.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    let separator = cc.arg(0);
    let limit = cc.arg(1);
    let mut args = [const { JSValue::UNDEFINED }; 2];
    let mut lengthA: u32 = 0;

    if !separator.is_undefined_or_null() {
        let splitter = separator.get_property(ctx, JS_ATOM_Symbol_split)?;
        if !splitter.is_undefined_or_null() {
            args[0] = O;
            args[1] = limit.clone();
            return JS_Call(ctx, &splitter, &separator, &args).check_exception();
        }
    }

    let sp = O.coerce_to_string(ctx)?;
    let s = sp.len() as i32;

    let A = ctx.create_array()?;
    let mut lengthA = 0;

    let lim = if limit.is_undefined() {
        u32::MAX
    } else {
        limit.coerce_to_u32(ctx)?
    };

    let rp = separator.coerce_to_string(ctx)?;
    let r = rp.len() as i32;

    if lim == 0 {
        return Ok(A);
    }

    let mut T = JSValue::UNDEFINED;
    let mut p = 0;

    macro_rules! add_tail {
        () => {
            T = sp.sub_string(ctx, p as usize, s as usize)?.into();
            A.create_data_property_u32(ctx, lengthA as _, T, 0)?;
            return Ok(A);
        };
    }

    if separator.is_undefined() {
        add_tail!();
    }

    if s == 0 {
        if r != 0 {
            add_tail!();
        }
        return Ok(A);
    }

    let mut q = p;
    while {
        q += if r == 0 { 1 } else { 0 };
        q
    } <= s - r - if r == 0 { 1 } else { 0 }
    {
        let e = if let Some(i) = sp.content().find(rp.content(), q as _) {
            i as i32
        } else {
            break;
        };

        T = sp.sub_string(ctx, p as usize, e as usize)?.into();
        A.create_data_property_u32(ctx, lengthA as _, T, 0)?;

        lengthA += 1;
        if lengthA == lim {
            return Ok(A);
        }

        p = e + r;
        q = p;
    }

    add_tail!();
}

pub fn js_string_substring(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;

    let len = p.len() as i32;
    let a = cc.arg(0).coerce_to_i32_clamp(ctx, 0, len, 0)?;
    let b = if !cc.arg(1).is_undefined() {
        cc.arg(1).coerce_to_i32_clamp(ctx, 0, len, 0)?
    } else {
        len
    };
    let (start, end) = if a < b { (a, b) } else { (b, a) };

    p.sub_string(ctx, start as usize, end as usize)
        .map(JSValue::String)
}

pub fn js_string_substr(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    let len = p.len() as i32;

    let a: i32 = cc.arg(0).coerce_to_i32_clamp(ctx, 0, len, len)?;
    let n = if !cc.arg(1).is_undefined() {
        cc.arg(1).coerce_to_i32_clamp(ctx, 0, len - a, 0)?
    } else {
        len - a
    };

    p.sub_string(ctx, a as usize, (a + n) as usize)
        .map(JSValue::String)
}

pub fn js_string_slice(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    let len = p.len() as i32;
    let start = cc.arg(0).coerce_to_i32_clamp(ctx, 0, len, len)?;
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).coerce_to_i32_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    p.sub_string(ctx, start as usize, std::cmp::max(end, start) as usize)
        .map(JSValue::String)
}

pub fn js_string_pad(ctx: &JSContext, cc: CallContext) -> JSResult {
    let padEnd = cc.magic();
    let mut n: i32 = cc.arg(0).coerce_to_i32_saturating(ctx)?;

    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    let len = p.len() as i32;
    if len >= n {
        return Ok(p.into());
    }

    let mut p1: Option<JSString> = None;
    let mut c = StringChar::Char(' ');

    if cc.length() > 1 && !cc.arg(1).is_undefined() {
        let s = cc.arg(1).coerce_to_string(ctx)?;
        if s.is_empty() {
            return Ok(p.into());
        } else if s.len() == 1 {
            c = s.content().iter().next().unwrap();
        } else {
            p1.replace(s);
        }
    }

    if n > JS_STRING_LEN_MAX as i32 {
        ctx.throw_internal_error("string too long");
        return Err(-1);
    }

    let mut res_string = if padEnd != 0 {
        p.content().copy_to_new().map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?
    } else {
        StringBuf::Empty
    };

    n -= len;

    if let Some(s) = p1 {
        let src = std::iter::repeat_n(s.content().iter(), (n as usize).div_ceil(s.len())).flatten();
        res_string.push_chars_iter(src)
        // res_string.push_chars_iter(
        //     std::iter::repeat_n(s.content().chars(), (n as usize).div_ceil(s.len())).flatten(),
        // )
    } else {
        res_string.push_chars_iter(std::iter::repeat_n(c, n as usize))
        // res_string.push_chars_iter(std::iter::repeat_n(c, n as usize))
    }
    .map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    println!("[O.o] {res_string:?}");

    if padEnd == 0 {
        res_string.push(p.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    ctx.create_string_from_content(res_string)
        .map(JSValue::String)
}

pub fn js_string_repeat(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;

    let repeat: i64 = cc.arg(0).coerce_to_i64_saturating(ctx)?;
    if !(0..=2147483647).contains(&repeat) {
        JS_ThrowRangeError(ctx, "invalid repeat count");
        return Err(-1);
    }

    let n = repeat as usize;
    let len = p.len();
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
        .push_chars_iter(std::iter::repeat_n(p.content().iter(), n).flatten())
        .unwrap();
    res_string.shrink_to_fit();

    ctx.create_string_from_content(res_string)
        .map(JSValue::String)
}

pub fn js_string_includes(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;

    if js_is_regexp(ctx, cc.arg(0))? {
        ctx.throw_type_error("regex not supported");
        return Err(-1);
    }
    let p1 = cc.arg(0).coerce_to_string(ctx)?;

    let mut len = p.len() as i32;
    let v_len = p1.len() as i32;

    let mut pos = if cc.magic() == 2 {
        len
    } else if cc.length() > 1 && !cc.arg(1).is_undefined() {
        cc.arg(1).coerce_to_i32_clamp(ctx, 0, len, 0)?
    } else {
        0
    };

    len -= v_len;

    let mut ret = false;
    let start: i32;
    let stop: i32;

    if cc.magic() == 0 {
        start = pos;
        stop = len;
    } else {
        if cc.magic() == 1 {
            if pos > len {
                return Ok(JSValue::Bool(ret));
            }
        } else {
            pos -= v_len;
        }
        start = pos;
        stop = pos;
    }

    if start >= 0 && start <= stop {
        for i in start..stop {
            if string_cmp(&p, &p1, i, 0, v_len) == 0 {
                ret = true;
                break;
            }
        }
    }

    Ok(JSValue::Bool(ret))
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

        let flags = if let Ok(s) = flags.coerce_to_string(ctx) {
            s
        } else {
            return -1;
        };

        let idx = flags.content().find_char('g', 0);
        if idx.is_none() {
            ctx.throw_type_error("regexp must have the 'g' flag");
            return -1;
        }
    }

    0
}

pub fn js_string_match(ctx: &JSContext, cc: CallContext) -> JSResult {
    let O = cc.this().clone();
    let regexp = cc.arg(0);
    let method = JSAtom(cc.magic() as u32);

    if O.is_undefined_or_null() {
        ctx.throw_type_error("cannot convert to object");
        return Err(-1);
    }

    if !regexp.is_undefined_or_null() {
        let matcher = regexp.get_property(ctx, method)?;

        if method == JS_ATOM_Symbol_matchAll {
            if check_regexp_g_flag(ctx, regexp) < 0 {
                return Err(-1);
            }
        }
        if !matcher.is_undefined_or_null() {
            return JS_Call(ctx, &matcher, regexp, &[O]).check_exception();
        }
    }

    let S = O.coerce_to_string(ctx)?;

    // make RegExp args
    let mut args_len = 1;
    let mut args = [regexp.clone(), JSValue::UNDEFINED];
    if method == JS_ATOM_Symbol_matchAll {
        args[args_len as usize] = ctx.create_string_from_char('g')?.into();
        args_len += 1;
    }

    let regexp_ctor = ctx.0.borrow().regexp_ctor.clone(); // avoid borrow for long
    let re = JS_CallConstructor(ctx, &regexp_ctor, &args[0..args_len]).check_exception()?;

    JS_Invoke(ctx, &re, method, &[S.into()]).check_exception()
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
    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    let mut s = p.content().copy_to_new().map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    s.trim(cc.magic() & 1 != 0, cc.magic() & 2 != 0);
    ctx.create_string_from_content(s).map(JSValue::String)
}

pub fn string_cmp(p1: &JSString, p2: &JSString, x1: i32, x2: i32, len: i32) -> i32 {
    let mut it1 = p1.content().chars().skip(x1 as usize);
    let mut it2 = p2.content().chars().skip(x2 as usize);

    for _ in 0..len {
        match (it1.next(), it2.next()) {
            (Some(c1), Some(c2)) => {
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
        .find_char(char::from_u32(c as u32).unwrap(), from as _)
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
        .find((*p2).content(), from as _)
        .map_or(-1, |i| i as i32)
}

/* also used for String.prototype.valueOf */
pub fn js_string_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    js_thisStringValue(ctx, cc.this().clone())
}

pub fn js_thisStringValue(ctx: &JSContext, this_val: JSValueConst) -> JSResult {
    if this_val.is_string() {
        Ok(this_val)
    } else if let Some(d) = this_val
        .object()
        .and_then(|p| p.0.payload.borrow().value().cloned())
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

    let p = JS_ToStringCheckObject(ctx, cc.this().clone())?;
    if p.is_empty() {
        return Ok(p.into());
    }

    let sc = match p.content().as_ref() {
        StringBuf::Empty | StringBuf::Null => unreachable!(),
        StringBuf::Utf8(s) => {
            let mut d = Vec::<u8>::try_with_capacity(s.len()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            d.extend_from_slice(s);
            if to_lower != 0 {
                d.make_ascii_lowercase();
            } else {
                d.make_ascii_uppercase();
            }
            StringBuf::Utf8(d)
        }
        StringBuf::Utf16(s) => {
            let mut w = Vec::<u16>::try_with_capacity(s.len() * 3 / 2).map_err(|_| {
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

    ctx.create_string_from_content(sc).map(JSValue::String)
}

pub const js_string_proto_funcs: [NativeField; 37] = [
    JS_PROP_INT32_DEF!("length", 0, JS_PROP_CONFIGURABLE),
    JS_CFUNC_NATIVE_DEF!("charAt", 1, js_string_at, 0),
    JS_CFUNC_NATIVE_DEF!("at", 1, js_string_at, 1),
    JS_CFUNC_NATIVE_DEF!("charCodeAt", 1, js_string_at, 2),
    // JS_CFUNC_NATIVE_DEF!("at", 1, js_string_charAt, 1),
    // JS_CFUNC_NATIVE_DEF!("charAt", 1, js_string_charAt, 0),
    // JS_CFUNC_NATIVE_DEF!("charCodeAt", 1, js_string_charCodeAt, 0),
    JS_CFUNC_NATIVE_DEF!("concat", 1, js_string_concat, 0),
    JS_CFUNC_NATIVE_DEF!("codePointAt", 1, js_string_codePointAt, 0),
    JS_CFUNC_NATIVE_DEF!("isWellFormed", 0, js_string_isWellFormed, 0),
    JS_CFUNC_NATIVE_DEF!("toWellFormed", 0, js_string_toWellFormed, 0),
    JS_CFUNC_NATIVE_DEF!("indexOf", 1, js_string_indexOf, 0),
    JS_CFUNC_NATIVE_DEF!("lastIndexOf", 1, js_string_indexOf, 1),
    JS_CFUNC_NATIVE_DEF!("includes", 1, js_string_includes, 0),
    JS_CFUNC_NATIVE_DEF!("endsWith", 1, js_string_includes, 2),
    JS_CFUNC_NATIVE_DEF!("startsWith", 1, js_string_includes, 1),
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
    // JS_CFUNC_DEF!("localeCompare", 1, js_string_localeCompare),
    JS_CFUNC_NATIVE_DEF!("toLowerCase", 0, js_string_toLowerCase, 1),
    JS_CFUNC_NATIVE_DEF!("toUpperCase", 0, js_string_toLowerCase, 0),
    JS_CFUNC_NATIVE_DEF!("toLocaleLowerCase", 0, js_string_toLowerCase, 1),
    JS_CFUNC_NATIVE_DEF!("toLocaleUpperCase", 0, js_string_toLowerCase, 0),
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
        let idx = it.idx;

        // iterate over string buf to get char or code at `idx`
        let mut str_iter = ss.content().iter_since(idx as usize);
        if let Some(c) = str_iter.next() {
            it.idx = idx + 1;
            *state = IteratorState::NotDone;
            match c {
                StringChar::Char(c) => ctx.create_string_from_char(c),
                StringChar::BrokenCode(c) => {
                    ctx.create_string_from_content(if (0..=255).contains(&c) {
                        StringBuf::Utf8(vec![c as u8])
                    } else {
                        StringBuf::Utf16(vec![c])
                    })
                }
            }
            .map(JSValue::String)
        } else {
            it.obj = JSValue::UNDEFINED;
            *state = IteratorState::Done;
            Ok(JSValue::UNDEFINED)
        }

        // old logic
        // if idx >= ss.len() as u32 {
        //     it_state.obj = JSValue::UNDEFINED;
        //     *state = IteratorState::Done;
        //     Ok(JSValue::UNDEFINED)
        // } else {
        //     let c = ss.content().chars().nth(idx as usize).unwrap();
        //     it_state.idx = idx + 1;
        //     *state = IteratorState::NotDone;
        //     ctx.create_string_from_char(c).map(JSValue::String)
        // }
    } else {
        Err(-1)
    }
}

pub const js_string_iterator_proto_funcs: [NativeField; 2] = [
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
        let c = a.coerce_to_i32(ctx)?;
        res_string.push_u16(c as u16).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    ctx.create_string_from_content(res_string)
        .map(JSValue::String)
}

pub fn js_string_fromCodePoint(ctx: &JSContext, cc: CallContext) -> JSResult {
    // XXX: could pre-compute string length if all arguments are JS_TAG_INT
    let mut res_string = StringBuf::Empty;

    for a in cc.arguments() {
        let c: i32 = if let JSValue::Int(i) = a {
            *i
        } else {
            let d: f64 = a.coerce_to_f64(ctx)?;
            d as i32
        };

        if let Some(c) = char::from_u32(c as u32) {
            res_string.push_char(c).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        } else {
            JS_ThrowRangeError(ctx, "invalid code point");
            return Err(-1);
        }
    }

    ctx.create_string_from_content(res_string)
        .map(JSValue::String)
}

pub fn js_string_raw(ctx: &JSContext, cc: CallContext) -> JSResult {
    // raw(temp,...a)
    let cooked = cc.arg(0).coerce_to_object(ctx)?;
    let raw = cooked
        .get_property(ctx, JS_ATOM_raw)?
        .coerce_to_object(ctx)?;

    let n = js_get_length64(ctx, &raw.clone().into())?;
    let mut res_string = StringBuf::Empty;

    for i in 0..n {
        let str = raw.get_property_i64(ctx, i)?.coerce_to_string(ctx)?;
        res_string.push(str.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        if i < n - 1 && i + 1 < cc.length() as i64 {
            let val = cc.arg(i as usize + 1);
            let s = val.coerce_to_string(ctx)?;
            res_string.push(s.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
    }

    ctx.create_string_from_content(res_string)
        .map(JSValue::String)
}

pub const js_string_funcs: [NativeField; 3] = [
    JS_CFUNC_NATIVE_DEF!("fromCharCode", 1, js_string_fromCharCode, 0),
    JS_CFUNC_NATIVE_DEF!("fromCodePoint", 1, js_string_fromCodePoint, 0),
    JS_CFUNC_NATIVE_DEF!("raw", 1, js_string_raw, 0),
];
