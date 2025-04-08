use {fallible_collections::FallibleVec, widestring::U16Str};

use crate::{
    array::JS_NewArray,
    atom::{consts::*, JSAtom, JSAtomStruct},
    class::js_create_from_ctor,
    context::JSContext,
    error::JS_ThrowRangeError,
    function::{JSCFunctionListEntry, JS_Call, JS_CallConstructor, JS_CallFree, JS_InvokeFree},
    iterator::{js_create_array_iterator, JSArrayIteratorData, JSIteratorKindEnum},
    regexp::js_is_regexp,
    value::{
        object::{
            js_get_length32, js_get_length64, JS_CreateDataPropertyUint32, JS_GetOpaque2,
            JS_GetProperty, JS_IsFunction, JS_SetObjectData, JS_ToObject, JS_ToObjectFree,
            JS_PROP_CONFIGURABLE,
        },
        JSString, JSTag, JSValue, JSValueConst, JS_ConcatString, JS_ConcatString3, JS_DupValue,
        JS_FreeValue, JS_IsException, JS_IsNull, JS_IsString, JS_IsSymbol, JS_IsUndefined,
        JS_NewBool, JS_NewString, JS_ToFloat64, JS_ToInt32, JS_ToInt32Clamp, JS_ToInt32Sat,
        JS_ToInt64Sat, JS_ToString, JS_ToStringCheckObject, JS_ToStringFree, JS_ToUint32,
        StringContent, JS_NAN, JS_STRING_LEN_MAX, JS_TAG_INT, JS_TAG_OBJECT, JS_TAG_STRING,
        JS_VALUE_GET_INT, JS_VALUE_GET_OBJ, JS_VALUE_GET_PTR, JS_VALUE_GET_STRING,
        JS_VALUE_GET_TAG,
    },
    JS_ALIAS_DEF, JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR,
    JS_ITERATOR_NEXT_DEF, JS_MKPTR, JS_PROP_INT32_DEF, JS_PROP_STRING_DEF,
};

pub unsafe fn js_string_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let tmp_str: JSValue = if argc == 0 {
        JS_ATOM_empty_string.to_value(ctx.as_mut().unwrap(), true)
    } else {
        let s = if new_target.is_undefined() && JS_IsSymbol(*argv.offset(0)) {
            let p = JS_VALUE_GET_PTR(*argv.offset(0)).cast::<JSAtomStruct>();
            JS_ConcatString3(
                ctx,
                c"Symbol(",
                (*p).atom_info()
                    .unwrap()
                    .atom()
                    .to_value(ctx.as_mut().unwrap(), true),
                c")",
            )
        } else {
            JS_ToString(ctx, *argv.offset(0))
        };
        if s.is_exception() {
            return JSValue::EXCEPTION;
        }

        s
    };

    if !new_target.is_undefined() {
        let obj = js_create_from_ctor(ctx, new_target, JS_CLASS_STRING);
        if !obj.is_exception() {
            JS_SetObjectData(ctx, obj, tmp_str);
            obj.define_property_value(
                ctx.as_mut().unwrap(),
                JS_ATOM_length,
                JSValue::from_i32(tmp_str.string().len() as _),
                0,
            )
            .unwrap();
        }
        obj
    } else {
        tmp_str
    }
}

pub unsafe fn js_string_charCodeAt(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }

    let p = JS_VALUE_GET_STRING(val);
    let mut idx: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut idx, *argv.offset(0)) != 0 {
        JS_FreeValue(ctx, val);
        return JSValue::EXCEPTION;
    }

    let ret = if idx < 0 || idx >= (*p).len() as i32 {
        JS_NAN
    } else {
        let c = (*p).content().chars().nth(idx as usize).unwrap();
        JSValue::from_i32(c as i32)
    };

    JS_FreeValue(ctx, val);
    ret
}

pub unsafe fn js_string_charAt(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    is_at: i32,
) -> JSValue {
    let mut ret: JSValue;

    let mut c: i32;

    let val = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }

    let p = JS_VALUE_GET_STRING(val);
    let mut idx: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut idx, *argv.offset(0)) != 0 {
        JS_FreeValue(ctx, val);
        return JSValue::EXCEPTION;
    }

    if idx < 0 && is_at != 0 {
        idx += (*p).len() as i32;
    }

    let ret = if idx < 0 || idx >= (*p).len() as i32 {
        if is_at != 0 {
            JSValue::UNDEFINED
        } else {
            (*ctx).create_string("")
        }
    } else {
        let c = (*p).content().chars().nth(idx as usize).unwrap();
        (*ctx).create_string_from_char(c)
    };

    JS_FreeValue(ctx, val);
    ret
}

pub unsafe fn js_string_codePointAt(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }

    let p = JS_VALUE_GET_STRING(val);
    let mut idx: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut idx, *argv.offset(0)) != 0 {
        JS_FreeValue(ctx, val);
        return JSValue::EXCEPTION;
    }

    let ret = if idx < 0 || idx >= (*p).len() as i32 {
        JSValue::UNDEFINED
    } else {
        let c = (*p).content().chars().nth(idx as usize).unwrap();
        JSValue::from_i32(c as u32 as i32)
    };

    JS_FreeValue(ctx, val);
    ret
}

pub unsafe fn js_string_concat(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    // XXX: Use more efficient method
    // XXX: This method is OK if r has a single refcount
    // XXX: should use string_buffer?
    let mut r = JS_ToStringCheckObject(ctx, this_val);
    for i in 0..argc {
        if JS_IsException(r) {
            break;
        }
        r = JS_ConcatString(ctx, r, JS_DupValue(ctx, *argv.offset(i as isize)));
    }

    r
}

// return the position of the first invalid character in the string or
// -1 if none
pub(crate) unsafe fn js_string_find_invalid_codepoint(p: *mut JSString) -> i32 {
    if let StringContent::Utf16(ws) = (*p).content().as_ref() {
        U16Str::from_slice(ws)
            .chars()
            .position(|c| c.is_err())
            .map_or(-1, |i| i as i32)
    } else {
        -1
    }
}

