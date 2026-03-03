// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;
use lre::RegexFlags;
use smallvec::SmallVec;

use crate::{
    JS_CLASS_REGEXP,
    atom::Atom,
    builtins::array::Array,
    context::{ContextImpl, ContextPtr},
    error::JSResult,
    function::{CallContext, Callback, FieldDef, js_function_apply},
    object::{
        JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JS_SpeciesConstructor, Object,
        ObjectPayload,
    },
    string::{CharCode, CharCodeDecoder, JS_ConcatString3, StringBuf, StringData, StringRef},
    value::JSValue,
};

use super::{RegExpData, RegExpStringIterPayload, make_regexp_capture_array};

// /* create a RegExp object from a string containing the RegExp bytecode
// and the source pattern */
// fn js_regexp_constructor_internal(
//     ctx: &mut ContextImpl,
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
    let mut ctx = cc.ctx_ptr();
    let pat = *cc.arg(0);
    let mut flags1 = *cc.arg(1);

    let pat_is_regexp = is_regexp(ctx, &pat)?;

    if cc.this().is_undefined() {
        // called as a function
        if pat_is_regexp && flags1.is_undefined() {
            let ctor = pat.get_property(&mut ctx, Atom::CONST_constructor)?;
            if ctor.is_same(ctx.rt_ref().current_function()) {
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
                flags1.to_string(&mut ctx).map(|s| s.content().clone())?
            };
            None
        }
    } else {
        let mut pat_str = pat;

        if pat_is_regexp {
            pat_str = pat.get_property(&mut ctx, Atom::CONST_source)?;
            if flags1.is_undefined() {
                flags1 = pat.get_property(&mut ctx, Atom::CONST_flags)?;
            }
        }

        if !pat_str.is_undefined() {
            pattern = pat_str.to_string(&mut ctx)?.content().clone();
        }
        if !flags1.is_undefined() {
            flags_str = flags1.to_string(&mut ctx)?.content().clone();
        }

        None
    };

    let payload = ObjectPayload::REGEXP(Box::new(if let Some(d) = re {
        d.clone()
    } else {
        RegExpData::create(&mut ctx, pattern, flags_str)?
    }));
    let mut obj = ctx
        .create_object_from_ctor(*cc.this(), payload)
        .map_err(|_| -1)?;

    obj.define_property_value(
        &mut ctx,
        Atom::CONST_lastIndex,
        JSValue::Int(0),
        JS_PROP_WRITABLE,
    )?;

    Ok(obj.into())
}

fn js_get_regexp<'a>(
    mut ctx: ContextPtr,
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

