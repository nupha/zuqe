use std::rc::Rc;

use smallvec::SmallVec;

use crate::{
    JS_CLASS_REGEXP,
    atom::Atom,
    builtins::js_get_this,
    context::{ContextImpl, ContextPtr, JS_GetActiveFunction},
    error::JSResult,
    function::{CallContext, Callback, FieldDef, JS_CallConstructor, js_function_apply},
    object::{
        JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JS_SpeciesConstructor, ObjectPayload,
        ObjectRef,
    },
    string::{JS_ConcatString3, StringBuf, StringChar, StringData, StringRef},
    value::JSValue,
};

use super::{
    LRE_FLAG_DOTALL, LRE_FLAG_GLOBAL, LRE_FLAG_IGNORECASE, LRE_FLAG_INDICES, LRE_FLAG_MULTILINE,
    LRE_FLAG_STICKY, LRE_FLAG_UTF16, RegExpData, RegExpStringIterPayload,
    make_regexp_capture_array,
};

// /* create a RegExp object from a string containing the RegExp bytecode
// and the source pattern */
// fn js_regexp_constructor_internal(
//     ctx: &ContextImpl,
//     ctor: JSValue,
//     pattern: StringRef,
//     flags: JSValue,
// ) -> Result<RegExp, i32> {
//     if !flags.is_undefined() {
//         RegExp::create(
//             ctx,
//             ctor,
//             pattern.content().clone(),
//             flags.to_string(ctx)?.content().clone(),
//         )
//     } else {
//         RegExp::create(
//             ctx,
//             ctor,
//             pattern.content().clone(),
//             if flags.is_undefined() {
//                 Atom::CONST_empty_string.force_string(ctx)
//             } else {
//                 flags.to_string(ctx)
//             }?
//             .content()
//             .clone(),
//         )
//     }
// }

pub fn js_regexp_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let mut pat = *cc.arg(0);
    let mut flags1 = *cc.arg(1);

    let pat_is_regexp = js_is_regexp(ctx, &pat)?;

    if cc.this().is_undefined() {
        // called as a function
        let new_target_tmp = JS_GetActiveFunction(ctx);
        if pat_is_regexp && flags1.is_undefined() {
            let ctor = pat.get_property(ctx, Atom::CONST_constructor)?;
            if ctor.is_same(new_target_tmp) {
                return Ok(pat);
            }
        }
    }

    let mut pattern = StringData::Empty;
    let mut flags_str = StringData::Empty;

    let re = if let Some(re) = js_get_regexp(ctx, &pat, false) {
        if flags1.is_undefined() {
            Some(re)
        } else {
            pattern = re.pattern.clone();
            flags_str = if flags1.is_undefined() {
                re.flags_str.clone()
            } else {
                flags1.to_string(ctx).map(|s| s.content().clone())?
            };
            None
        }
    } else {
        if pat_is_regexp {
            pat = pat.get_property(ctx, Atom::CONST_source)?;
            if flags1.is_undefined() {
                flags1 = pat.get_property(ctx, Atom::CONST_flags)?;
            }
        }

        pattern = if pat.is_undefined() {
            StringData::Empty
        } else {
            pat.to_string(ctx)?.content().clone()
        };

        flags_str = if !flags1.is_undefined() {
            flags1.to_string(ctx)?.content().clone()
        } else {
            StringData::Empty
        };

        None
    };

    let re = if let Some(re) = re {
        re.clone()
    } else {
        let (re, flags) =
            RegExpData::compile_re(ctx, &pattern.to_utf8(false), flags_str.chars_lossy())?;
        RegExpData {
            regex: Rc::new(re),
            pattern,
            flags_str,
            flags,
        }
    };

    let mut obj = ctx
        .create_object_from_ctor(*cc.this(), ObjectPayload::REGEXP(Box::new(re)))
        .map_err(|_| -1)?;
    obj.define_property_value(
        ctx,
        Atom::CONST_lastIndex,
        JSValue::Int(0),
        JS_PROP_WRITABLE,
    )?;
    Ok(obj.into())
}