pub unsafe fn js_string_isWellFormed(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_STRING(str);
    let ret = js_string_find_invalid_codepoint(p) < 0;
    JS_FreeValue(ctx, str);

    JS_NewBool(ret)
}

pub unsafe fn js_string_toWellFormed(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_STRING(str);
    // avoid reallocating the string if it is well-formed
    let i = js_string_find_invalid_codepoint(p);
    if i < 0 {
        return str;
    }

    if let StringContent::Utf16(ws) = (*p).content().as_ref() {
        let ret = if let Ok(mut w) = Vec::<u16>::try_with_capacity(ws.len() * 3 / 2) {
            for c in U16Str::from_slice(ws).chars_lossy() {
                w.extend_from_slice(c.encode_utf16(&mut [0; 2]));
            }
            w.shrink_to_fit();
            (*ctx).create_string_from_content(StringContent::Utf16(w))
        } else {
            JSValue::EXCEPTION
        };

        JS_FreeValue(ctx, str);
        ret
    } else {
        unreachable!()
    }
}

pub unsafe fn js_string_indexOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    lastIndexOf: i32,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return str;
    }

    let v = JS_ToString(ctx, *argv.offset(0));
    if JS_IsException(v) {
        // C2RS: goto fail; replaced with early return
        JS_FreeValue(ctx, str);
        JS_FreeValue(ctx, v);
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_STRING(str);
    let p1 = JS_VALUE_GET_STRING(v);
    let len = (*p).len() as i32;
    let v_len = (*p1).len() as i32;

    let start: i32;
    let stop: i32;
    let inc: i32;

    if lastIndexOf != 0 {
        let mut pos = len - v_len;
        if argc > 1 {
            let mut d: f64 = 0.0;
            if JS_ToFloat64(ctx, &mut d, *argv.offset(1)) != 0 {
                JS_FreeValue(ctx, str);
                JS_FreeValue(ctx, v);
                return JSValue::EXCEPTION;
            }
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
        let mut pos = 0;
        if argc > 1 {
            if JS_ToInt32Clamp(ctx, &mut pos, *argv.offset(1), 0, len, 0) != 0 {
                JS_FreeValue(ctx, str);
                JS_FreeValue(ctx, v);
                return JSValue::EXCEPTION;
            }
        }
        start = pos;
        stop = len - v_len;
        inc = 1;
    }

    let mut ret = -1;
    if len >= v_len && inc * (stop - start) >= 0 {
        let mut i = start;
        loop {
            if string_cmp(p, p1, i, 0, v_len) == 0 {
                ret = i;
                break;
            }
            if i == stop {
                break;
            }
            i += inc;
        }
    }

    JS_FreeValue(ctx, str);
    JS_FreeValue(ctx, v);

    JSValue::from_i32(ret)
}

pub unsafe fn js_string_replace(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    is_replaceAll: i32,
) -> JSValue {
    // replace(rx, rep)
    let O = this_val;
    let searchValue = *argv.offset(0);
    let replaceValue = *argv.offset(1);

    let mut search_str = JSValue::UNDEFINED;
    let mut replaceValue_str = JSValue::UNDEFINED;
    let mut str = JSValue::UNDEFINED;

    // let mut b_s: StringBuffer = std::mem::zeroed();
    // let b = &mut b_s;

    if O.is_undefined_or_null() {
        return (*ctx).JS_ThrowTypeError(c"cannot convert to object");
    }

    macro_rules! exception {
        () => {
            //string_buffer_free(b);
            JS_FreeValue(ctx, search_str);
            JS_FreeValue(ctx, replaceValue_str);
            JS_FreeValue(ctx, str);
            return JSValue::EXCEPTION;
        };
    }

    if !JS_IsUndefined(searchValue) && !JS_IsNull(searchValue) {
        if is_replaceAll != 0 {
            if check_regexp_g_flag(ctx, searchValue) < 0 {
                return JSValue::EXCEPTION;
            }
        }

        let replacer = JS_GetProperty(ctx, searchValue, JS_ATOM_Symbol_replace);
        if JS_IsException(replacer) {
            return JSValue::EXCEPTION;
        } else if !JS_IsUndefined(replacer) && !JS_IsNull(replacer) {
            return JS_CallFree(
                ctx,
                replacer,
                searchValue,
                2,
                [O, replaceValue].as_mut_ptr(),
            );
        }
    }

    //string_buffer_init(ctx, b, 0);

    str = JS_ToString(ctx, O);
    if JS_IsException(str) {
        exception!();
    }
    search_str = JS_ToString(ctx, searchValue);
    if JS_IsException(search_str) {
        exception!();
    }

    let functionalReplace = JS_IsFunction(ctx, replaceValue);
    if !functionalReplace {
        replaceValue_str = JS_ToString(ctx, replaceValue);
        if JS_IsException(replaceValue_str) {
            exception!();
        }
    }

    let sp = JS_VALUE_GET_STRING(str);
    let searchp = JS_VALUE_GET_STRING(search_str);
    let mut is_first = true;
    let mut endOfLastMatch = 0_i32;

    let mut res_string = StringContent::Empty;

    loop {
        let pos = if (*searchp).is_empty() {
            if is_first {
                0
            } else if endOfLastMatch >= (*sp).len() as i32 {
                -1
            } else {
                endOfLastMatch + 1
            }
        } else {
            (*sp)
                .content()
                .find((*searchp).content(), endOfLastMatch as _)
                .map_or(-1, |i| i as i32)
        };

        if pos < 0 {
            if is_first {
                //string_buffer_free(b);
                JS_FreeValue(ctx, search_str);
                JS_FreeValue(ctx, replaceValue_str);
                return str;
            } else {
                break;
            }
        }

        let repl_str = if functionalReplace {
            JS_ToStringFree(
                ctx,
                JS_Call(
                    ctx,
                    replaceValue,
                    JSValue::UNDEFINED,
                    3,
                    [search_str, JSValue::from_i32(pos), str].as_mut_ptr(),
                ),
            )
        } else {
            js_string___GetSubstitution(
                ctx,
                JSValue::UNDEFINED,
                6,
                [
                    search_str,
                    str,
                    JSValue::from_i32(pos),
                    JSValue::UNDEFINED,
                    JSValue::UNDEFINED,
                    replaceValue_str,
                ]
                .as_mut_ptr(),
            )
        };

        if JS_IsException(repl_str) {
            exception!();
        }
        let repl_val = JS_ToStringFree(ctx, repl_str);
        if JS_IsException(repl_val) {
            exception!();
        }
        let repl = JS_VALUE_GET_STRING(repl_val);

        if res_string
            .push_content_slice(&(*sp).content().slice(pos as usize..endOfLastMatch as usize))
            .is_err()
        {
            JS_FreeValue(ctx, repl_val);
            ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
            exception!();
        };

        if res_string.push((*repl).content()).is_err() {
            JS_FreeValue(ctx, repl_val);
            ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
            exception!();
        };

        JS_FreeValue(ctx, repl_val);

        endOfLastMatch = pos + (*searchp).len() as i32;
        is_first = false;
        if is_replaceAll == 0 {
            break;
        }
    }

    // remainder of src string
    let rem_len = (*sp).len() - endOfLastMatch as usize;
    if rem_len > 0 {
        if res_string
            .push_chars_iter(
                (*sp)
                    .content()
                    .chars()
                    .skip(endOfLastMatch as usize)
                    .take(rem_len),
            )
            .is_err()
        {
            ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
            exception!();
        };
    }

    JS_FreeValue(ctx, search_str);
    JS_FreeValue(ctx, replaceValue_str);
    JS_FreeValue(ctx, str);

    (*ctx).create_string_from_content(res_string)
}

pub unsafe fn js_string_split(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    // split(sep, limit)
    let O = this_val;

    if JS_IsUndefined(O) || JS_IsNull(O) {
        return ctx
            .as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"cannot convert to object");
    }

    let separator = *argv.offset(0);
    let limit = *argv.offset(1);
    let mut args: [JSValueConst; 2] = [JSValueConst::default(); 2];
    let mut lengthA: u32 = 0;

    let mut S = JSValue::UNDEFINED;
    let mut A = JSValue::UNDEFINED;
    let mut R = JSValue::UNDEFINED;
    let mut T = JSValue::UNDEFINED;

    macro_rules! done {
        () => {
            JS_FreeValue(ctx, S);
            JS_FreeValue(ctx, R);
            return A;
        };
    }
    macro_rules! exception {
        () => {
            JS_FreeValue(ctx, A);
            JS_FreeValue(ctx, S);
            JS_FreeValue(ctx, R);
            return JSValue::EXCEPTION;
        };
    }

    if !JS_IsUndefined(separator) && !JS_IsNull(separator) {
        let splitter = JS_GetProperty(ctx, separator, JS_ATOM_Symbol_split);
        if JS_IsException(splitter) {
            return JSValue::EXCEPTION;
        }
        if !JS_IsUndefined(splitter) && !JS_IsNull(splitter) {
            args[0] = O;
            args[1] = limit;
            return JS_CallFree(ctx, splitter, separator, 2, args.as_mut_ptr());
        }
    }

    S = JS_ToString(ctx, O);
    if JS_IsException(S) {
        exception!();
    }
    A = JS_NewArray(ctx);
    if JS_IsException(A) {
        exception!();
    }
    lengthA = 0;

    let lim = if JS_IsUndefined(limit) {
        u32::MAX
    } else {
        if let Ok(x) = JS_ToUint32(ctx, limit) {
            x
        } else {
            exception!();
        }
    };

    let sp = JS_VALUE_GET_STRING(S);
    let s = (*sp).len() as i32;
    R = JS_ToString(ctx, separator);
    if JS_IsException(R) {
        exception!();
    }

    let rp = JS_VALUE_GET_STRING(R);
    let r = (*rp).len() as i32;
    let mut p = 0;
    if lim == 0 {
        done!();
    }

    macro_rules! add_tail {
        () => {
            T = js_sub_string(ctx, sp, p, s);
            if JS_IsException(T) {
                exception!();
            } else if JS_CreateDataPropertyUint32(ctx, A, lengthA as _, T, 0) < 0 {
                exception!();
            } else {
                done!();
            }
        };
    }

    if JS_IsUndefined(separator) {
        add_tail!();
    }
    if s == 0 {
        if r != 0 {
            add_tail!();
        }
        done!();
    }

    let mut q = p;
    while {
        q += if r == 0 { 1 } else { 0 };
        q
    } <= s - r - if r == 0 { 1 } else { 0 }
    {
        let e = if let Some(i) = (*sp).content().find((*rp).content(), q as _) {
            i as i32
        } else {
            break;
        };

        T = js_sub_string(ctx, sp, p, e);
        if JS_IsException(T) {
            exception!();
        }
        if JS_CreateDataPropertyUint32(ctx, A, lengthA as _, T, 0) < 0 {
            exception!();
        }

        lengthA += 1;
        if lengthA == lim {
            done!();
        }

        p = e + r;
        q = p;
    }

    add_tail!();
}