pub(crate) fn is_regexp(mut ctx: ContextPtr, obj: &JSValue) -> Result<bool, i32> {
    if let JSValue::Object(o) = obj {
        let m = {
            #[cfg(feature = "builtin_symbol")]
            {
                o.get_property(&mut ctx, Atom::CONST_Symbol_match)
            }
            #[cfg(not(feature = "builtin_symbol"))]
            {
                o.get_property_str(&mut ctx, "match")
            }
        }?;

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
    let mut ctx = cc.ctx_ptr();
    let this = cc.this();

    if this.is_object() {
        if this.is_same(
            ctx.class_proto(JS_CLASS_REGEXP)
                .map_or(JSValue::NULL, JSValue::Object),
        ) {
            // empty_regex
            return ctx.create_string_from_str("(?:)").map(JSValue::from);
        }

        let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
        if re.pattern.is_empty() {
            return ctx.create_string_from_str("(?:)").map(JSValue::from);
        }

        let mut buf =
            StringBuf::with_capacity(re.pattern.len(), re.pattern.is_wide()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        // Escape '/' and newline sequences as needed
        let mut bra = false;
        let mut it = re.pattern.chars();
        while let Some((_, c)) = it.next() {
            match c {
                CharCode::Char('\\') => {
                    let _ = buf.push_u8(b'\\');
                    if let Some((_, x)) = it.next() {
                        let _ = buf.push_char(x);
                    }
                }
                CharCode::Char(']') => {
                    bra = false;
                    let _ = buf.push_u8(b']');
                }
                CharCode::Char('[') => {
                    if !bra {
                        if let Some((CharCode::Char(']'), _)) = it.current_char() {
                            // [] case
                            it.seek_relative(1); // consume ']'
                            let _ = buf.push_u8_slice(b"[]");
                            continue;
                        }
                        bra = true;
                    }
                    let _ = buf.push_u8(b'[');
                }
                CharCode::Char('\n') => {
                    let _ = buf.push_u8_slice(b"\\n");
                }
                CharCode::Char('\r') => {
                    let _ = buf.push_u8_slice(b"\\r");
                }

                CharCode::Char('/') => {
                    if !bra {
                        let _ = buf.push_u8_slice(b"\\/");
                    } else {
                        let _ = buf.push_u8(b'/');
                    }
                }
                c => {
                    let _ = buf.push_char(c);
                }
            }
        }

        ctx.create_string_with_buf(buf).map(JSValue::from)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_get_flag(cc: CallContext) -> JSResult {
    let mask = RegexFlags::from_bits_truncate(cc.magic() as u32);
    let mut ctx = cc.ctx_ptr();

    let this = *cc.this();
    if this.is_object() {
        if let Some(re) = js_get_regexp(ctx, &this, false) {
            Ok(JSValue::Bool(!(re.flags() & mask).is_empty()))
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
    let mut ctx = cc.ctx_ptr();

    if let JSValue::Object(o) = cc.this() {
        const TESTS: [(&str, u8); 8] = [
            ("hasIndices", b'd'),
            ("global", b'g'),
            ("ignoreCase", b'i'),
            ("multiline", b'm'),
            ("dotAll", b's'),
            ("unicode", b'u'),
            ("unicodeSets", b'v'),
            ("sticky", b'y'),
        ];

        let mut buf: [u8; 8] = [0; 8];
        let mut pos = 0_usize;

        for (name, ch) in TESTS.iter() {
            if o.get_property_str(&mut ctx, name)
                .and_then(|b| b.to_bool())?
            {
                buf[pos] = *ch;
                pos += 1;
            }
        }
        if pos == 0 {
            Atom::CONST_empty_string.force_string(&mut ctx)
        } else {
            ctx.create_string_from_str(std::str::from_utf8(&buf[0..pos]).unwrap())
        }
        .map(JSValue::from)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_toString(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    if let JSValue::Object(_) = cc.this() {
        let Ok(mut buf) = StringBuf::with_capacity(96, false) else {
            ctx.throw_out_of_memory();
            return Err(-1);
        };

        let _ = buf.push_u8(b'/');

        let pattern = cc
            .this()
            .get_property(&mut ctx, Atom::CONST_source)
            .and_then(|v| v.to_string(&mut ctx))?;

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
            .get_property(&mut ctx, Atom::CONST_flags)
            .and_then(|v| v.to_string(&mut ctx))?;

        buf.push_content(flags.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        buf.shrink_to_fit();
        ctx.create_string_with_buf(buf).map(JSValue::from)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_regexp_compile(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

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
            flags1.to_string(&mut ctx)?.content().clone()
        }
    } else {
        pattern = if pattern1.is_undefined() {
            StringData::Empty
        } else {
            pattern1.to_string(&mut ctx)?.content().clone()
        };
        flags_str = if flags1.is_undefined() {
            StringData::Empty
        } else {
            flags1.to_string(&mut ctx)?.content().clone()
        };
    }

    let new_re = Box::new(RegExpData::create(&mut ctx, pattern, flags_str)?);

    let mut this = *cc.this();
    if let ObjectPayload::REGEXP(re0) = this.as_object_mut().unwrap().payload_mut() {
        // replace the regexp data of payload
        unsafe {
            let ptr = re0 as *const Box<RegExpData> as *mut Box<RegExpData>;
            std::ptr::replace(ptr, new_re);
        }
    }

    this.set_property(&mut ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;

    Ok(this)
}

pub fn js_regexp_exec(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let str = cc.arg(0).to_string(&mut ctx)?;
    Ok(regexp_exec_default(ctx, *cc.this(), str.gc_ref().into())?
        .map_or(JSValue::NULL, |a| a.into()))
}

pub fn js_regexp_test(cc: CallContext) -> JSResult {
    js_regexp_exec(cc).map(|v| JSValue::Bool(!v.is_null()))
}

pub fn js_regexp_Symbol_match(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let this = *cc.this();
    let str = cc.arg(0).to_string(&mut ctx)?;

    let flags = this
        .get_property(&mut ctx, Atom::CONST_flags)?
        .to_string(&mut ctx)?;
    let is_global = flags.index_of_char('g', 0).is_some();
    if is_global {
        let is_unicode = flags
            .chars()
            .any(|(_, c)| matches!(c, CharCode::Char('u') | CharCode::Char('v')));

        this.set_property(&mut ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;

        let mut lst = Vec::<JSValue>::with_capacity(8);
        while let Some(mat) = regexp_exec(ctx, this, str.gc_ref().into())? {
            let match_str = mat.get_property_u32(&mut ctx, 0)?.to_string(&mut ctx)?;
            lst.push(JSValue::from(match_str.gc_ref()));

            if match_str.is_empty() {
                // advance to next char
                let last_index = this
                    .get_property(&mut ctx, Atom::CONST_lastIndex)
                    .and_then(|v| v.to_length(&mut ctx))?;

                this.set_property(
                    &mut ctx,
                    Atom::CONST_lastIndex,
                    JSValue::from(
                        str.index_of_next_i64(last_index, is_unicode)
                            .unwrap_or(last_index + 1),
                    ),
                )?;
            }
        }

        if !lst.is_empty() {
            lst.shrink_to_fit();
            ctx.create_array_with_values(lst).map(|x| x.into())
        } else {
            Ok(JSValue::NULL)
        }
    } else {
        regexp_exec(ctx, this, str.gc_ref().into()).map(|a| a.map_or(JSValue::NULL, |a| a.into()))
    }
}

pub fn js_regexp_Symbol_matchAll(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let this = *cc.this();
    let str = cc.arg(0).to_string(&mut ctx)?;

    // make matcher: regexp object
    let flags = this
        .get_property(&mut ctx, Atom::CONST_flags)?
        .to_string(&mut ctx)?;
    let ctor = JS_SpeciesConstructor(ctx, this, ctx.regexp_ctor)?;
    let regexp = ctor.call_ctor(&mut ctx, &[this, JSValue::from(flags.gc_ref())])?;

    let last_index = this
        .get_property(&mut ctx, Atom::CONST_lastIndex)?
        .to_length(&mut ctx)?;

    regexp.set_property(&mut ctx, Atom::CONST_lastIndex, last_index.into())?;

    // make regexp string iterator object
    ctx.create_object(ObjectPayload::REGEXP_STRING_ITERATOR(
        RegExpStringIterPayload::new(
            regexp.try_into().unwrap(),
            str.gc_ref().into(),
            flags.index_of_char('g', 0).is_some(),
            flags
                .chars()
                .any(|(_, c)| matches!(c, CharCode::Char('u') | CharCode::Char('v'))),
        ),
    ))
    .map(JSValue::Object)
    .map_err(|_| -1)
}

fn js_is_standard_regexp(ctx: &mut ContextImpl, rx: Object) -> Result<bool, i32> {
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
    let mut ctx = cc.ctx_ptr();

    let Some(this_obj) = cc.this().as_object().copied() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };
    let str = cc.arg(0).to_string(&mut ctx)?;
    let rep = *cc.arg(1);
    let rep_is_func = rep.is_function();
    let rep_str: Option<StringData> = if !rep_is_func {
        Some(rep.to_string(&mut ctx)?.content().clone())
    } else {
        None
    };

    let flags = this_obj
        .get_property(&mut ctx, Atom::CONST_flags)
        .and_then(|v| v.to_string(&mut ctx))?;
    let is_global = flags.index_of_char('g', 0).is_some();
    let is_unicode = if is_global {
        this_obj.set_property(&mut ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
        flags
            .chars()
            .any(|(_, c)| matches!(c, CharCode::Char('u') | CharCode::Char('v')))
    } else {
        false
    };

    // if let Some(rp) = rep_str.as_ref()
    //     && rp.is_empty()
    //     && is_global
    //     && js_is_standard_regexp(ctx, this)?
    // {
    //     // fast path: delete
    //     // return JS_RegExpDelete(ctx, this, rp);
    // }

    let mut sbuf = StringBuf::with_capacity(str.len(), false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    // regexp match results list
    let mut matche_list = SmallVec::<[Object; 10]>::new();
    let re_obj: JSValue = this_obj.into();

    while let Some(mat) = regexp_exec(ctx, re_obj, str.gc_ref().into())? {
        matche_list.push(mat);

        if !is_global {
            break;
        }

        if mat
            .get_property_u32(&mut ctx, 0)
            .and_then(|v| v.to_string(&mut ctx))?
            .is_empty()
        {
            // empty match: advance to next char
            let last_index = this_obj
                .get_property(&mut ctx, Atom::CONST_lastIndex)?
                .to_length(&mut ctx)?;

            let next_index = str
                .index_of_next_i64(last_index, is_unicode)
                .unwrap_or(last_index + 1);

            this_obj.set_property(&mut ctx, Atom::CONST_lastIndex, JSValue::from(next_index))?;
        }
    }

    // protect match results nodes
    let ctx0 = ctx;
    let _ng = ctx0
        .gc_heap()
        .protect_nodes_iter(matche_list.iter().map(|m| m.gc_node_ptr()));

    let mut pos = 0;

    for mat in matche_list {
        let count = mat
            .get_property(&mut ctx, Atom::CONST_length)
            .and_then(|v| v.to_u32(&mut ctx))?
            .max(1) as usize;

        let matched = mat.get_property_u32(&mut ctx, 0)?.to_string(&mut ctx)?;

        let captures = {
            let mut lst = Vec::<JSValue>::with_capacity(
                count
                    + if rep_str.is_some() {
                        0
                    } else {
                        3 // reserved extra slots for function call args
                    },
            );
            lst.push(JSValue::from(matched.gc_ref())); // [0]: matched string

            for i in 1..count {
                let c = mat.get_property_u32(&mut ctx, i as _)?;
                lst.push(if !c.is_undefined() {
                    c.to_string(&mut ctx)?.into()
                } else {
                    JSValue::UNDEFINED
                });
            }
            lst
        };

        let named_captures = mat.get_property(&mut ctx, Atom::CONST_groups)?;

        let index = (mat
            .get_property(&mut ctx, Atom::CONST_index)
            .and_then(|v| v.to_length(&mut ctx))?
            .clamp(0, str.len() as i64)) as usize;

        let repl: StringData = if let Some(rp) = rep_str.as_ref() {
            // replacement is string
            let named_dict = if !named_captures.is_undefined() {
                Some(named_captures.objectify(&mut ctx)?)
            } else {
                None
            };

            let mut ctx2 = ctx;
            let mut ctx3 = ctx;

            let t = StringData::substitution_buf(
                matched.content(),
                str.content(),
                index,
                rp,
                |n| {
                    let v = captures.get(n).unwrap();
                    if !v.is_undefined() {
                        Ok(Some(v.to_string(&mut ctx2)?.content().clone()))
                    } else {
                        Ok(None)
                    }
                },
                count,
                named_dict.is_some(),
                |name| {
                    let val = named_dict
                        .as_ref()
                        .unwrap()
                        .get_property_str(&mut ctx, name)?;
                    if !val.is_undefined() {
                        Ok(Some(val.to_string(&mut ctx3)?.content().clone()))
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
            StringData::from_buf(t)
        } else {
            // replacement is function
            js_function_apply(CallContext::new(
                cc.ctx_ptr(),
                rep,
                &[JSValue::UNDEFINED, {
                    // make arguments: captures + extras
                    let mut aa = captures;
                    aa.push(JSValue::Int(index as i32));
                    aa.push(JSValue::from(str.gc_ref()));
                    if !named_captures.is_undefined() {
                        aa.push(named_captures);
                    }
                    ctx.create_array_with_values(aa)?.into()
                }],
                0,
                None,
            ))?
            .to_string(&mut ctx)?
            .content()
            .clone()
        };

        if pos <= index {
            sbuf.push_content_range(str.content(), pos..index)
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            sbuf.push_content(&repl).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            pos = index + matched.len();
        }
    }

    // concat remaining part of `str`
    if pos < str.content().len() {
        sbuf.push_content_range(str.content(), pos..str.content().len())
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::from)
}

fn js_regexp_Symbol_search(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let Some(this_obj) = cc.this().as_object().copied() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let prev_last_index = this_obj.get_property(&mut ctx, Atom::CONST_lastIndex)?;
    if !prev_last_index.is_same(JSValue::Int(0)) {
        this_obj.set_property(&mut ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
    }

    let str = cc.arg(0).to_string(&mut ctx)?;

    let mat = regexp_exec(ctx, this_obj.into(), str.gc_ref().into())?;
    let cur_last_index = this_obj.get_property(&mut ctx, Atom::CONST_lastIndex)?;
    if !cur_last_index.is_same(prev_last_index) {
        // restore original lastIndex
        this_obj.set_property(&mut ctx, Atom::CONST_lastIndex, prev_last_index)?;
    }

    if let Some(m) = mat {
        let index = m
            .get_property(&mut ctx, Atom::CONST_index)?
            .to_index(&mut ctx)?;
        Ok(JSValue::Int(index as i32))
    } else {
        Ok(JSValue::Int(-1))
    }
}

/// [Symbol.split](str, limit)
pub fn js_regexp_Symbol_split(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let Some(this_obj) = cc.this().as_object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let str = cc.arg(0).to_string(&mut ctx)?;
    let limit = cc.arg(1);
    let limit = if limit.is_undefined() {
        usize::MAX
    } else {
        limit.to_u32(&mut ctx)? as usize
    };

    // make RegExp as splitter
    let (splitter, flag_unicode) = {
        let mut flags = this_obj
            .get_property(&mut ctx, Atom::CONST_flags)?
            .to_string(&mut ctx)?;
        let is_unicode = flags.index_of_char('u', 0).is_some();
        if flags.index_of_char('y', 0).is_none() {
            flags = JS_ConcatString3(ctx, c"", (&flags).into(), c"y")?;
        }

        let regexp_ctor = ctx.regexp_ctor;
        let ctor = JS_SpeciesConstructor(ctx, *cc.this(), regexp_ctor)?;

        let re = ctor.call_ctor(&mut ctx, &[*cc.this(), (&flags).into()])?;
        (Object::try_from(re).unwrap(), is_unicode)
    };

    let mut arr = ctx.create_array()?;
    if limit == 0 {
        return Ok(arr.into());
    }

    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
    let mut idx = 0_usize;

    if str.is_empty() {
        if regexp_exec(ctx, splitter.into(), str.gc_ref().into())?.is_none() {
            // not match: add tail, must be an empty str
            let emp_str_val =
                JSValue::from(&Atom::CONST_empty_string.force_string(&mut ctx).unwrap());
            arr.define_property_value_u32(&mut ctx, idx as u32, emp_str_val, prop_flags)?;
        }
        return Ok(arr.into());
    }

    let len = str.len();
    let mut split_start = 0;
    let mut last_index = 0;

    while last_index < len {
        // match regexp from `pos`
        splitter.set_property(
            &mut ctx,
            Atom::CONST_lastIndex,
            JSValue::Int(last_index as i32),
        )?;

        if let Some(m) = regexp_exec(ctx, splitter.into(), str.gc_ref().into())? {
            let mut next_index = splitter
                .get_property(&mut ctx, Atom::CONST_lastIndex)?
                .to_length(&mut ctx)? as usize;

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
                let sub = str.sub_string(&mut ctx, split_start..last_index)?;
                arr.define_property_value_u32(
                    &mut ctx,
                    idx as u32,
                    JSValue::from(&sub),
                    prop_flags,
                )?;
                idx += 1;
                if idx == limit {
                    return Ok(arr.into());
                }

                // append captures 1.. to array
                let ncaps = m
                    .get_property(&mut ctx, Atom::CONST_length)?
                    .to_length(&mut ctx)? as usize;
                for c in 1..ncaps {
                    let sub = m.get_property_u32(&mut ctx, c as u32)?;
                    arr.define_property_value_u32(&mut ctx, idx as _, sub, prop_flags)?;
                    idx += 1;
                    if idx == limit {
                        return Ok(arr.into());
                    }
                }

                split_start = next_index;
                last_index = next_index;
            }
        } else {
            // no match: advance index
            last_index = str
                .content()
                .index_of_next(last_index, flag_unicode)
                .unwrap_or(len);
        }
    }

    // remaining part of str
    split_start = std::cmp::min(split_start, len);
    let rem = str.sub_string(&mut ctx, split_start..len)?;
    arr.define_property_value_u32(&mut ctx, idx as u32, rem.into(), prop_flags)?;

    Ok(arr.into())
}

pub const js_regexp_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("flags", Some(js_regexp_get_flags), None, 0),
    FieldDef::GETSET("source", Some(js_regexp_get_source), None, 0),
    FieldDef::GETSET(
        "global",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::GLOBAL.to_u32() as _,
    ),
    FieldDef::GETSET(
        "ignoreCase",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::IGNORE_CASE.to_u32() as _,
    ),
    FieldDef::GETSET(
        "multiline",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::MULTI_LINE.to_u32() as _,
    ),
    FieldDef::GETSET(
        "dotAll",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::DOTALL.to_u32() as _,
    ),
    FieldDef::GETSET(
        "unicode",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::UNICODE.to_u32() as _,
    ),
    FieldDef::GETSET(
        "unicodeSets",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::UNICODE_SETS.to_u32() as _,
    ),
    FieldDef::GETSET(
        "sticky",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::STICKY.to_u32() as _,
    ),
    FieldDef::GETSET(
        "hasIndices",
        Some(js_regexp_get_flag),
        None,
        RegexFlags::INDICES.to_u32() as _,
    ),
    FieldDef::CALL("exec", 1, js_regexp_exec, 0),
    FieldDef::CALL("compile", 2, js_regexp_compile, 0),
    FieldDef::CALL("test", 1, js_regexp_test, 0),
    FieldDef::CALL("toString", 0, js_regexp_toString, 0),
    FieldDef::CALL(
        #[cfg(feature = "builtin_symbol")]
        "[Symbol.replace]",
        #[cfg(not(feature = "builtin_symbol"))]
        "replace",
        2,
        js_regexp_Symbol_replace,
        0,
    ),
    FieldDef::CALL(
        #[cfg(feature = "builtin_symbol")]
        "[Symbol.match]",
        #[cfg(not(feature = "builtin_symbol"))]
        "match",
        1,
        js_regexp_Symbol_match,
        0,
    ),
    FieldDef::CALL(
        #[cfg(feature = "builtin_symbol")]
        "[Symbol.matchAll]",
        #[cfg(not(feature = "builtin_symbol"))]
        "matchAll",
        1,
        js_regexp_Symbol_matchAll,
        0,
    ),
    FieldDef::CALL(
        #[cfg(feature = "builtin_symbol")]
        "[Symbol.search]",
        #[cfg(not(feature = "builtin_symbol"))]
        "search",
        1,
        js_regexp_Symbol_search,
        0,
    ),
    FieldDef::CALL(
        #[cfg(feature = "builtin_symbol")]
        "[Symbol.split]",
        #[cfg(not(feature = "builtin_symbol"))]
        "split",
        2,
        js_regexp_Symbol_split,
        0,
    ),
];

pub const js_regexp_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET(
        "[Symbol.species]",
        Some(crate::builtins::js_get_this),
        None,
        0,
    ),
];

fn regexp_exec_default(
    mut ctx: ContextPtr,
    this: JSValue,
    str: StringRef,
) -> Result<Option<Array>, i32> {
    let re = js_get_regexp(ctx, &this, true).ok_or(-1)?;
    let re_flags = re.flags();

    let mut from = this
        .get_property(&mut ctx, Atom::CONST_lastIndex)?
        .to_length(&mut ctx)? as usize;

    if !re_flags.has_global() && !re_flags.has_sticky() {
        from = 0;
    }

    if let Some(cap_list) = re.capture(str.content(), from) {
        let (arr, last_index) = make_regexp_capture_array(
            &mut ctx,
            str.gc_ref().into(),
            &cap_list,
            re_flags.has_indices(),
        )?;

        if re_flags.has_global() || re_flags.has_sticky() {
            this.set_property(
                &mut ctx,
                Atom::CONST_lastIndex,
                JSValue::from(last_index as u32),
            )?;
        }
        Ok(Some(arr))
    } else {
        if from >= str.content().len() || re_flags.has_sticky() || re_flags.has_global() {
            this.set_property(&mut ctx, Atom::CONST_lastIndex, JSValue::Int(0))?;
        }
        Ok(None)
    }
}

/// Invoke regexp.exec() method or call standard `regexp_exec_internal`.
/// returns match result object or None.
pub(super) fn regexp_exec(
    mut ctx: ContextPtr,
    regexp: JSValue,
    str: StringRef,
) -> Result<Option<Object>, i32> {
    if let Some(o) = regexp.as_object() {
        let exec = o.get_property(&mut ctx, Atom::CONST_exec)?;
        if !exec.match_native_callback(Callback::Func(js_regexp_exec), 0) {
            // call method function
            let s: JSValue = str.into();
            let ret = exec.call(&mut ctx, regexp, std::slice::from_ref(&s))?;
            return match ret {
                JSValue::Object(o) => Ok(Some(o)),
                JSValue::NULL => Ok(None),
                _ => {
                    ctx.throw_type_error("RegExp exec method must return an object or null");
                    Err(-1)
                }
            };
        }
    }

    regexp_exec_default(ctx, regexp, str.gc_ref().into()).map(|a| a.map(|a| a.into()))
}
