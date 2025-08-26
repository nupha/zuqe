use std::{
    ops::{Deref, Range},
    rc::Rc,
};

use smallvec::SmallVec;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_REGEXP,
    atom::consts::*,
    context::{JS_GetActiveFunction, JSContext},
    error::{ErrorKind, JSResult},
    function::{CallContext, FieldDef, JS_CallConstructor, NativeFunctionType, js_function_apply},
    js_core::js_get_this,
    object::{
        JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JS_SpeciesConstructor, JSObject,
        ObjectDetail,
    },
    runtime::JSRuntime,
    string::{JS_ConcatString3, JSString, StringBuf, StringChar, StringContent},
    value::JSValue,
};

use super::{
    JSRegExp, JSRegExpStringIteratorData, LRE_FLAG_DOTALL, LRE_FLAG_GLOBAL, LRE_FLAG_IGNORECASE,
    LRE_FLAG_INDICES, LRE_FLAG_MULTILINE, LRE_FLAG_STICKY, LRE_FLAG_UTF16,
};

/* create a RegExp object from a string containing the RegExp bytecode
and the source pattern */
pub fn js_regexp_constructor_internal(
    ctx: &JSContext,
    ctor: &JSValue,
    pattern: &JSString,
    flags: &JSValue,
) -> Result<JSObject, i32> {
    if !flags.is_undefined() {
        ctx.create_regexp(ctor, pattern, &flags.to_string(ctx)?)
    } else {
        ctx.create_regexp(ctor, pattern, &JS_ATOM_empty_string.to_string(ctx).unwrap())
    }
}

pub fn js_regexp_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut pat = cc.arg(0).clone();
    let mut flags1 = cc.arg(1).clone();

    let pat_is_regexp = js_is_regexp(ctx, &pat)?;

    if cc.this().is_undefined() {
        // called as a function
        let new_target_tmp = unsafe { JS_GetActiveFunction(ctx) };
        if pat_is_regexp && flags1.is_undefined() {
            let ctor = pat.get_property(ctx, JS_ATOM_constructor)?;
            if ctor.is_same(new_target_tmp) {
                return Ok(pat.clone());
            }
        }
    }

    let mut pattern: StringContent = Rc::new(StringBuf::Empty);
    let mut flags_str: StringContent = Rc::new(StringBuf::Empty);

    let re = if let Some(re) = pat.object().and_then(|o| js_get_regexp(ctx, o, false)) {
        if flags1.is_undefined() {
            Some(re)
        } else {
            pattern = re.pattern.clone();
            flags_str = if flags1.is_undefined() {
                re.flags_str.clone()
            } else {
                TryInto::<JSString>::try_into(flags1)
                    .unwrap()
                    .content()
                    .clone()
            };
            None
        }
    } else {
        if pat_is_regexp {
            pat = pat.get_property(ctx, JS_ATOM_source)?;
            if flags1.is_undefined() {
                flags1 = pat.get_property(ctx, JS_ATOM_flags)?;
            }
        }

        pattern = if pat.is_undefined() {
            Rc::new(StringBuf::Empty)
        } else {
            pat.to_string(ctx)?.content().clone()
        };

        flags_str = if !flags1.is_undefined() {
            flags1.to_string(ctx)?.content().clone()
        } else {
            Rc::new(StringBuf::Empty)
        };

        None
    };

    let re = if let Some(re) = re {
        re
    } else {
        let (re, flags) = JSRegExp::new_re(ctx, &pattern.to_utf8(), flags_str.chars_lossy())
            .map_err(|e| {
                ctx.throw_error(ErrorKind::Syntax, &e.to_string());
                -1
            })?;

        JSRegExp {
            regex: Rc::new(re),
            pattern,
            flags_str,
            flags,
        }
    };

    let obj = ctx
        .create_object_apply_ctor(cc.this(), ObjectDetail::REGEXP(Box::new(re)))
        .map_err(|_| -1)?;
    obj.define_property_value(ctx, JS_ATOM_lastIndex, JSValue::Int(0), JS_PROP_WRITABLE)?;
    Ok(obj.into())
}