pub unsafe fn js_string_substring(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return str;
    }

    let p = JS_VALUE_GET_STRING(str);

    let mut a = 0_i32;
    if JS_ToInt32Clamp(ctx, &mut a, *argv.offset(0), 0, (*p).len() as i32, 0) != 0 {
        JS_FreeValue(ctx, str);
        return JSValue::EXCEPTION;
    }

    let mut b = (*p).len() as i32;
    if !JS_IsUndefined(*argv.offset(1)) {
        if JS_ToInt32Clamp(ctx, &mut b, *argv.offset(1), 0, (*p).len() as i32, 0) != 0 {
            JS_FreeValue(ctx, str);
            return JSValue::EXCEPTION;
        }
    }

    let start: i32;
    let end: i32;

    if a < b {
        start = a;
        end = b;
    } else {
        start = b;
        end = a;
    }

    let ret = js_sub_string(ctx, p, start, end);
    JS_FreeValue(ctx, str);
    ret
}

pub unsafe fn js_string_substr(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let mut str = JSValue::default();
    let mut ret = JSValue::default();
    let mut a: i32 = 0;
    let mut len: i32 = 0;
    let mut n: i32 = 0;
    let mut p: *mut JSString = std::ptr::null_mut();

    str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return str;
    }
    p = JS_VALUE_GET_STRING(str);
    len = (*p).len() as i32;
    if JS_ToInt32Clamp(ctx, &mut a, *argv.offset(0), 0, len, len) != 0 {
        JS_FreeValue(ctx, str);
        return JSValue::EXCEPTION;
    }
    n = len - a;
    if !JS_IsUndefined(*argv.offset(1)) {
        if JS_ToInt32Clamp(ctx, &mut n, *argv.offset(1), 0, len - a, 0) != 0 {
            JS_FreeValue(ctx, str);
            return JSValue::EXCEPTION;
        }
    }
    ret = js_sub_string(ctx, p, a, a + n);
    JS_FreeValue(ctx, str);
    ret
}