fn js_get_regexp<'a>(
    ctx: &ContextImpl,
    obj: &'a JSValue,
    throw_error: bool,
) -> Option<&'a RegExpData> {
    if let Some(ObjectPayload::REGEXP(re)) = obj.retrive_object_payload0(JS_CLASS_REGEXP) {
        Some(re)
    } else {
        if throw_error {
            ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP);
        }
        None
    }
}

pub fn js_is_regexp(ctx: &ContextImpl, obj: &JSValue) -> Result<bool, i32> {
    if let JSValue::Object(o) = obj {
        let m = o.get_property(ctx, Atom::CONST_Symbol_match)?;
        if !m.is_undefined() {
            m.to_bool()
        } else {
            Ok(js_get_regexp(ctx, obj, false).is_some())
        }
    } else {
        Ok(false)
    }
}

pub fn js_regexp_get_source(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let this = cc.this();

    if this.is_object() {
        if cc.this().is_same(
            ctx.class_proto(JS_CLASS_REGEXP)
                .map_or(JSValue::NULL, JSValue::Object),
        ) {
            // empty_regex
            return ctx.create_string_from_str("(?:))").map(JSValue::String);
        }

        let re = js_get_regexp(ctx, this, true).ok_or(-1)?;

        if re.pattern.is_empty() {
            return ctx.create_string_from_str("(?:))").map(JSValue::String);
        }

        let mut buf =
            StringBuf::with_capacity(re.pattern.len(), re.pattern.is_wide()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        /* Escape '/' and newline sequences as needed */
        let mut bra = false;
        let mut chars = re.pattern.iter();
        while let Some((_, c)) = chars.next() {
            match c {
                StringChar::Char('\\') => {
                    let _ = buf.push_char('\\');
                    if let Some((_, x)) = chars.next() {
                        let _ = buf.push_char(x);
                    }
                }
                StringChar::Char(']') => {
                    bra = false;
                    let _ = buf.push_char(']');
                }
                StringChar::Char('[') => {
                    if !bra {
                        if chars.peek() == Some(']' as u32 as u16) {
                            // [] case
                            chars.next(); // consume ']'
                            let _ = buf.push_str("[]");
                            continue;
                        }
                        bra = true;
                    }
                    let _ = buf.push_char('[');
                }
                StringChar::Char('\n') => {
                    let _ = buf.push_char('\n');
                }
                StringChar::Char('\r') => {
                    let _ = buf.push_char('\r');
                }

                StringChar::Char('/') => {
                    if bra {
                        let _ = buf.push_str("\\/");
                    } else {
                        let _ = buf.push_char('/');
                    }
                }
                c => {
                    let _ = buf.push_char(c);
                }
            }
        }

        ctx.create_string_with_buf(buf).map(JSValue::String)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_get_flag(cc: CallContext) -> JSResult {
    let mask = cc.magic();
    let ctx = cc.ctx();
    let this = cc.this();

    if this.is_object() {
        if let Some(re) = js_get_regexp(ctx, this, false) {
            Ok(JSValue::Bool((re.flags as i32) & mask != 0))
        } else if this.is_same(
            ctx.class_proto(JS_CLASS_REGEXP)
                .map_or(JSValue::NULL, JSValue::Object),
        ) {
            Ok(JSValue::UNDEFINED)
        } else {
            ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP);
            Err(-1)
        }
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_get_flags(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if let JSValue::Object(o) = cc.this() {
        const TESTS: [(&str, u8); 7] = [
            ("hasIndices", b'd'),
            ("global", b'g'),
            ("ignoreCase", b'i'),
            ("multiline", b'm'),
            ("dotAll", b's'),
            ("unicode", b'u'),
            ("sticky", b'y'),
        ];

        let mut str: [u8; 8] = [0; 8];
        let mut pos = 0_usize;

        for (name, ch) in TESTS.iter() {
            if o.get_property_str(ctx, name).and_then(|b| b.to_bool())? {
                str[pos] = *ch;
                pos += 1;
            }
        }

        ctx.create_string_from_str(std::str::from_utf8(&str[0..pos]).unwrap())
            .map(JSValue::String)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_toString(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if let JSValue::Object(_) = cc.this() {
        let Ok(mut buf) = StringBuf::with_capacity(96, false) else {
            ctx.throw_out_of_memory();
            return Err(-1);
        };

        let _ = buf.push_u8(b'/');

        let pattern = cc
            .this()
            .get_property(ctx, Atom::CONST_source)
            .and_then(|v| v.to_string(ctx))?;

        buf.push_content(pattern.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        buf.push_u8(b'/').map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let flags = cc
            .this()
            .get_property(ctx, Atom::CONST_flags)
            .and_then(|v| v.to_string(ctx))?;

        buf.push_content(flags.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        buf.shrink_to_fit();
        ctx.create_string_with_buf(buf).map(JSValue::String)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_regexp_compile(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let _ = js_get_regexp(ctx, cc.this(), true).ok_or(-1)?;

    let pattern1 = cc.arg(0);
    let flags1 = cc.arg(1);

    let pattern: StringData;
    let flags_str: StringData;

    if let Some(re1) = js_get_regexp(ctx, pattern1, false) {
        if flags1.is_undefined() {
            ctx.throw_type_error("flags must be undefined");
            return Err(-1);
        }

        pattern = re1.pattern.clone();
        flags_str = if flags1.is_undefined() {
            re1.flags_str.clone()
        } else {
            flags1.to_string(ctx)?.content().clone()
        }
    } else {
        pattern = if pattern1.is_undefined() {
            StringData::Empty
        } else {
            pattern1.to_string(ctx)?.content().clone()
        };
        flags_str = if flags1.is_undefined() {
            StringData::Empty
        } else {
            flags1.to_string(ctx)?.content().clone()
        };
    }

    let new_re = Box::new(RegExpData::create(ctx, pattern, flags_str)?);
    if let ObjectPayload::REGEXP(re0) = cc.this().as_object().unwrap().payload() {
        // replace the regexp data of payload
        unsafe {
            let ptr = re0 as *const Box<RegExpData> as *mut Box<RegExpData>;
            std::ptr::replace(ptr, new_re);
        }
    }

    cc.this()
        .set_property(ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;

    Ok(*cc.this())
}

pub fn js_regexp_exec(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let re = js_get_regexp(ctx, cc.this(), true).ok_or(-1)?;
    let str = cc.arg(0).to_string(ctx)?;

    let from = cc
        .this()
        .get_property(ctx, Atom::CONST_lastIndex)?
        .to_length(ctx)? as usize;

    if let Some(cap_list) = re.capture(str.content(), from) {
        let (arr, last_index) =
            make_regexp_capture_array(ctx, str, &cap_list, re.flags & LRE_FLAG_INDICES as u8 != 0)?;
        if re.flags & (LRE_FLAG_GLOBAL | LRE_FLAG_STICKY) as u8 != 0 {
            cc.this()
                .set_property(ctx, Atom::CONST_lastIndex, JSValue::from(last_index as u32))?;
        }
        Ok(arr)
    } else {
        cc.this()
            .set_property(ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
        Ok(JSValue::NULL)
    }
}

pub fn js_regexp_test(cc: CallContext) -> JSResult {
    js_regexp_exec(cc).map(|v| JSValue::Bool(!v.is_null()))
}

pub fn js_regexp_Symbol_match(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let str = cc.arg(0).to_string(ctx)?;

    let re = js_get_regexp(ctx, cc.this(), true).ok_or(-1)?;
    let exec = cc.this().get_property(ctx, Atom::CONST_exec)?;

    if !exec.is_function() || exec.match_native_callback(Callback::Func(js_regexp_exec), 0) {
        // fast path: use native regex find()
        if re.flags & LRE_FLAG_GLOBAL as u8 != 0 {
            // has g flag: use find(), returns array of match
            let found = re.find(str.content(), 0);
            if !found.is_empty() {
                let mut arr = ctx.create_array()?;
                let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
                let mut last_index = 0;

                for (i, range) in found.iter().enumerate() {
                    let val = str.sub_string(ctx, range.clone())?;
                    arr.define_property_value_u32(ctx, i as _, val.into(), prop_flags)?;
                    last_index = range.end;
                }

                cc.this().set_property(
                    ctx,
                    Atom::CONST_lastIndex,
                    JSValue::from(last_index as u32),
                )?;
                Ok(arr.into())
            } else {
                cc.this()
                    .set_property(ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
                Ok(JSValue::NULL)
            }
        } else {
            // use capture, returns exec data
            if let Some(cap_list) = re.capture(
                str.content(),
                if re.flags & LRE_FLAG_GLOBAL as u8 == 0 {
                    0
                } else {
                    let val = cc.this().get_property(ctx, Atom::CONST_lastIndex)?;
                    val.to_length(ctx)? as usize
                },
            ) {
                let (arr, last_index) = make_regexp_capture_array(
                    ctx,
                    str,
                    &cap_list,
                    re.flags & LRE_FLAG_INDICES as u8 != 0,
                )?;

                cc.this().set_property(
                    ctx,
                    Atom::CONST_lastIndex,
                    JSValue::from(last_index as u32),
                )?;
                Ok(arr)
            } else {
                cc.this()
                    .set_property(ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
                Ok(JSValue::NULL)
            }
        }
    } else {
        // slow path: use customized exec method, and collect matched string to array
        let mut arr = ctx.create_array()?;
        let args = [str.into()];

        let mut cnt = 0;
        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

        loop {
            match exec.call(ctx, *cc.this(), &args)? {
                JSValue::Object(o) => {
                    let last_index = cc
                        .this()
                        .get_property(ctx, Atom::CONST_lastIndex)?
                        .to_u32(ctx)? as usize;

                    let matched = o.get_property_u32(ctx, 0)?.to_string(ctx)?;
                    if matched.is_empty() {
                        // matched is empty: advance index manually
                        let unicode = re.flags & (LRE_FLAG_UTF16 as u8) != 0;
                        let next_index = str
                            .content()
                            .index_of_next(last_index, unicode)
                            .unwrap_or(last_index + 1);

                        cc.this().set_property(
                            ctx,
                            Atom::CONST_lastIndex,
                            JSValue::from(next_index as u32),
                        )?;
                    }

                    arr.define_property_value_u32(ctx, cnt, matched.into(), prop_flags)?;
                    cnt += 1;
                }
                JSValue::NULL => {
                    cc.this()
                        .set_property(ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
                    break;
                }
                _ => {
                    ctx.throw_type_error("RegExp exec method must return an object or null");
                    return Err(-1);
                }
            }
        }

        Ok(arr.into())
    }
}

pub fn js_regexp_Symbol_matchAll(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let re = js_get_regexp(ctx, cc.this(), true).ok_or(-1)?;
    let last_index = cc.this().get_property(ctx, Atom::CONST_lastIndex)?;

    let str = cc.arg(0).to_string(ctx)?;

    //
    // make matcher: regexp object
    //
    let regexp_ctor = ctx.regexp_ctor;
    let ctor = JS_SpeciesConstructor(ctx, *cc.this(), regexp_ctor)?;
    let regexp = JS_CallConstructor(
        ctx,
        ctor,
        &[
            *cc.this(),
            ctx.create_string_with_content(re.flags_str.clone())?.into(),
        ],
    )
    .err()?;
    regexp.set_property(ctx, Atom::CONST_lastIndex, last_index)?;

    // make regexp string iterator object
    ctx.create_object(ObjectPayload::REGEXP_STRING_ITERATOR(
        RegExpStringIterPayload::new(
            regexp.try_into().unwrap(),
            str,
            re.flags & LRE_FLAG_GLOBAL as u8 != 0,
            re.flags & LRE_FLAG_UTF16 as u8 != 0,
        ),
    ))
    .map(JSValue::Object)
    .map_err(|_| -1)
}

fn js_is_standard_regexp(ctx: &ContextImpl, rx: ObjectRef) -> Result<bool, i32> {
    let val = rx.get_property(ctx, Atom::CONST_constructor)?;
    if val.is_same(ctx.regexp_ctor) {
        let f = rx.get_property(ctx, Atom::CONST_exec)?;
        Ok(f.match_native_callback(Callback::Func(js_regexp_exec), 0))
    } else {
        Ok(false)
    }
}

pub fn js_regexp_Symbol_replace(cc: CallContext) -> JSResult {
    // [Symbol.replace](str, rep)
    let ctx = cc.ctx();

    let Some(this) = cc.this().as_object().copied() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let str = cc.arg(0).to_string(ctx)?;

    let rep = *cc.arg(1);
    let rep_is_func = rep.is_function();
    let rep_str: Option<StringData> = if !rep_is_func {
        Some(rep.to_string(ctx)?.content().clone())
    } else {
        None
    };

    let flags = this
        .get_property(ctx, Atom::CONST_flags)
        .and_then(|v| v.to_string(ctx))?;

    let is_global = flags.content().index_of_char('g', 0).is_some();
    let is_unicode = if is_global {
        this.set_property(ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
        flags.content().index_of_char('u', 0).is_some()
    } else {
        false
    };

    // if rep_str.as_ref().map_or(false, |s| s.is_empty())
    //     && is_global
    //     && js_is_standard_regexp(ctx, this)?
    // {
    //     // fast path: delete only
    //     // return JS_RegExpDelete(ctx, this, sp);
    //     todo!();
    // }

    let mut sbuf = StringBuf::with_capacity(str.len(), false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    // regexp match results list
    let mut matches = SmallVec::<[ObjectRef; 10]>::new();
    while let Ok(m) = ObjectRef::try_from(JS_RegExpExec(ctx, this, str)?) {
        if let Ok(s) = StringRef::try_from(m.get_property_u32(ctx, 0)?)
            && s.is_empty()
        {
            // always advance of at least one char
            let last_index = this
                .get_property(ctx, Atom::CONST_lastIndex)?
                .to_length(ctx)?;

            let next_index = str
                .content()
                .index_of_next(last_index as usize, is_unicode)
                .unwrap_or(last_index as usize + 1);
            this.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(next_index as u32))?;
        }

        matches.push(m);
        if !is_global {
            break;
        }
    }

    let mut pos = 0;

    for mut m in matches {
        let matched = m.get_property_u32(ctx, 0).and_then(|v| v.to_string(ctx))?;

        let index = m
            .get_property(ctx, Atom::CONST_index)
            .and_then(|v| v.to_u32(ctx))? as usize;

        let length = m
            .get_property(ctx, Atom::CONST_length)
            .and_then(|v| v.to_u32(ctx))? as usize;

        sbuf.push_content_range(str.content(), pos..index)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        pos = index + matched.len();

        if let Some(rep_str) = rep_str.as_ref() {
            // replacement string
            let groups = ObjectRef::try_from(m.get_property(ctx, Atom::CONST_groups)?).ok();

            let t = StringData::substitution_buf(
                matched.content(),
                str.content(),
                index,
                rep_str,
                |n| {
                    if n < length {
                        let v = m.get_property_u32(ctx, n as _)?;
                        if !v.is_undefined() {
                            Ok(Some(v.to_string(ctx)?.content().clone()))
                        } else {
                            Ok(None)
                        }
                    } else {
                        Ok(None)
                    }
                },
                length,
                |name| {
                    if let Some(g) = groups.as_ref() {
                        let val = g.get_property_str(ctx, name)?;
                        if !val.is_undefined() {
                            Ok(Some(val.to_string(ctx)?.content().clone()))
                        } else {
                            Ok(None)
                        }
                    } else {
                        Ok(None)
                    }
                },
            )
            .map_err(|e| {
                if e.is_some() {
                    ctx.throw_out_of_memory();
                }
                -1
            })?;

            sbuf.push(&t).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        } else {
            // functional replacement.

            // append extra argument `index`, `str` to array
            m.define_property_value_i64(
                ctx,
                length as i64,
                JSValue::Int(index as i32),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;
            m.define_property_value_i64(
                ctx,
                length as i64 + 1,
                str.into(),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;

            // apply replacement function
            let t = js_function_apply(CallContext::new(
                cc.ctx_ptr(),
                rep,
                &[JSValue::UNDEFINED, m.into()],
                0,
                None,
            ))?
            .to_string(ctx)?;

            sbuf.push_content(t.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        };
    }

    // concat remaining part of `str`
    if pos < str.content().len() {
        sbuf.push_content_range(str.content(), pos..str.content().len())
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::String)
}

fn js_regexp_Symbol_search(cc: CallContext) -> JSResult {
    let cx = cc.ctx();

    let Some(this) = cc.this().as_object().copied() else {
        cx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let prev_last_index = this
        .get_property(cx, Atom::CONST_lastIndex)?
        .to_array_index(cx)? as usize;

    if prev_last_index != 0 {
        this.set_property(cx, Atom::CONST_lastIndex, JSValue::Int(0))?;
    }

    let str = cc.arg(0).to_string(cx)?;

    let m = JS_RegExpExec(cx, this, str)?;
    let cur_last_index = this
        .get_property(cx, Atom::CONST_lastIndex)?
        .to_array_index(cx)? as usize;

    if cur_last_index != prev_last_index {
        // restore original lastIndex
        this.set_property(
            cx,
            Atom::CONST_lastIndex,
            JSValue::from(prev_last_index as u32),
        )?;
    }

    if m.is_null() {
        Ok(JSValue::Int(-1))
    } else {
        let index = m.get_property(cx, Atom::CONST_index)?.to_array_index(cx)?;
        Ok(JSValue::Int(index as i32))
    }
}

/// [Symbol.split](str, limit)
pub fn js_regexp_Symbol_split(cc: CallContext) -> JSResult {
    let cx = cc.ctx();

    let Some(this_obj) = cc.this().as_object() else {
        cx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let str = cc.arg(0).to_string(cx)?;
    let limit = cc.arg(1);
    let limit = if limit.is_undefined() {
        usize::MAX
    } else {
        limit.to_u32(cx)? as usize
    };

    // make RegExp as splitter
    let (splitter, flag_unicode) = {
        let mut flags = this_obj
            .get_property(cx, Atom::CONST_flags)?
            .to_string(cx)?;
        let u = flags.content().index_of_char('u', 0).is_some();
        if flags.content().index_of_char('y', 0).is_none() {
            flags = JS_ConcatString3(cx, c"", flags.into(), c"y")?;
        }

        let regexp_ctor = cx.regexp_ctor;
        let ctor = JS_SpeciesConstructor(cx, *cc.this(), regexp_ctor)?;

        let re = JS_CallConstructor(cx, ctor, &[*cc.this(), flags.into()]).err()?;
        (TryInto::<ObjectRef>::try_into(re).unwrap(), u)
    };

    let mut arr = cx.create_array()?;
    if limit == 0 {
        return Ok(arr.into());
    }

    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
    let mut idx = 0_usize;

    if str.is_empty() {
        let z = JS_RegExpExec(cx, splitter, str)?;
        if z.is_null() {
            // not match: add tail, must be an empty str
            arr.define_property_value_u32(
                cx,
                idx as u32,
                Atom::CONST_empty_string.force_string(cx).unwrap().into(),
                prop_flags,
            )?;
        }
        return Ok(arr.into());
    }

    let len = str.len();
    let mut split_start = 0;
    let mut last_index = 0;

    while last_index < len {
        // match regexp from `pos`
        let m = {
            splitter.set_property(cx, Atom::CONST_lastIndex, JSValue::Int(last_index as i32))?;
            JS_RegExpExec(cx, splitter, str)?
        };

        if m.is_null() {
            // no match: advance index
            last_index = str
                .content()
                .index_of_next(last_index, flag_unicode)
                .unwrap_or(len);
        } else {
            let mut next_index = splitter
                .get_property(cx, Atom::CONST_lastIndex)?
                .to_length(cx)? as usize;

            if next_index > len {
                next_index = len;
            }
            if next_index == split_start {
                // empty: advance index
                last_index = str
                    .content()
                    .index_of_next(last_index, flag_unicode)
                    .unwrap_or(len);
            } else {
                let sub = str.sub_string(cx, split_start..last_index)?;
                arr.define_property_value_u32(cx, idx as u32, sub.into(), prop_flags)?;
                idx += 1;
                if idx == limit {
                    return Ok(arr.into());
                }

                // append captures 1.. to array
                let ncaps = m.get_property(cx, Atom::CONST_length)?.to_length(cx)? as usize;
                for c in 1..ncaps {
                    let sub = m.get_property_u32(cx, c as u32)?;
                    arr.define_property_value_u32(cx, idx as _, sub, prop_flags)?;
                    idx += 1;
                    if idx == limit {
                        return Ok(arr.into());
                    }
                }

                split_start = next_index;
                last_index = next_index;
            }
        }
    }

    // remaining part of str
    split_start = std::cmp::min(split_start, len);
    let rem = str.sub_string(cx, split_start..len)?;
    arr.define_property_value_u32(cx, idx as u32, rem.into(), prop_flags)?;

    Ok(arr.into())
}

pub static js_regexp_proto_funcs: [FieldDef; 18] = [
    FieldDef::GETSET("flags", Some(js_regexp_get_flags), None, 0),
    FieldDef::GETSET("source", Some(js_regexp_get_source), None, 0),
    FieldDef::GETSET(
        "global",
        Some(js_regexp_get_flag),
        None,
        LRE_FLAG_GLOBAL as _,
    ),
    FieldDef::GETSET(
        "ignoreCase",
        Some(js_regexp_get_flag),
        None,
        LRE_FLAG_IGNORECASE as _,
    ),
    FieldDef::GETSET(
        "multiline",
        Some(js_regexp_get_flag),
        None,
        LRE_FLAG_MULTILINE as _,
    ),
    FieldDef::GETSET(
        "dotAll",
        Some(js_regexp_get_flag),
        None,
        LRE_FLAG_DOTALL as _,
    ),
    FieldDef::GETSET(
        "unicode",
        Some(js_regexp_get_flag),
        None,
        LRE_FLAG_UTF16 as _,
    ),
    FieldDef::GETSET(
        "sticky",
        Some(js_regexp_get_flag),
        None,
        LRE_FLAG_STICKY as _,
    ),
    FieldDef::GETSET(
        "hasIndices",
        Some(js_regexp_get_flag),
        None,
        LRE_FLAG_INDICES as _,
    ),
    FieldDef::CALL("exec", 1, js_regexp_exec, 0),
    FieldDef::CALL("compile", 2, js_regexp_compile, 0),
    FieldDef::CALL("test", 1, js_regexp_test, 0),
    FieldDef::CALL("toString", 0, js_regexp_toString, 0),
    FieldDef::CALL("[Symbol.replace]", 2, js_regexp_Symbol_replace, 0),
    FieldDef::CALL("[Symbol.match]", 1, js_regexp_Symbol_match, 0),
    FieldDef::CALL("[Symbol.matchAll]", 1, js_regexp_Symbol_matchAll, 0),
    FieldDef::CALL("[Symbol.search]", 1, js_regexp_Symbol_search, 0),
    FieldDef::CALL("[Symbol.split]", 2, js_regexp_Symbol_split, 0),
    //JS_CGETSET_DEF("__source", js_regexp_get___source, NULL ),
    //JS_CGETSET_DEF("__flags", js_regexp_get___flags, NULL ),
];

pub static js_regexp_funcs: [FieldDef; 1] = [
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
    //JS_CFUNC_DEF("__RegExpExec", 2, js_regexp___RegExpExec ),
    //JS_CFUNC_DEF("__RegExpDelete", 2, js_regexp___RegExpDelete ),
];

/// invoke regexp.exec() method with a string, if `exec()` method is not callable,
/// call js_regexp_exec() instead.
/// result must be object or null.
pub(super) fn JS_RegExpExec(ctx: &ContextImpl, re: ObjectRef, str: StringRef) -> JSResult {
    let method = re.get_property(ctx, Atom::CONST_exec)?;
    let args: JSValue = str.into();

    if method.is_function() {
        let ret = method.call(ctx, re.into(), std::slice::from_ref(&args))?;
        if !ret.is_object() && !ret.is_null() {
            ctx.throw_type_error("RegExp exec method must return an object or null");
            Err(-1)
        } else {
            Ok(ret)
        }
    } else {
        let ctx = unsafe { ContextPtr::from_ref_unchecked(ctx) };
        js_regexp_exec(CallContext::new(
            ctx,
            re.into(),
            std::slice::from_ref(&args),
            0,
            None,
        ))
    }
}