pub fn js_get_regexp(ctx: &JSContext, obj: &JSObject, throw_error: bool) -> Option<JSRegExp> {
    if let ObjectDetail::REGEXP(re) = obj.0.payload.borrow().deref() {
        Some(*re.clone())
    } else {
        if throw_error {
            ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP);
        }
        None
    }
}

pub fn js_is_regexp(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    if let JSValue::Object(o) = obj {
        let m = o.get_property(ctx, JS_ATOM_Symbol_match)?;
        if !m.is_undefined() {
            m.to_bool()
        } else {
            Ok(js_get_regexp(ctx, o, false).is_some())
        }
    } else {
        Ok(false)
    }
}

pub fn js_regexp_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {}

pub fn js_regexp_iterator_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {}

pub fn js_regexp_iterator_mark(val: JSValue) {
    // unsafe {
    //     if let ObjectDetail::REGEXP_STRING_ITERATOR(p) =
    //         val.object_unchecked().0.payload.borrow().deref()
    //     {
    //         //
    //     }
    // }
}

pub fn js_regexp_get_source(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this = cc.this();

    if let JSValue::Object(o) = this {
        if cc
            .this()
            .is_same(&ctx.0.borrow().class_proto[JS_CLASS_REGEXP as usize])
        {
            // empty_regex
            return ctx.create_string_from_str("(?:))").map(JSValue::String);
        }

        let re = js_get_regexp(ctx, o, true).ok_or(-1)?;
        let p = re.pattern;
        if p.is_empty() {
            return ctx.create_string_from_str("(?:))").map(JSValue::String);
        }

        let mut buf = StringBuf::with_capacity(p.buf_len() * 3 / 2, p.is_wide()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        /* Escape '/' and newline sequences as needed */
        let mut bra = false;
        let mut chars = p.iter();
        while let Some((_, c)) = chars.next() {
            match c {
                StringChar::Char('\\') => {
                    buf.push_char('\\');
                    if let Some((_, x)) = chars.next() {
                        buf.push_char(x);
                    }
                }
                StringChar::Char(']') => {
                    bra = false;
                    buf.push_char(']');
                }
                StringChar::Char('[') => {
                    if !bra {
                        if chars.peek() == Some(']' as u32 as u16) {
                            // [] case
                            chars.next(); // consume ']'
                            buf.push_str("[]");
                            continue;
                        }
                        bra = true;
                    }
                    buf.push_char('[');
                }
                StringChar::Char('\n') => {
                    buf.push_char('\n');
                }
                StringChar::Char('\r') => {
                    buf.push_char('\r');
                }

                StringChar::Char('/') => {
                    if bra {
                        buf.push_str("\\/");
                    } else {
                        buf.push_char('/');
                    }
                }
                c => {
                    buf.push_char(c);
                }
            }
        }

        buf.shrink_to_fit();
        ctx.create_string_with_buf(buf).map(JSValue::String)
    } else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }
}