pub unsafe fn js_string_slice(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return str;
    }
    let p = JS_VALUE_GET_STRING(str);
    let len = (*p).len() as i32;

    let mut start = 0_i32;
    if JS_ToInt32Clamp(ctx, &mut start, *argv.offset(0), 0, len, len) != 0 {
        JS_FreeValue(ctx, str);
        return JSValue::EXCEPTION;
    }

    let mut end = len;
    if !JS_IsUndefined(*argv.offset(1)) {
        if JS_ToInt32Clamp(ctx, &mut end, *argv.offset(1), 0, len, len) != 0 {
            JS_FreeValue(ctx, str);
            return JSValue::EXCEPTION;
        }
    }
    let ret = js_sub_string(ctx, p, start, std::cmp::max(end, start));
    JS_FreeValue(ctx, str);

    ret
}

pub unsafe fn js_string_pad(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    padEnd: i32,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return JSValue::EXCEPTION;
    }

    let mut v = JSValue::UNDEFINED;
    // let mut b_s: StringBuffer = std::mem::zeroed();
    // let b = &mut b_s;
    let mut c = ' ';

    macro_rules! fail {
        () => {
            //string_buffer_free(b);
            fail3!();
        };
    }
    macro_rules! fail3 {
        () => {
            JS_FreeValue(ctx, v);
            fail2!();
        };
    }
    macro_rules! fail2 {
        () => {
            JS_FreeValue(ctx, str);
            fail1!()
        };
    }
    macro_rules! fail1 {
        () => {
            return JSValue::EXCEPTION;
        };
    }

    let mut n: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut n, *argv.offset(0)) != 0 {
        fail2!();
    }

    let p = JS_VALUE_GET_STRING(str);
    let mut p1 = std::ptr::null_mut();

    let len = (*p).len() as i32;
    if len >= n {
        return str;
    }

    if argc > 1 && !JS_IsUndefined(*argv.offset(1)) {
        v = JS_ToString(ctx, *argv.offset(1));
        if JS_IsException(v) {
            fail2!();
        }

        p1 = JS_VALUE_GET_STRING(v);
        if (*p1).is_empty() {
            JS_FreeValue(ctx, v);
            return str;
        }
        if (*p1).len() == 1 {
            c = (*p1).content().chars().next().unwrap();
            p1 = std::ptr::null_mut();
        }
    }

    if n > JS_STRING_LEN_MAX as i32 {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowInternalError(c"string too long");
        fail2!();
    }
    // if string_buffer_init(ctx, b, n as _) != 0 {
    //     fail3!();
    // }

    let mut res_string = if padEnd != 0 {
        if let Ok(s) = (*p).content().copy_to_new() {
            s
        } else {
            (*ctx).JS_ThrowOutOfMemory();
            fail!();
        }
    } else {
        StringContent::Empty
    };

    n -= len;

    let r = if !p1.is_null() {
        res_string.push_chars_iter(
            std::iter::repeat_n((*p1).content().chars(), (n as usize).div_ceil((*p1).len()))
                .flatten(),
        )
    } else {
        res_string.push_chars_iter(std::iter::repeat_n(c, n as usize))
    };
    if r.is_err() {
        (*ctx).JS_ThrowOutOfMemory();
        fail!();
    }

    if padEnd == 0 {
        if res_string.push((*p).content()).is_err() {
            (*ctx).JS_ThrowOutOfMemory();
            fail!();
        }
    }

    JS_FreeValue(ctx, v);
    JS_FreeValue(ctx, str);

    (*ctx).create_string_from_content(res_string)
}

pub unsafe fn js_string_repeat(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        JS_FreeValue(ctx, str);
        return JSValue::EXCEPTION;
    }

    macro_rules! fail {
        () => {
            JS_FreeValue(ctx, str);
            return JSValue::EXCEPTION;
        };
    }

    let mut val: i64 = 0;
    if JS_ToInt64Sat(ctx, &mut val, *argv.offset(0)) != 0 {
        fail!();
    }
    if !(0..=2147483647).contains(&val) {
        JS_ThrowRangeError(ctx, c"invalid repeat count");
        fail!();
    }

    let n = val as usize;
    let p = JS_VALUE_GET_STRING(str);
    let len = (*p).len();
    if len == 0 || n == 1 {
        return str;
    }
    if val as usize * len > JS_STRING_LEN_MAX {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowInternalError(c"string too long");
        fail!();
    }

    let mut res_string = if let Ok(s) = StringContent::with_capacity(n * len, (*p).is_wide_char()) {
        s
    } else {
        fail!();
    };

    res_string
        .push_chars_iter(std::iter::repeat_n((*p).content().chars(), n).flatten())
        .unwrap();
    res_string.shrink_to_fit();

    JS_FreeValue(ctx, str);
    (*ctx).create_string_from_content(res_string)
}

