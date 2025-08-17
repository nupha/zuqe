use std::{ops::Deref, rc::Rc};

use regex::bytes::Regex;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_REGEXP,
    atom::consts::*,
    context::{JS_GetActiveFunction, JSContext},
    error::{ErrorKind, JSResult},
    function::{CallContext, JS_CallConstructor, NativeField, NativeFunctionType},
    js_core::js_get_this,
    object::{
        JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JS_SpeciesConstructor, JSObject,
        ObjectDetail,
    },
    runtime::JSRuntime,
    string::{JSString, StringBuf, StringChar, StringContent},
    value::JSValue,
};

use super::{
    JSRegExp, JSRegExpStringIteratorData, LRE_FLAG_DOTALL, LRE_FLAG_GLOBAL, LRE_FLAG_IGNORECASE,
    LRE_FLAG_INDICES, LRE_FLAG_MULTILINE, LRE_FLAG_STICKY, LRE_FLAG_UTF16, RegExpExecMode,
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

    let mut re = if let Some(re) = pat.object().and_then(|o| js_get_regexp(ctx, o, false)) {
        re
    } else {
        if pat_is_regexp {
            pat = pat.get_property(ctx, JS_ATOM_source)?;
            if flags1.is_undefined() {
                flags1 = pat.get_property(ctx, JS_ATOM_flags)?;
            }
        }

        let pattern = if pat.is_undefined() {
            Rc::new(StringBuf::Empty)
        } else {
            pat.to_string(ctx)?.content().clone()
        };

        let (re, flags) = JSRegExp::new_re(ctx, &pattern.to_utf8(), "".chars()).map_err(|e| {
            ctx.throw_error(ErrorKind::Syntax, &e.to_string());
            -1
        })?;
        JSRegExp {
            regex: Rc::new(re),
            pattern,
            flags_str: Rc::new(StringBuf::Empty),
            flags,
        }
    };

    if !flags1.is_undefined() {
        // update flags
        re.flags_str = flags1.to_string(ctx)?.content().clone();
        for c in re.flags_str.chars_lossy() {
            match c {
                'g' => {
                    re.flags |= LRE_FLAG_GLOBAL as u8;
                }
                'i' => {
                    re.flags |= LRE_FLAG_IGNORECASE as u8;
                }
                'm' => {
                    re.flags |= LRE_FLAG_MULTILINE as u8;
                }
                'u' | 'v' => {
                    re.flags |= LRE_FLAG_UTF16 as u8;
                }
                's' => {
                    re.flags |= LRE_FLAG_DOTALL as u8;
                }
                'y' => {
                    re.flags |= LRE_FLAG_STICKY as u8;
                }
                _ => {}
            }
        }
    }

    let obj = ctx
        .create_object_apply_ctor(cc.this(), ObjectDetail::REGEXP(Box::new(re)))
        .map_err(|_| -1)?;
    obj.define_property_value(ctx, JS_ATOM_lastIndex, 0, JS_PROP_WRITABLE)?;
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
            m.coerce_to_bool(ctx)
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
            if o.get_property_str(ctx, *name)
                .and_then(|b| b.coerce_to_bool(ctx))?
            {
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
    let re = js_get_regexp(ctx, cc.this().object().unwrap(), true).ok_or(-1)?;

    let pattern1 = cc.arg(0);
    let flags1 = cc.arg(1);

    let pattern: StringContent;
    let regex: Rc<Regex>;

    if let Some(re1) = pattern1.object().and_then(|o| js_get_regexp(ctx, o, false)) {
        if flags1.is_undefined() {
            ctx.throw_type_error("flags must be undefined");
            return Err(-1);
        }
        pattern = re1.pattern.clone();
        regex = re1.regex.clone();
    } else {
        pattern = if pattern1.is_undefined() {
            Rc::new(StringBuf::Empty)
        } else {
            pattern1.to_string(ctx)?.content().clone()
        };

        let (re, _) = JSRegExp::new_re(ctx, &pattern.to_utf8(), "".chars()).map_err(|e| {
            ctx.throw_error(ErrorKind::Syntax, &e.to_string());
            -1
        })?;
        regex = Rc::new(re);
    }

    cc.this()
        .set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;

    // TODO: mutable.
    // re.pattern = pattern;
    // re.regex = regex;

    Ok(cc.this().clone())
}

pub fn js_regexp_exec(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    let str = cc.arg(0).to_string(ctx)?;

    if let Some(cap_list) = re.capture(
        &str,
        if re.flags & LRE_FLAG_GLOBAL as u8 == 0 {
            0
        } else {
            let val = cc.this().get_property(ctx, JS_ATOM_lastIndex)?;
            val.coerce_to_length(ctx)? as usize
        },
    ) {
        let arr = ctx.create_array()?;
        let mut group = JSValue::UNDEFINED;

        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
        let mut last_index = 0;

        for (i, (range, name)) in cap_list.iter().enumerate() {
            let val: JSValue = str.sub_string(ctx, range.clone())?.into();

            println!("[O.o]**************** {i}: range={range:?}, val={val:?}");

            arr.define_property_value_u32(ctx, i as _, val.clone(), prop_flags)?;

            if let Some(name) = name {
                if group.is_undefined() {
                    group = ctx.create_base_object()?.into();
                }
                group.define_property_value_str(ctx, name, val, prop_flags)?;
            }

            if i == 0 {
                arr.define_property_value(
                    ctx,
                    JS_ATOM_index,
                    JSValue::Int(range.start as i32),
                    prop_flags,
                )?;
                last_index = range.end;
            }
        }

        arr.define_property_value(ctx, JS_ATOM_groups, group, prop_flags)?;
        arr.define_property_value(ctx, JS_ATOM_input, str.into(), prop_flags)?;
        this.set_property(ctx, JS_ATOM_lastIndex, JSValue::from(last_index as u32))?;

        Ok(arr)
    } else {
        this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
        Ok(JSValue::NULL)
    }

    // let (ret, last_index) = re
    //     .exec(
    //         ctx,
    //         &str,
    //         if re.flags & LRE_FLAG_GLOBAL as u8 == 0 {
    //             0
    //         } else {
    //             let val = cc.this().get_property(ctx, JS_ATOM_lastIndex)?;
    //             val.coerce_to_length(ctx)? as usize
    //         },
    //         RegExpExecMode::CaptureOnce,
    //     )?
    //     .unwrap_or_else(|| (JSValue::NULL, 0));

    // this.set_property(ctx, JS_ATOM_lastIndex, JSValue::from(last_index as u32))?;

    // Ok(ret)
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
        let found = re.find(&str, 0);
        if !found.is_empty() {
            let arr = ctx.create_array()?;
            let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
            let mut last_index = 0;

            for (i, range) in found.iter().enumerate() {
                let val = str.sub_string(ctx, range.clone())?;

                println!(
                    "[O.o]****** {i} range={range:?}, val={val:?}, chars={:?}",
                    str.content().iter_range(range.clone()).collect::<Vec<_>>()
                );

                arr.define_property_value_u32(ctx, i as _, val.into(), prop_flags)?;
                last_index = range.end;
            }

            this.set_property(ctx, JS_ATOM_lastIndex, JSValue::from(last_index as u32))?;
            Ok(arr.into())
        } else {
            this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
            Ok(JSValue::NULL)
        }

        // let (ret, last_index) = re
        //     .exec(ctx, &str, 0, RegExpExecMode::Match)?
        //     .unwrap_or_else(|| (JSValue::NULL, 0));
        // this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(last_index as _))?;
        // Ok(ret)
    } else {
        // slow path: use customized exec method, and collect matched string to array
        let arr = ctx.create_array()?;
        let args = [str.clone().into()];

        let mut cnt = 0;
        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

        loop {
            match exec.call(ctx, &cc.this(), &args)? {
                JSValue::Object(o) => {
                    let last_index = this
                        .get_property(ctx, JS_ATOM_lastIndex)?
                        .coerce_to_u32(ctx)? as usize;

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

                    arr.define_property_value_u32(ctx, cnt, matched.into(), prop_flags)?;
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

        Ok(arr)
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
    .error()?;
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

    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    let str = cc.arg(0).to_string(ctx)?;

    let rep = cc.arg(1);
    let functionalReplace = rep.is_callable();
    let rp: Option<String> = if !functionalReplace {
        Some(rep.to_string(ctx)?.content().to_utf8().into_owned())
    } else {
        None
    };

    let flags = this
        .get_property(ctx, JS_ATOM_flags)
        .and_then(|v| v.to_string(ctx))?;

    let is_global = re.flags & LRE_FLAG_GLOBAL as u8 != 0;
    let fullUnicode = if is_global {
        this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
        flags.content().index_of_char('u', 0).is_some()
    } else {
        false
    };

    if rp.as_ref().map_or(false, |s| s.is_empty()) && is_global && js_is_standard_regexp(ctx, this)?
    {
        // fast path: delete only
        // return JS_RegExpDelete(ctx, this, sp);
        todo!();
    }

    let utf8_text = str.content().to_utf8();
    let mut chars_iter = if str.is_wide() {
        Some(str.content().iter())
    } else {
        None
    };
    let mut sbuf =
        StringBuf::with_capacity(str.buf_len() * 3 / 2, str.is_wide()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

    let mut str_last_index = 0;
    let mut utf8_cursor = 0;

    for caps in re
        .regex
        .captures_iter(utf8_text.as_bytes())
        .take(if !is_global { 1 } else { usize::MAX })
    {
        if let Some(m) = caps.get(0) {
            // convert utf8 index to string buf index
            let pos = if m.start() == 0 {
                0
            } else if let Some(it) = chars_iter.as_mut() {
                let nc = utf8_text[utf8_cursor..m.start()].chars().count();
                let (post, _) = it.nth(nc - 1).unwrap(); // use nc-1
                post
            } else {
                m.start()
            };
            sbuf.push_content_slice(&str.content().slice(str_last_index..pos))
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

            utf8_cursor = m.end();
            str_last_index = if utf8_cursor == utf8_text.len() {
                str.content().buf_len()
            } else if let Some(it) = chars_iter.as_mut() {
                let nc = std::str::from_utf8(m.as_bytes()).unwrap().chars().count();
                it.nth(nc - 1).unwrap().0
            } else {
                utf8_cursor
            };

            if let Some(rep_str) = rp.as_ref() {
                let t = StringBuf::substitution(
                    std::str::from_utf8(m.as_bytes()).unwrap(),
                    str.content(),
                    pos,
                    rep_str.chars().map(|c| c.into()),
                    |n| {
                        caps.get(n)
                            .map(|m| std::str::from_utf8(m.as_bytes()).unwrap().into())
                    },
                    |name| {
                        caps.name(name)
                            .map(|m| std::str::from_utf8(m.as_bytes()).unwrap().into())
                    },
                )
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

                if sbuf.is_empty() {
                    sbuf = t;
                } else {
                    sbuf.push(&t).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
            } else {
                // call func
                let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

                let obj = ctx.create_array()?;
                for (i, cap) in caps.iter().enumerate() {
                    let val = if let Some(m) = cap {
                        ctx.create_string_from_str(std::str::from_utf8(m.as_bytes()).unwrap())?
                            .into()
                    } else {
                        JSValue::UNDEFINED
                    };
                    obj.define_property_value_u32(ctx, i as _, val, prop_flags)?;
                }
                for (name, matched) in re.regex.capture_names().filter_map(|n| {
                    n.and_then(|n| {
                        caps.name(n)
                            .map(|m| ((n, std::str::from_utf8(m.as_bytes()).unwrap())))
                    })
                }) {
                    let val = ctx.create_string_from_str(matched)?;
                    obj.define_property_value_str(ctx, name, val.into(), prop_flags)?;
                }

                let r = rep.call(ctx, &rep, &[obj])?.to_string(ctx)?;

                sbuf.push(&r.content()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
        }
    }

    // str remainder
    if str_last_index < str.content().buf_len() {
        sbuf.push_content_slice(&str.content().slice(str_last_index..str.content().buf_len()))
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
    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;

    let str = cc.arg(0).to_string(ctx)?;
    let utf8_text = str.content().to_utf8();

    if let Some(m) = re.regex.find(utf8_text.as_bytes()) {
        let pos = if m.start() == 0 {
            0
        } else if str.content().is_wide() {
            let nc = utf8_text[..m.start()].chars().count();
            let (i, _) = str.content().iter().nth(nc - 1).unwrap();
            i
        } else {
            m.start()
        };
        Ok(JSValue::from(pos as u32))
    } else {
        Ok(JSValue::Int(-1))
    }
}

pub fn js_regexp_Symbol_split(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };
    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;

    let str = cc.arg(0).to_string(ctx)?;
    let limit = cc.arg(1);
    let limit = if limit.is_undefined() {
        usize::MAX
    } else {
        limit.coerce_to_u32(ctx)? as usize
    };

    let arr = ctx.create_array()?;
    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

    // FIXME: consider sticky flag
    for (i, s) in re
        .regex
        .splitn(str.content().to_utf8().as_bytes(), limit)
        .enumerate()
    {
        let val = ctx.create_string_from_str(std::str::from_utf8(s).unwrap())?;
        arr.define_property_value_u32(ctx, i as _, val.into(), prop_flags)?;
    }

    Ok(arr)
}

pub const js_regexp_proto_funcs: [NativeField; 18] = [
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

pub const js_regexp_funcs: [NativeField; 1] = [
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