pub fn js_regexp_get_flag(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mask = cc.magic();
    let this = cc.this();

    if let JSValue::Object(o) = this {
        if let Some(re) = js_get_regexp(ctx, o, false) {
            Ok(JSValue::Bool((re.flags as i32) & mask != 0))
        } else {
            if this.is_same(&ctx.0.borrow().class_proto[JS_CLASS_REGEXP as usize]) {
                Ok(JSValue::UNDEFINED)
            } else {
                ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP);
                Err(-1)
            }
        }
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_get_flags(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(o) = cc.this() {
        const TESTS: [(&str, u8); 7] = [
            ("hasIndices", 'd' as u8),
            ("global", 'g' as u8),
            ("ignoreCase", 'i' as u8),
            ("multiline", 'm' as u8),
            ("dotAll", 's' as u8),
            ("unicode", 'u' as u8),
            ("sticky", 'y' as u8),
        ];

        let mut str: [u8; 8] = [0; 8];
        let mut pos = 0_usize;

        for (name, ch) in TESTS.iter() {
            if o.get_property_str(ctx, *name).and_then(|b| b.to_bool())? {
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

pub fn js_regexp_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(_) = cc.this() {
        let Ok(mut buf) = StringBuf::with_capacity(96, false) else {
            ctx.throw_out_of_memory();
            return Err(-1);
        };

        let _ = buf.push_u8('/' as u8);

        let pattern = cc
            .this()
            .get_property(ctx, JS_ATOM_source)
            .and_then(|v| v.to_string(ctx))?;

        buf.push(&pattern.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        buf.push_u8('/' as u8).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let flags = cc
            .this()
            .get_property(ctx, JS_ATOM_flags)
            .and_then(|v| v.to_string(ctx))?;

        buf.push(&flags.content()).map_err(|_| {
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

fn js_regexp_compile(ctx: &JSContext, cc: CallContext) -> JSResult {
    let pattern1 = cc.arg(0);
    let flags1 = cc.arg(1);

    let pattern: StringContent;
    let flags_str: StringContent;

    if let Some(re0) = pattern1.object().and_then(|o| js_get_regexp(ctx, o, false)) {
        if flags1.is_undefined() {
            ctx.throw_type_error("flags must be undefined");
            return Err(-1);
        }

        pattern = re0.pattern.clone();
        flags_str = if flags1.is_undefined() {
            re0.flags_str.clone()
        } else {
            flags1.to_string(ctx)?.content().clone()
        }
    } else {
        pattern = if pattern1.is_undefined() {
            Rc::new(StringBuf::Empty)
        } else {
            pattern1.to_string(ctx)?.content().clone()
        };
        flags_str = if flags1.is_undefined() {
            Rc::new(StringBuf::Empty)
        } else {
            flags1.to_string(ctx)?.content().clone()
        };
    }

    let re = Box::new(JSRegExp::new(ctx, pattern, flags_str)?);
    *cc.this().object().unwrap().0.payload.borrow_mut() = ObjectDetail::REGEXP(re);

    cc.this()
        .set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;

    Ok(cc.this().clone())
}

fn make_regexp_capture_array(
    ctx: &JSContext,
    text: JSString,
    cap_list: &[(Option<Range<usize>>, Option<&str>)],
) -> Result<(JSValue, usize), i32> {
    let arr = ctx.create_array(0)?;
    let mut group = JSValue::UNDEFINED;

    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
    let mut last_index = 0;

    for (i, (range, name)) in cap_list.iter().enumerate() {
        let val: JSValue = if let Some(range) = range {
            if i == 0 {
                arr.0.define_property_value(
                    ctx,
                    JS_ATOM_index,
                    JSValue::Int(range.start as i32),
                    prop_flags,
                )?;
                last_index = range.end;
            }

            text.sub_string(ctx, range.clone())?.into()
        } else {
            JSValue::UNDEFINED
        };

        arr.0
            .define_property_value_u32(ctx, i as _, val.clone(), prop_flags)?;

        if let Some(name) = name {
            if group.is_undefined() {
                group = ctx.create_base_object()?.into();
            }
            group.define_property_value_str(ctx, name, val, prop_flags)?;
        }
    }

    arr.0
        .define_property_value(ctx, JS_ATOM_groups, group, prop_flags)?;
    arr.0
        .define_property_value(ctx, JS_ATOM_input, text.into(), prop_flags)?;

    Ok((arr.into(), last_index))
}

pub fn js_regexp_exec(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    let str = cc.arg(0).to_string(ctx)?;

    let from = {
        let val = cc.this().get_property(ctx, JS_ATOM_lastIndex)?;
        val.coerce_to_length(ctx)? as usize
    };
    if let Some(cap_list) = re.capture(&str, from) {
        let (arr, last_index) = make_regexp_capture_array(ctx, str, &cap_list)?;
        if re.flags & (LRE_FLAG_GLOBAL | LRE_FLAG_STICKY) as u8 != 0 {
            this.set_property(ctx, JS_ATOM_lastIndex, JSValue::from(last_index as u32))?;
        }
        Ok(arr)
    } else {
        this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
        Ok(JSValue::NULL)
    }
}

pub fn js_regexp_test(ctx: &JSContext, cc: CallContext) -> JSResult {
    js_regexp_exec(ctx, cc).map(|v| JSValue::Bool(v.is_null()))
}

pub fn js_regexp_Symbol_match(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let str = cc.arg(0).to_string(ctx)?;

    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    let exec = this.get_property(ctx, JS_ATOM_exec)?;

    if !exec.is_callable()
        || exec.match_native_callback(NativeFunctionType::Func(js_regexp_exec), 0)
    {
        // fast path: use native regex find()
        if re.flags & LRE_FLAG_GLOBAL as u8 != 0 {
            // has g flag: use find(), returns array of match
            let found = re.find(&str, 0);
            if !found.is_empty() {
                let arr = ctx.create_array(0)?;
                let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
                let mut last_index = 0;

                for (i, range) in found.iter().enumerate() {
                    let val = str.sub_string(ctx, range.clone())?;

                    arr.0
                        .define_property_value_u32(ctx, i as _, val.into(), prop_flags)?;
                    last_index = range.end;
                }

                this.set_property(ctx, JS_ATOM_lastIndex, JSValue::from(last_index as u32))?;
                Ok(arr.into())
            } else {
                this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
                Ok(JSValue::NULL)
            }
        } else {
            // use capture, returns exec data
            if let Some(cap_list) = re.capture(
                &str,
                if re.flags & LRE_FLAG_GLOBAL as u8 == 0 {
                    0
                } else {
                    let val = cc.this().get_property(ctx, JS_ATOM_lastIndex)?;
                    val.coerce_to_length(ctx)? as usize
                },
            ) {
                let (arr, last_index) = make_regexp_capture_array(ctx, str, &cap_list)?;
                this.set_property(ctx, JS_ATOM_lastIndex, JSValue::from(last_index as u32))?;
                Ok(arr)
            } else {
                this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
                Ok(JSValue::NULL)
            }
        }
    } else {
        // slow path: use customized exec method, and collect matched string to array
        let arr = ctx.create_array(0)?;
        let args = [str.clone().into()];

        let mut cnt = 0;
        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

        loop {
            match exec.call(ctx, &cc.this(), &args)? {
                JSValue::Object(o) => {
                    let last_index =
                        this.get_property(ctx, JS_ATOM_lastIndex)?.to_u32(ctx)? as usize;

                    let matched = o.get_property_u32(ctx, 0)?.to_string(ctx)?;
                    if matched.is_empty() {
                        // matched is empty: advance index manually
                        let unicode = re.flags & (LRE_FLAG_UTF16 as u8) != 0;
                        let next_index = str
                            .content()
                            .index_of_next(last_index, unicode)
                            .unwrap_or(last_index + 1);

                        this.set_property(
                            ctx,
                            JS_ATOM_lastIndex,
                            JSValue::from(next_index as u32),
                        )?;
                    }

                    arr.0
                        .define_property_value_u32(ctx, cnt, matched.into(), prop_flags)?;
                    cnt += 1;
                }
                JSValue::NULL => {
                    this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
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

pub fn js_regexp_Symbol_matchAll(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    let last_index = this.get_property(ctx, JS_ATOM_lastIndex)?;

    let str = cc.arg(0).to_string(ctx)?;

    //
    // make matcher: regexp object
    //
    let regexp_ctor = ctx.0.borrow().regexp_ctor.clone();
    let ctor = JS_SpeciesConstructor(ctx, cc.this(), regexp_ctor)?;
    let matcher = JS_CallConstructor(
        ctx,
        &ctor,
        &[
            this.clone().into(),
            ctx.create_string_with_content(re.flags_str.clone())?.into(),
        ],
    )
    .err()?;
    matcher.set_property(ctx, JS_ATOM_lastIndex, last_index)?;

    // make regexp string iterator object
    let isa = JSRegExpStringIteratorData {
        iterating_regexp: matcher.try_into().unwrap(),
        iterated_string: str.content().clone(),
        global: re.flags & LRE_FLAG_GLOBAL as u8 != 0,
        unicode: re.flags & LRE_FLAG_UTF16 as u8 != 0,
        done: false,
    };
    let iter = ctx
        .create_object(ObjectDetail::REGEXP_STRING_ITERATOR(Box::new(isa)))
        .map_err(|_| -1)?;

    Ok(iter.into())
}

fn js_is_standard_regexp(ctx: &JSContext, rx: &JSObject) -> Result<bool, i32> {
    let val = rx.get_property(ctx, JS_ATOM_constructor)?;
    if val.is_same(&ctx.0.borrow().regexp_ctor) {
        let f = rx.get_property(ctx, JS_ATOM_exec)?;
        Ok(f.match_native_callback(NativeFunctionType::Func(js_regexp_exec), 0))
    } else {
        Ok(false)
    }
}

pub fn js_regexp_Symbol_replace(ctx: &JSContext, cc: CallContext) -> JSResult {
    // [Symbol.replace](str, rep)
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let str = cc.arg(0).to_string(ctx)?;

    let rep = cc.arg(1);
    let rep_is_func = rep.is_callable();
    let rep_str: Option<StringContent> = if !rep_is_func {
        Some(rep.to_string(ctx)?.content().clone())
    } else {
        None
    };

    let flags = this
        .get_property(ctx, JS_ATOM_flags)
        .and_then(|v| v.to_string(ctx))?;

    let is_global = flags.content().index_of_char('g', 0).is_some();
    let is_unicode = if is_global {
        this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
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

    let mut sbuf =
        StringBuf::with_capacity(str.buf_len() * 3 / 2, str.is_wide()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

    // regexp match results list
    let mut matches = SmallVec::<[JSObject; 10]>::new();
    while let Ok(m) = TryInto::<JSObject>::try_into(JS_RegExpExec(ctx, this.clone(), str.clone())?)
    {
        if let Ok(s) = TryInto::<JSString>::try_into(m.get_property_u32(ctx, 0)?) {
            if s.is_empty() {
                // always advance of at least one char
                let last_index = this
                    .get_property(ctx, JS_ATOM_lastIndex)?
                    .coerce_to_length(ctx)?;

                let next_index = str
                    .content()
                    .index_of_next(last_index as usize, is_unicode)
                    .unwrap_or(last_index as usize + 1);
                this.set_property(ctx, JS_ATOM_lastIndex, JSValue::from(next_index as u32))?;
            }
        }

        matches.push(m);
        if !is_global {
            break;
        }
    }

    let mut pos = 0;

    for m in matches {
        let matched = m.get_property_u32(ctx, 0).and_then(|v| v.to_string(ctx))?;

        let index = m
            .get_property(ctx, JS_ATOM_index)
            .and_then(|v| v.to_u32(ctx))? as usize;

        let length = m
            .get_property(ctx, JS_ATOM_length)
            .and_then(|v| v.to_u32(ctx))? as usize;

        sbuf.push_content_slice(&str.content().slice(pos..index))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        pos = index + matched.buf_len();

        if let Some(rep_str) = rep_str.as_ref() {
            // replacement string

            let groups = TryInto::<JSObject>::try_into(m.get_property(ctx, JS_ATOM_groups)?).ok();

            let t = StringBuf::substitution_buf(
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
                        let val = g.get_property_str(ctx, name).unwrap();
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
                str.clone().into(),
                JS_PROP_C_W_E | JS_PROP_THROW,
            )?;

            // apply replacement function
            let t = js_function_apply(
                ctx,
                CallContext::new(ctx, rep.clone(), &[JSValue::UNDEFINED, m.into()], 0, None),
            )?
            .to_string(ctx)?;

            sbuf.push(&t.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        };
    }

    // concat remaining part of `str`
    if pos < str.content().buf_len() {
        sbuf.push_content_slice(&str.content().slice(pos..str.content().buf_len()))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::String)
}

fn js_regexp_Symbol_search(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let prev_last_index = this
        .get_property(ctx, JS_ATOM_lastIndex)?
        .coerce_to_index(ctx)? as usize;

    if prev_last_index != 0 {
        this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
    }

    let str = cc.arg(0).to_string(ctx)?;

    let m = JS_RegExpExec(ctx, this.clone(), str.clone())?;
    let cur_last_index = this
        .get_property(ctx, JS_ATOM_lastIndex)?
        .coerce_to_index(ctx)? as usize;

    if cur_last_index != prev_last_index {
        // restore original lastIndex
        this.set_property(
            ctx,
            JS_ATOM_lastIndex,
            JSValue::from(prev_last_index as u32),
        )?;
    }

    if m.is_null() {
        Ok(JSValue::Int(-1))
    } else {
        let index = m.get_property(ctx, JS_ATOM_index)?.coerce_to_index(ctx)?;
        Ok(JSValue::Int(index as i32))
    }
}

/// [Symbol.split](str, limit)
pub fn js_regexp_Symbol_split(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this_obj) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let str = cc.arg(0).to_string(ctx)?;
    let limit = cc.arg(1);
    let limit = if limit.is_undefined() {
        usize::MAX
    } else {
        limit.to_u32(ctx)? as usize
    };

    // make RegExp as splitter
    let (splitter, flag_unicode) = {
        let mut flags = this_obj.get_property(ctx, JS_ATOM_flags)?.to_string(ctx)?;
        let u = flags.content().index_of_char('u', 0).is_some();
        if flags.content().index_of_char('y', 0).is_none() {
            flags = JS_ConcatString3(ctx, c"", flags.into(), c"y")?;
        }

        let regexp_ctor = ctx.0.borrow().regexp_ctor.clone();
        let ctor = JS_SpeciesConstructor(ctx, cc.this(), regexp_ctor)?;

        let re = JS_CallConstructor(ctx, &ctor, &[cc.this().clone(), flags.into()]).err()?;
        (TryInto::<JSObject>::try_into(re).unwrap(), u)
    };

    let arr = ctx.create_array(0)?;
    if limit == 0 {
        return Ok(arr.into());
    }

    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
    let mut idx = 0_usize;

    if str.is_empty() {
        let z = JS_RegExpExec(ctx, splitter.clone(), str.clone())?;
        if z.is_null() {
            // not match: add tail, must be an empty str
            arr.0.define_property_value_u32(
                ctx,
                idx as u32,
                JS_ATOM_empty_string.to_string(ctx).unwrap().into(),
                prop_flags,
            )?;
        }
        return Ok(arr.into());
    }

    let len = str.buf_len();
    let mut split_start = 0;
    let mut last_index = 0;

    while last_index < len {
        // match regexp from `pos`
        let m = {
            splitter.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(last_index as i32))?;
            JS_RegExpExec(ctx, splitter.clone(), str.clone())?
        };

        if m.is_null() {
            // no match: advance index
            last_index = str
                .content()
                .index_of_next(last_index, flag_unicode)
                .unwrap_or(len);
        } else {
            let mut next_index = splitter
                .get_property(ctx, JS_ATOM_lastIndex)?
                .coerce_to_length(ctx)? as usize;

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
                let sub = str.sub_string(ctx, split_start..last_index)?;
                arr.0
                    .define_property_value_u32(ctx, idx as u32, sub.into(), prop_flags)?;
                idx += 1;
                if idx == limit {
                    return Ok(arr.into());
                }

                // append captures 1.. to array
                let ncaps = m.get_property(ctx, JS_ATOM_length)?.coerce_to_length(ctx)? as usize;
                for c in 1..ncaps {
                    let sub = m.get_property_u32(ctx, c as u32)?;
                    arr.0
                        .define_property_value_u32(ctx, idx as _, sub, prop_flags)?;
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
    let rem = str.sub_string(ctx, split_start..len)?;
    arr.0
        .define_property_value_u32(ctx, idx as u32, rem.into(), prop_flags)?;

    Ok(arr.into())
}

pub static js_regexp_proto_funcs: [FieldDef; 18] = [
    JS_CGETSET_NATIVE_DEF!("flags", js_regexp_get_flags, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("source", js_regexp_get_source, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("global", js_regexp_get_flag, NONE, LRE_FLAG_GLOBAL as _),
    JS_CGETSET_NATIVE_DEF!(
        "ignoreCase",
        js_regexp_get_flag,
        NONE,
        LRE_FLAG_IGNORECASE as _
    ),
    JS_CGETSET_NATIVE_DEF!(
        "multiline",
        js_regexp_get_flag,
        NONE,
        LRE_FLAG_MULTILINE as _
    ),
    JS_CGETSET_NATIVE_DEF!("dotAll", js_regexp_get_flag, NONE, LRE_FLAG_DOTALL as _),
    JS_CGETSET_NATIVE_DEF!("unicode", js_regexp_get_flag, NONE, LRE_FLAG_UTF16 as _),
    JS_CGETSET_NATIVE_DEF!("sticky", js_regexp_get_flag, NONE, LRE_FLAG_STICKY as _),
    JS_CGETSET_NATIVE_DEF!(
        "hasIndices",
        js_regexp_get_flag,
        NONE,
        LRE_FLAG_INDICES as _
    ),
    JS_CFUNC_NATIVE_DEF!("exec", 1, js_regexp_exec, 0),
    JS_CFUNC_NATIVE_DEF!("compile", 2, js_regexp_compile, 0),
    JS_CFUNC_NATIVE_DEF!("test", 1, js_regexp_test, 0),
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_regexp_toString, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.replace]", 2, js_regexp_Symbol_replace, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.match]", 1, js_regexp_Symbol_match, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.matchAll]", 1, js_regexp_Symbol_matchAll, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.search]", 1, js_regexp_Symbol_search, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.split]", 2, js_regexp_Symbol_split, 0),
    //JS_CGETSET_DEF("__source", js_regexp_get___source, NULL ),
    //JS_CGETSET_DEF("__flags", js_regexp_get___flags, NULL ),
];

pub static js_regexp_funcs: [FieldDef; 1] = [
    JS_CGETSET_DEF!("[Symbol.species]", js_get_this, NONE),
    //JS_CFUNC_DEF("__RegExpExec", 2, js_regexp___RegExpExec ),
    //JS_CFUNC_DEF("__RegExpDelete", 2, js_regexp___RegExpDelete ),
];

pub(super) fn JS_RegExpExec(ctx: &JSContext, this: JSObject, str: JSString) -> JSResult {
    let method = this.get_property(ctx, JS_ATOM_exec)?;
    let args: [JSValue; 1] = [str.into()];

    if method.is_callable() {
        let ret = method.call(ctx, &this.into(), &args)?;
        if !ret.is_object() && !ret.is_null() {
            ctx.throw_type_error("RegExp exec method must return an object or null");
            Err(-1)
        } else {
            Ok(ret)
        }
    } else {
        js_regexp_exec(ctx, CallContext::new(ctx, this.into(), &args, 0, None))
    }
}