pub unsafe fn js_sub_string(
    ctx: *mut JSContext,
    p: *mut JSString,
    start: i32,
    end: i32,
) -> JSValue {
    let start = std::cmp::max(0, start);
    let end = std::cmp::min(end, (*p).len() as i32);

    if start == 0 && end == (*p).len() as i32 {
        JS_DupValue(ctx, JS_MKPTR!(JSTag::String, p.cast()))
    } else {
        let count = if end > start { end - start } else { 0 };
        if count > 0 {
            if let Ok(sub) = (*p)
                .content()
                .slice(start as usize..end as usize)
                .to_owned()
            {
                (*ctx).create_string_from_content(sub)
            } else {
                (*ctx).JS_ThrowOutOfMemory();
                JSValue::EXCEPTION
            }
        } else {
            (*ctx).create_string_from_content(StringContent::Empty)
        }
    }
}

pub unsafe fn js_string_includes(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return str;
    }

    match js_is_regexp(ctx, *argv.offset(0)) {
        Ok(false) => {}
        Ok(true) => {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"regex not supported");
            JS_FreeValue(ctx, str);
            return JSValue::EXCEPTION;
        }
        Err(_) => {
            JS_FreeValue(ctx, str);
            return JSValue::EXCEPTION;
        }
    }

    let v = JS_ToString(ctx, *argv.offset(0));
    if JS_IsException(v) {
        JS_FreeValue(ctx, str);
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_STRING(str);
    let p1 = JS_VALUE_GET_STRING(v);
    let mut len = (*p).len() as i32;
    let v_len = (*p1).len() as i32;

    let mut pos = if magic == 2 { len } else { 0 };
    if argc > 1 && !JS_IsUndefined(*argv.offset(1)) {
        if JS_ToInt32Clamp(ctx, &mut pos, *argv.offset(1), 0, len, 0) != 0 {
            JS_FreeValue(ctx, str);
            JS_FreeValue(ctx, v);
            return JSValue::EXCEPTION;
        }
    }

    len -= v_len;

    let mut ret = false;
    let mut pos: i32 = 0;
    let mut start: i32 = 0;
    let mut stop: i32 = 0;
    if magic == 0 {
        start = pos;
        stop = len;
    } else {
        if magic == 1 {
            if pos > len {
                JS_FreeValue(ctx, str);
                JS_FreeValue(ctx, v);
                return JS_NewBool(ret);
            }
        } else {
            pos -= v_len;
        }
        start = pos;
        stop = pos;
    }

    if start >= 0 && start <= stop {
        for i in start..stop {
            if string_cmp(p, p1, i, 0, v_len) == 0 {
                ret = true;
                break;
            }
        }
    }

    JS_FreeValue(ctx, str);
    JS_FreeValue(ctx, v);

    JS_NewBool(ret)
}

pub unsafe fn check_regexp_g_flag(ctx: *mut JSContext, regexp: JSValueConst) -> i32 {
    let ret = if let Ok(b) = js_is_regexp(ctx, regexp) {
        b
    } else {
        return -1;
    };

    if ret {
        let mut flags = JS_GetProperty(ctx, regexp, JS_ATOM_flags);
        if JS_IsException(flags) {
            return -1;
        }
        if JS_IsUndefined(flags) || JS_IsNull(flags) {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"cannot convert to object");
            return -1;
        }

        flags = JS_ToStringFree(ctx, flags);
        if JS_IsException(flags) {
            return -1;
        }

        let idx = (*JS_VALUE_GET_STRING(flags)).content().find_char('g', 0);
        JS_FreeValue(ctx, flags);
        if idx.is_none() {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"regexp must have the 'g' flag");
            return -1;
        }
    }

    0
}

pub unsafe fn js_string_match(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let mut O: JSValueConst = this_val;
    let regexp: JSValueConst = *argv.offset(0);
    let atom = JSAtom(magic as u32);

    if JS_IsUndefined(O) || JS_IsNull(O) {
        return ctx
            .as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"cannot convert to object");
    }

    if !JS_IsUndefined(regexp) && !JS_IsNull(regexp) {
        let matcher = JS_GetProperty(ctx, regexp, atom);
        if JS_IsException(matcher) {
            return JSValue::EXCEPTION;
        }

        if atom == JS_ATOM_Symbol_matchAll {
            if check_regexp_g_flag(ctx, regexp) < 0 {
                JS_FreeValue(ctx, matcher);
                return JSValue::EXCEPTION;
            }
        }
        if !JS_IsUndefined(matcher) && !JS_IsNull(matcher) {
            return JS_CallFree(ctx, matcher, regexp, 1, &mut O);
        }
    }

    let mut S = JS_ToString(ctx, O);
    if JS_IsException(S) {
        return JSValue::EXCEPTION;
    }

    let mut args_len = 1;
    let mut args: [JSValueConst; 2] = [JSValue::UNDEFINED; 2];
    args[0] = regexp;

    let mut str = JSValue::UNDEFINED;

    if atom == JS_ATOM_Symbol_matchAll {
        str = JS_NewString(ctx, c"g");
        if JS_IsException(str) {
            JS_FreeValue(ctx, S);
            return JSValue::EXCEPTION;
        }
        args[args_len as usize] = str;
        args_len += 1;
    }

    let rx = JS_CallConstructor(ctx, (*ctx).regexp_ctor, args_len, args.as_mut_ptr());
    JS_FreeValue(ctx, str);
    if JS_IsException(rx) {
        JS_FreeValue(ctx, S);
        return JSValue::EXCEPTION;
    }

    let result = JS_InvokeFree(ctx, rx, atom, 1, &mut S);
    JS_FreeValue(ctx, S);

    result
}

pub unsafe fn js_string___GetSubstitution(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
) -> JSValue {
    let matched = *argv.offset(0); // 匹配到的字符串。
    let str = *argv.offset(1); // 原始字符串。
    let captures = *argv.offset(3); // 捕获组，通常是正则表达式匹配的结果。
    let namedCaptures = *argv.offset(4); // 命名捕获组。
    let rep = *argv.offset(5); // 替换字符串或替换函数。

    let sp = JS_VALUE_GET_STRING(str);
    let rp = JS_VALUE_GET_STRING(rep);

    if !JS_IsString(rep) || !JS_IsString(str) {
        return (*ctx).JS_ThrowTypeError(c"not a string");
    }

    let captures_len = if !JS_IsUndefined(captures) {
        if let Ok(n) = js_get_length32(ctx, captures) {
            n
        } else {
            return JSValue::EXCEPTION;
        }
    } else {
        0
    };

    let matched_len = if let Ok(n) = js_get_length32(ctx, matched) {
        n
    } else {
        return JSValue::EXCEPTION;
    };

    let position = if let Ok(n) = JS_ToUint32(ctx, *argv.offset(2)) {
        n
    } else {
        return JSValue::EXCEPTION;
    };

    let mut res_string =
        if let Ok(s) = StringContent::with_capacity((*sp).len(), (*sp).is_wide_char()) {
            s
        } else {
            (*ctx).JS_ThrowOutOfMemory();
            return JSValue::EXCEPTION;
        };

    let len = (*rp).len();
    let mut i = 0;
    loop {
        let mut j = if let Some(x) = (*rp).content().find_char('$', i) {
            x
        } else {
            break;
        };

        if j + 1 >= len as u16 {
            break;
        }

        if res_string
            .push_content_slice(&(*rp).content().slice(i as usize..j as usize))
            .is_err()
        {
            (*ctx).JS_ThrowOutOfMemory();
            return JSValue::EXCEPTION;
        }

        let j0 = j;
        j += 1;
        let c = (*rp).content().chars().nth(j as usize).unwrap();
        j += 1;
        if c == '$' {
            if res_string.push_char(c).is_err() {
                return JSValue::EXCEPTION;
            }
        } else if c == '&' {
            let sval = JS_ToStringFree(ctx, matched);
            if sval.is_exception() {
                return JSValue::EXCEPTION;
            }
            let r = res_string.push((*JS_VALUE_GET_STRING(sval)).content());
            JS_FreeValue(ctx, sval);
            if r.is_err() {
                return JSValue::EXCEPTION;
            }
        } else if c == '`' {
            if res_string
                .push_content_slice(&(*sp).content().slice(0..position as usize))
                .is_err()
            {
                return JSValue::EXCEPTION;
            }
        } else if c == '\'' {
            if res_string
                .push_content_slice(
                    &(*sp)
                        .content()
                        .slice((position + matched_len) as usize..(*sp).len()),
                )
                .is_err()
            {
                return JSValue::EXCEPTION;
            }
        } else if c.is_ascii_digit() {
            let mut k = c as u32 - '0' as u32;
            if j < len as u16 {
                let c1 = (*rp).content().chars().nth(j as usize).unwrap();
                if c1.is_ascii_digit() {
                    let k1 = k * 10 + (c1 as u32 - '0' as u32);
                    if k1 >= 1 && k1 < captures_len {
                        k = k1;
                        j += 1;
                    }
                }
            }

            if k >= 1 && k < captures_len {
                let s = captures.get_property_i64(ctx.as_mut().unwrap(), k as i64);
                if s.is_exception() {
                    return JSValue::EXCEPTION;
                }
                if !s.is_undefined() {
                    let sval = JS_ToStringFree(ctx, s);
                    if sval.is_exception() {
                        return JSValue::EXCEPTION;
                    }
                    let r = res_string.push((*JS_VALUE_GET_STRING(sval)).content());
                    JS_FreeValue(ctx, sval);
                    if r.is_err() {
                        return JSValue::EXCEPTION;
                    }
                }
            } else {
                if res_string
                    .push_content_slice(&(*rp).content().slice(j0 as usize..j as usize))
                    .is_err()
                {
                    return JSValue::EXCEPTION;
                }
            }
        } else if c == '<' && !namedCaptures.is_undefined() {
            if let Some(k) = (*rp).content().find_char('>', j as _) {
                let name = js_sub_string(ctx, rp, j as _, k as _);
                if name.is_exception() {
                    return JSValue::EXCEPTION;
                }

                let capture = namedCaptures.get_property_value(ctx.as_mut().unwrap(), name);
                if capture.is_exception() {
                    return JSValue::EXCEPTION;
                }
                if !capture.is_undefined() {
                    let sval = JS_ToStringFree(ctx, capture);
                    if sval.is_exception() {
                        return JSValue::EXCEPTION;
                    }
                    let r = res_string.push((*JS_VALUE_GET_STRING(sval)).content());
                    JS_FreeValue(ctx, sval);
                    if r.is_err() {
                        return JSValue::EXCEPTION;
                    }
                }
                j = k + 1;
            } else {
                if res_string
                    .push_content_slice(&(*rp).content().slice(j0 as usize..j as usize))
                    .is_err()
                {
                    return JSValue::EXCEPTION;
                }
            }
        } else {
            if res_string
                .push_content_slice(&(*rp).content().slice(j0 as usize..j as usize))
                .is_err()
            {
                return JSValue::EXCEPTION;
            }
        }
        i = j;
    }

    if res_string
        .push_content_slice(&(*rp).content().slice(i as usize..(*rp).len()))
        .is_err()
    {
        return JSValue::EXCEPTION;
    }

    (*ctx).create_string_from_content(res_string)
}

pub unsafe fn js_string_trim(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let str = JS_ToStringCheckObject(ctx, this_val);
    if JS_IsException(str) {
        return str;
    }

    let p = JS_VALUE_GET_STRING(str);
    if let Ok(mut s) = (*p).content().copy_to_new() {
        JS_FreeValue(ctx, str);
        s.trim(magic & 1 != 0, magic & 2 != 0);
        (*ctx).create_string_from_content(s)
    } else {
        JS_FreeValue(ctx, str);
        JSValue::EXCEPTION
    }
}

pub unsafe fn string_cmp(p1: *mut JSString, p2: *mut JSString, x1: i32, x2: i32, len: i32) -> i32 {
    let mut it1 = (*p1).content().chars().skip(x1 as usize);
    let mut it2 = (*p2).content().chars().skip(x2 as usize);

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

#[deprecated(note = "use StringContent::find_char() instead")]
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

#[deprecated(note = "use StringContent::find() instead")]
pub unsafe fn string_indexof(p1: *mut JSString, p2: *mut JSString, from: i32) -> i32 {
    debug_assert!(from >= 0);
    (*p1)
        .content()
        .find((*p2).content(), from as _)
        .map_or(-1, |i| i as i32)
}

/* also used for String.prototype.valueOf */
pub unsafe fn js_string_toString(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    js_thisStringValue(ctx, this_val)
}

pub unsafe fn js_thisStringValue(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
    if JS_VALUE_GET_TAG(this_val) == JS_TAG_STRING {
        return this_val.dup();
    }

    if JS_VALUE_GET_TAG(this_val) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(this_val);
        if (*p).class_id() == JS_CLASS_STRING {
            if JS_VALUE_GET_TAG((*p).u.object_data) == JS_TAG_STRING {
                return (*p).u.object_data.dup();
            }
        }
    }

    (*ctx).JS_ThrowTypeError(c"not a string")
}

pub unsafe fn js_string_toLowerCase(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    to_lower: i32,
) -> JSValue {
    let val = JS_ToStringCheckObject(ctx, this_val); // val is duplicated inside. don't forget free it.
    if JS_IsException(val) {
        return val;
    }

    let p = JS_VALUE_GET_STRING(val);
    if (*p).is_empty() {
        return val;
    }

    let sc = match (*p).content().as_ref() {
        StringContent::Empty => unreachable!(),
        StringContent::Utf8(s) => {
            if let Ok(mut d) = Vec::<u8>::try_with_capacity(s.len()) {
                d.extend_from_slice(s);
                if to_lower != 0 {
                    d.make_ascii_lowercase();
                } else {
                    d.make_ascii_uppercase();
                }
                StringContent::Utf8(d)
            } else {
                JS_FreeValue(ctx, val);
                return JSValue::EXCEPTION;
            }
        }
        StringContent::Utf16(s) => {
            if let Ok(mut w) = Vec::<u16>::try_with_capacity(s.len() * 3 / 2) {
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
                StringContent::Utf16(w)
            } else {
                JS_FreeValue(ctx, val);
                return JSValue::EXCEPTION;
            }
        }
    };

    let sval = ctx.as_mut().unwrap().create_string_from_content(sc);
    JS_FreeValue(ctx, val);
    sval
}

pub const js_string_proto_funcs: [JSCFunctionListEntry; 37] = [
    JS_PROP_INT32_DEF!(c"length", 0, JS_PROP_CONFIGURABLE),
    JS_CFUNC_MAGIC_DEF!(c"at", 1, js_string_charAt, 1),
    JS_CFUNC_DEF!(c"charCodeAt", 1, js_string_charCodeAt),
    JS_CFUNC_MAGIC_DEF!(c"charAt", 1, js_string_charAt, 0),
    JS_CFUNC_DEF!(c"concat", 1, js_string_concat),
    JS_CFUNC_DEF!(c"codePointAt", 1, js_string_codePointAt),
    JS_CFUNC_DEF!(c"isWellFormed", 0, js_string_isWellFormed),
    JS_CFUNC_DEF!(c"toWellFormed", 0, js_string_toWellFormed),
    JS_CFUNC_MAGIC_DEF!(c"indexOf", 1, js_string_indexOf, 0),
    JS_CFUNC_MAGIC_DEF!(c"lastIndexOf", 1, js_string_indexOf, 1),
    JS_CFUNC_MAGIC_DEF!(c"includes", 1, js_string_includes, 0),
    JS_CFUNC_MAGIC_DEF!(c"endsWith", 1, js_string_includes, 2),
    JS_CFUNC_MAGIC_DEF!(c"startsWith", 1, js_string_includes, 1),
    JS_CFUNC_MAGIC_DEF!(c"match", 1, js_string_match, JS_ATOM_Symbol_match.0 as _),
    JS_CFUNC_MAGIC_DEF!(
        c"matchAll",
        1,
        js_string_match,
        JS_ATOM_Symbol_matchAll.0 as _
    ),
    JS_CFUNC_MAGIC_DEF!(c"search", 1, js_string_match, JS_ATOM_Symbol_search.0 as _),
    JS_CFUNC_DEF!(c"split", 2, js_string_split),
    JS_CFUNC_DEF!(c"substring", 2, js_string_substring),
    JS_CFUNC_DEF!(c"substr", 2, js_string_substr),
    JS_CFUNC_DEF!(c"slice", 2, js_string_slice),
    JS_CFUNC_DEF!(c"repeat", 1, js_string_repeat),
    JS_CFUNC_MAGIC_DEF!(c"replace", 2, js_string_replace, 0),
    JS_CFUNC_MAGIC_DEF!(c"replaceAll", 2, js_string_replace, 1),
    JS_CFUNC_MAGIC_DEF!(c"padEnd", 1, js_string_pad, 1),
    JS_CFUNC_MAGIC_DEF!(c"padStart", 1, js_string_pad, 0),
    JS_CFUNC_MAGIC_DEF!(c"trim", 0, js_string_trim, 3),
    JS_CFUNC_MAGIC_DEF!(c"trimEnd", 0, js_string_trim, 2),
    JS_ALIAS_DEF!(c"trimRight", c"trimEnd"),
    JS_CFUNC_MAGIC_DEF!(c"trimStart", 0, js_string_trim, 1),
    JS_ALIAS_DEF!(c"trimLeft", c"trimStart"),
    JS_CFUNC_DEF!(c"toString", 0, js_string_toString),
    JS_CFUNC_DEF!(c"valueOf", 0, js_string_toString),
    // JS_CFUNC_DEF!(c"localeCompare", 1, js_string_localeCompare),
    JS_CFUNC_MAGIC_DEF!(c"toLowerCase", 0, js_string_toLowerCase, 1),
    JS_CFUNC_MAGIC_DEF!(c"toUpperCase", 0, js_string_toLowerCase, 0),
    JS_CFUNC_MAGIC_DEF!(c"toLocaleLowerCase", 0, js_string_toLowerCase, 1),
    JS_CFUNC_MAGIC_DEF!(c"toLocaleUpperCase", 0, js_string_toLowerCase, 0),
    JS_CFUNC_MAGIC_DEF!(
        c"[Symbol.iterator]",
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

unsafe fn js_string_iterator_next(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    pdone: *mut i32,
    magic: i32,
) -> JSValue {
    let it = JS_GetOpaque2(ctx, this_val, JS_CLASS_STRING_ITERATOR).cast::<JSArrayIteratorData>();
    if it.is_null() {
        *pdone = 0;
        return JSValue::EXCEPTION;
    }
    if JS_IsUndefined((*it).obj) {
        *pdone = 1;
        return JSValue::UNDEFINED;
    }

    let p = JS_VALUE_GET_STRING((*it).obj);
    let idx = (*it).idx;

    if idx >= (*p).len() as u32 {
        JS_FreeValue(ctx, (*it).obj);
        (*it).obj = JSValue::UNDEFINED;
        *pdone = 1;
        return JSValue::UNDEFINED;
    }

    let c = (*p).content().chars().nth(idx as usize).unwrap();
    (*it).idx = idx;
    *pdone = 0;

    (*ctx).create_string_from_char(c)
}

pub const js_string_iterator_proto_funcs: [JSCFunctionListEntry; 2] = [
    JS_ITERATOR_NEXT_DEF!(c"next", 0, js_string_iterator_next, 0),
    JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"String Iterator",
        JS_PROP_CONFIGURABLE
    ),
];

pub unsafe fn js_string_fromCharCode(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let mut res_string = StringContent::Empty;

    for i in 0..argc {
        if let Ok(c) = JS_ToInt32(ctx, *argv.offset(i as isize)) {
            if res_string.push_u16(c as u16).is_err() {
                (*ctx).JS_ThrowOutOfMemory();
                return JSValue::EXCEPTION;
            }
        } else {
            return JSValue::EXCEPTION;
        }
    }

    (*ctx).create_string_from_content(res_string)
}

pub unsafe fn js_string_fromCodePoint(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    // XXX: could pre-compute string length if all arguments are JS_TAG_INT
    let mut res_string = StringContent::Empty;

    for i in 0..argc {
        let c: i32 = if JS_VALUE_GET_TAG(*argv.offset(i as isize)) == JS_TAG_INT {
            JS_VALUE_GET_INT(*argv.offset(i as isize))
        } else {
            let mut d: f64 = 0.0;
            if JS_ToFloat64(ctx, &mut d, *argv.offset(i as isize)) != 0 {
                return JSValue::EXCEPTION;
            }
            d as i32
        };

        if let Some(c) = char::from_u32(c as u32) {
            if res_string.push_char(c).is_err() {
                (*ctx).JS_ThrowOutOfMemory();
                return JSValue::EXCEPTION;
            }
        } else {
            JS_ThrowRangeError(ctx, c"invalid code point");
            return JSValue::EXCEPTION;
        }
    }

    (*ctx).create_string_from_content(res_string)
}

pub unsafe fn js_string_raw(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    // raw(temp,...a)

    let cooked = JS_ToObject(ctx, *argv.offset(0));
    if JS_IsException(cooked) {
        return JSValue::EXCEPTION;
    }

    let raw = JS_ToObjectFree(ctx, JS_GetProperty(ctx, cooked, JS_ATOM_raw));
    if JS_IsException(raw) {
        JS_FreeValue(ctx, cooked);
        return JSValue::EXCEPTION;
    }

    let n = if let Ok(n) = js_get_length64(ctx, raw) {
        n
    } else {
        JS_FreeValue(ctx, cooked);
        JS_FreeValue(ctx, raw);
        return JSValue::EXCEPTION;
    };

    let mut res_string = StringContent::Empty;

    for i in 0..n {
        let tmp = JS_ToStringFree(ctx, raw.get_property_i64(ctx.as_mut().unwrap(), i));
        if tmp.is_exception() {
            JS_FreeValue(ctx, cooked);
            JS_FreeValue(ctx, raw);
            return JSValue::EXCEPTION;
        }

        let r = res_string.push(tmp.string().content());
        JS_FreeValue(ctx, tmp);

        if r.is_err() {
            JS_FreeValue(ctx, cooked);
            JS_FreeValue(ctx, raw);
            return JSValue::EXCEPTION;
        }

        if i < n - 1 && i + 1 < argc as i64 {
            let val = *argv.offset((i + 1) as isize);
            let tmp = JS_ToStringFree(ctx, val);
            if tmp.is_exception() {
                JS_FreeValue(ctx, cooked);
                JS_FreeValue(ctx, raw);
                return JSValue::EXCEPTION;
            }

            let r = res_string.push(tmp.string().content());
            JS_FreeValue(ctx, tmp);

            if r.is_err() {
                JS_FreeValue(ctx, cooked);
                JS_FreeValue(ctx, raw);
                return JSValue::EXCEPTION;
            }
        }
    }

    JS_FreeValue(ctx, cooked);
    JS_FreeValue(ctx, raw);
    (*ctx).create_string_from_content(res_string)
}

pub const js_string_funcs: [JSCFunctionListEntry; 3] = [
    JS_CFUNC_DEF!(c"fromCharCode", 1, js_string_fromCharCode),
    JS_CFUNC_DEF!(c"fromCodePoint", 1, js_string_fromCodePoint),
    JS_CFUNC_DEF!(c"raw", 1, js_string_raw),
];
