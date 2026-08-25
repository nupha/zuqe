// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use lre::RegexFlags;
use smallvec::SmallVec;

use crate::{
    JS_CLASS_REGEXP,
    array::Array,
    atom::Atom,
    builtins::common::SPECIES_FIELD,
    context::ScopedContext,
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

/// `ToLength(Get(obj, "lastIndex"))`: the shared prologue of the RegExp
/// methods that resume matching from the current position.
pub(crate) fn get_last_index(ctx: &mut ScopedContext, obj: JSValue) -> Result<i64, i32> {
    obj.get_property(ctx, Atom::CONST_lastIndex)?.to_length(ctx)
}

pub fn js_regexp_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let pat = cc.arg(0);
    let mut flags1 = cc.arg(1);

    let pat_is_regexp = is_regexp(ctx, pat)?;

    if cc.this().is_undefined() {
        // called as a function
        if pat_is_regexp && flags1.is_undefined() {
            let ctor = pat.get_property(ctx, Atom::CONST_constructor)?;
            if ctor.is_same(ctx.rt().current_function()) {
                return Ok(pat);
            }
        }
    }

    let mut pattern = StringData::Empty;
    let mut flags_str = StringData::Empty;

    let re = if let Some(re) = js_get_regexp(&pat) {
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
        let mut pat_str = pat;

        if pat_is_regexp {
            pat_str = pat.get_property(ctx, Atom::CONST_source)?;
            if flags1.is_undefined() {
                flags1 = pat.get_property(ctx, Atom::CONST_flags)?;
            }
        }

        if !pat_str.is_undefined() {
            pattern = pat_str.to_string(ctx)?.content().clone();
        }
        if !flags1.is_undefined() {
            flags_str = flags1.to_string(ctx)?.content().clone();
        }

        None
    };

    let payload = ObjectPayload::REGEXP(Box::new(if let Some(d) = re {
        d.clone()
    } else {
        RegExpData::create(ctx, pattern, flags_str)?
    }));
    let mut obj = ctx
        .ordinary_create_from_constructor(cc.this(), payload)
        .map_err(|_| -1)?;

    obj.define_property_value(
        ctx,
        Atom::CONST_lastIndex,
        JSValue::from(0),
        JS_PROP_WRITABLE,
    )?;

    Ok(obj.into())
}

/// Get the RegExpData payload if `obj` is a RegExp instance (no error thrown).
fn js_get_regexp<'a>(obj: &'a JSValue) -> Option<&'a RegExpData> {
    obj.try_class_payload(JS_CLASS_REGEXP)
}

pub(crate) fn is_regexp(ctx: &mut ScopedContext, obj: JSValue) -> Result<bool, i32> {
    if let Some(o) = obj.as_object() {
        let m = o.get_property(ctx, Atom::CONST_Symbol_match)?;
        if !m.is_undefined() {
            m.to_bool()
        } else {
            Ok(js_get_regexp(&obj).is_some())
        }
    } else {
        Ok(false)
    }
}

pub fn js_regexp_get_source(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();

    if this.is_object() {
        if this.is_same(
            ctx.class_proto(JS_CLASS_REGEXP)
                .map_or(JSValue::NULL, JSValue::from),
        ) {
            // empty_regex
            return ctx.create_string_from_str("(?:)").map(JSValue::make_string);
        }

        let re: &RegExpData = this.class_payload(ctx, JS_CLASS_REGEXP)?;
        if re.pattern.is_empty() {
            return ctx.create_string_from_str("(?:)").map(JSValue::make_string);
        }

        let mut buf = StringBuf::with_capacity(re.pattern.len(), re.pattern.is_wide())
            .map_err(|_| ctx.throw_out_of_memory())?;

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

        ctx.create_string_with_buf(buf).map(JSValue::make_string)
    } else {
        Err(ctx.throw_type_error_not_an_object())
    }
}

pub fn js_regexp_get_flag(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mask = RegexFlags::from_bits_truncate(cc.magic() as u32);

    let this = cc.this();
    if this.is_object() {
        if let Some(re) = js_get_regexp(&this) {
            Ok(JSValue::from(!(re.flags() & mask).is_empty()))
        } else if this.is_same(
            ctx.class_proto(JS_CLASS_REGEXP)
                .map_or(JSValue::NULL, JSValue::from),
        ) {
            Ok(JSValue::UNDEFINED)
        } else {
            Err(ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP))
        }
    } else {
        Err(ctx.throw_type_error_not_an_object())
    }
}

pub fn js_regexp_get_flags(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(o) = cc.this().as_object() {
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
            if o.get_property_str(ctx, name).and_then(|b| b.to_bool())? {
                buf[pos] = *ch;
                pos += 1;
            }
        }
        if pos == 0 {
            Atom::CONST_empty_string.force_string(ctx)
        } else {
            ctx.create_string_from_str(std::str::from_utf8(&buf[0..pos]).unwrap())
        }
        .map(JSValue::make_string)
    } else {
        Err(ctx.throw_type_error_not_an_object())
    }
}

pub fn js_regexp_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if cc.this().is_object() {
        let Ok(mut buf) = StringBuf::with_capacity(96, false) else {
            return Err(ctx.throw_out_of_memory());
        };

        let _ = buf.push_u8(b'/');

        let pattern = cc
            .this()
            .get_property(ctx, Atom::CONST_source)
            .and_then(|v| v.to_string(ctx))?;

        buf.push_content(pattern.content())
            .map_err(|_| ctx.throw_out_of_memory())?;

        buf.push_u8(b'/').map_err(|_| ctx.throw_out_of_memory())?;

        let flags = cc
            .this()
            .get_property(ctx, Atom::CONST_flags)
            .and_then(|v| v.to_string(ctx))?;

        buf.push_content(flags.content())
            .map_err(|_| ctx.throw_out_of_memory())?;

        buf.shrink_to_fit();
        ctx.create_string_with_buf(buf).map(JSValue::make_string)
    } else {
        Err(ctx.throw_type_error_not_an_object())
    }
}

fn js_regexp_compile(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let _: &RegExpData = this.class_payload(ctx, JS_CLASS_REGEXP)?;

    let pattern1 = cc.arg(0);
    let flags1 = cc.arg(1);

    let pattern: StringData;
    let flags_str: StringData;

    if let Some(re1) = js_get_regexp(&pattern1) {
        if flags1.is_undefined() {
            return Err(ctx.throw_type_error("flags must be undefined"));
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

    let mut this = cc.this();
    if let ObjectPayload::REGEXP(re0) = this.as_object().unwrap().payload_mut() {
        // replace the regexp data of payload
        unsafe {
            let ptr = re0 as *const Box<RegExpData> as *mut Box<RegExpData>;
            std::ptr::replace(ptr, new_re);
        }
    }

    this.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(0))?;
    Ok(this)
}

pub fn js_regexp_exec(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let str = cc.arg(0).to_string(ctx)?;
    Ok(regexp_exec_default(ctx, cc.this(), str.into())?.map_or(JSValue::NULL, |a| a.into()))
}

pub fn js_regexp_test(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    js_regexp_exec(ctx, cc).map(|v| JSValue::from(!v.is_null()))
}

pub fn js_regexp_Symbol_match(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let str = cc.arg(0).to_string(ctx)?;

    let flags = this.get_property(ctx, Atom::CONST_flags)?.to_string(ctx)?;
    let is_global = flags.index_of_char('g', 0).is_some();
    if is_global {
        let is_unicode = flags
            .chars()
            .any(|(_, c)| matches!(c, CharCode::Char('u') | CharCode::Char('v')));

        this.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(0))?;

        let mut lst = Vec::<JSValue>::with_capacity(8);
        while let Some(mat) = regexp_exec(ctx, this, str.into())? {
            let match_str = mat.get_property_u32(ctx, 0)?.to_string(ctx)?;
            lst.push(JSValue::make_string(match_str));

            if match_str.is_empty() {
                // advance to next char
                let last_index = get_last_index(ctx, this)?;

                this.set_property(
                    ctx,
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
            Array::from_values(ctx, lst).map(|x| x.into())
        } else {
            Ok(JSValue::NULL)
        }
    } else {
        regexp_exec(ctx, this, str.into()).map(|a| a.map_or(JSValue::NULL, |a| a.into()))
    }
}

pub fn js_regexp_Symbol_matchAll(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let str = cc.arg(0).to_string(ctx)?;

    // make matcher: regexp object
    let flags = this.get_property(ctx, Atom::CONST_flags)?.to_string(ctx)?;
    let ctor = JS_SpeciesConstructor(ctx, this, ctx.regexp_ctor().into())?;
    let regexp = ctor.call_ctor(ctx, &[this, JSValue::make_string(flags)])?;

    let last_index = get_last_index(ctx, this)?;

    regexp.set_property(ctx, Atom::CONST_lastIndex, last_index.into())?;

    // make regexp string iterator object
    ctx.create_instance_with_payload(ObjectPayload::REGEXP_STRING_ITERATOR(
        RegExpStringIterPayload::new(
            regexp.try_into().unwrap(),
            str.into(),
            flags.index_of_char('g', 0).is_some(),
            flags
                .chars()
                .any(|(_, c)| matches!(c, CharCode::Char('u') | CharCode::Char('v'))),
        ),
    ))
    .map(JSValue::from)
    .map_err(|_| -1)
}

// fn js_is_standard_regexp(ctx: &mut ContextImpl, rx: Object) -> Result<bool, i32> {
//     let val = rx.get_property(ctx, Atom::CONST_constructor)?;
//     if val.is_same(ctx.regexp_ctor) {
//         let f = rx.get_property(ctx, Atom::CONST_exec)?;
//         Ok(f.match_native_callback(Callback::Func(js_regexp_exec), 0))
//     } else {
//         Ok(false)
//     }
// }

pub fn js_regexp_Symbol_replace(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // [Symbol.replace](str, rep)

    let Some(this_obj) = cc.this().as_object() else {
        return Err(ctx.throw_type_error_not_an_object());
    };
    let str = cc.arg(0).to_string(ctx)?;
    let rep = cc.arg(1);
    let rep_is_func = rep.is_callable();
    let rep_str: Option<StringData> = if !rep_is_func {
        Some(rep.to_string(ctx)?.content().clone())
    } else {
        None
    };

    let flags = this_obj
        .get_property(ctx, Atom::CONST_flags)
        .and_then(|v| v.to_string(ctx))?;
    let is_global = flags.index_of_char('g', 0).is_some();
    let is_unicode = if is_global {
        this_obj.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(0))?;
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

    let mut sbuf =
        StringBuf::with_capacity(str.len(), false).map_err(|_| ctx.throw_out_of_memory())?;

    // regexp match results list
    let mut matche_list = SmallVec::<[Object; 10]>::new();
    let re_obj: JSValue = this_obj.into();

    while let Some(mat) = regexp_exec(ctx, re_obj, str.into())? {
        matche_list.push(mat);

        if !is_global {
            break;
        }

        if mat
            .get_property_u32(ctx, 0)
            .and_then(|v| v.to_string(ctx))?
            .is_empty()
        {
            // empty match: advance to next char
            let last_index = get_last_index(ctx, this_obj.into())?;

            let next_index = str
                .index_of_next_i64(last_index, is_unicode)
                .unwrap_or(last_index + 1);

            this_obj.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(next_index))?;
        }
    }

    // protect match results
    let scope_stack_id = ctx.scope_stack_id();
    ctx.gc_heap_mut()
        .protect_nodes_iter(scope_stack_id, matche_list.iter().map(|m| m.gc_node_ptr()));

    let mut pos = 0;

    for mat in matche_list {
        let count = mat
            .get_property(ctx, Atom::CONST_length)
            .and_then(|v| v.to_number(ctx))
            .map(|n| n.to_u32())?
            .max(1) as usize;

        let matched = mat.get_property_u32(ctx, 0)?.to_string(ctx)?;

        let captures = {
            let mut lst = Vec::<JSValue>::with_capacity(
                count
                    + if rep_str.is_some() {
                        0
                    } else {
                        3 // reserved extra slots for function call args
                    },
            );
            lst.push(JSValue::make_string(matched)); // [0]: matched string

            for i in 1..count {
                let c = mat.get_property_u32(ctx, i as _)?;
                lst.push(if !c.is_undefined() {
                    JSValue::make_string(c.to_string(ctx)?)
                } else {
                    JSValue::UNDEFINED
                });
            }
            lst
        };

        let named_captures = mat.get_property(ctx, Atom::CONST_groups)?;

        let index = (mat
            .get_property(ctx, Atom::CONST_index)
            .and_then(|v| v.to_length(ctx))?
            .clamp(0, str.len() as i64)) as usize;

        let repl: StringData = if let Some(rp) = rep_str.as_ref() {
            // replacement is string
            let named_dict = if !named_captures.is_undefined() {
                Some(named_captures.objectify(ctx)?)
            } else {
                None
            };

            let mut ctx_p = ctx.ctx_ptr;
            let mut ctx_p2 = ctx_p;

            let t = StringData::substitution_buf(
                matched.content(),
                str.content(),
                index,
                rp,
                |n| {
                    let v = captures.get(n).unwrap();
                    if !v.is_undefined() {
                        ctx_p.with_current_scope(|mut s| {
                            Ok(Some(v.to_string(&mut s)?.content().clone()))
                        })
                    } else {
                        Ok(None)
                    }
                },
                count,
                named_dict.is_some(),
                |name| {
                    let val = named_dict.as_ref().unwrap().get_property_str(ctx, name)?;
                    if !val.is_undefined() {
                        ctx_p2.with_current_scope(|mut s| {
                            Ok(Some(val.to_string(&mut s)?.content().clone()))
                        })
                    } else {
                        Ok(None)
                    }
                },
            )
            .map_err(|e| {
                if e.is_some() {
                    ctx.throw_out_of_memory()
                } else {
                    -1
                }
            })?;

            StringData::from_buf(t)
        } else {
            // replacement is function
            let args = [JSValue::UNDEFINED, {
                // make arguments: captures + extras
                let mut aa = captures;
                aa.push(JSValue::from(index as i32));
                aa.push(JSValue::make_string(str));
                if !named_captures.is_undefined() {
                    aa.push(named_captures);
                }
                Array::from_values(ctx, aa)?.into()
            }];
            js_function_apply(ctx, CallContext::new_simple(rep, &args))?
                .to_string(ctx)?
                .content()
                .clone()
        };

        if pos <= index {
            sbuf.push_content_range(str.content(), pos..index)
                .map_err(|_| ctx.throw_out_of_memory())?;
            sbuf.push_content(&repl)
                .map_err(|_| ctx.throw_out_of_memory())?;

            pos = index + matched.len();
        }
    }

    // concat remaining part of `str`
    if pos < str.content().len() {
        sbuf.push_content_range(str.content(), pos..str.content().len())
            .map_err(|_| ctx.throw_out_of_memory())?;
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

fn js_regexp_Symbol_search(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(this_obj) = cc.this().as_object() else {
        return Err(ctx.throw_type_error_not_an_object());
    };

    let prev_last_index = this_obj.get_property(ctx, Atom::CONST_lastIndex)?;
    if !prev_last_index.is_same(JSValue::from(0)) {
        this_obj.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(0))?;
    }

    let str = cc.arg(0).to_string(ctx)?;

    let mat = regexp_exec(ctx, this_obj.into(), str.into())?;
    let cur_last_index = this_obj.get_property(ctx, Atom::CONST_lastIndex)?;
    if !cur_last_index.is_same(prev_last_index) {
        // restore original lastIndex
        this_obj.set_property(ctx, Atom::CONST_lastIndex, prev_last_index)?;
    }

    Ok(JSValue::from(if let Some(m) = mat {
        m.get_property(ctx, Atom::CONST_index)?.to_index(ctx)? as i32
    } else {
        -1
    }))
}

/// [Symbol.split](str, limit)
pub fn js_regexp_Symbol_split(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let Some(this_obj) = this.as_object() else {
        return Err(ctx.throw_type_error_not_an_object());
    };

    let str = cc.arg(0).to_string(ctx)?;
    let limit = cc.arg(1);
    let limit = if limit.is_undefined() {
        usize::MAX
    } else {
        limit.to_number(ctx)?.to_u32() as usize
    };

    // make RegExp as splitter
    let (splitter, flag_unicode) = {
        let mut flags = this_obj
            .get_property(ctx, Atom::CONST_flags)?
            .to_string(ctx)?;
        let is_unicode = flags.index_of_char('u', 0).is_some();
        if flags.index_of_char('y', 0).is_none() {
            flags = JS_ConcatString3(ctx, c"", JSValue::make_string(flags), c"y")?;
        }

        let regexp_ctor = ctx.regexp_ctor();
        let ctor = JS_SpeciesConstructor(ctx, this, regexp_ctor.into())?;

        let re = ctor.call_ctor(ctx, &[this, JSValue::make_string(flags)])?;
        (Object::try_from(re).unwrap(), is_unicode)
    };

    let mut arr = Array::create(ctx)?;
    if limit == 0 {
        return Ok(arr.into());
    }

    let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;
    let mut idx = 0_usize;

    if str.is_empty() {
        if regexp_exec(ctx, splitter.into(), str.into())?.is_none() {
            // not match: add tail, must be an empty str
            let emp_str_val =
                JSValue::make_string(Atom::CONST_empty_string.force_string(ctx).unwrap());
            arr.define_property_value_u32(ctx, idx as u32, emp_str_val, prop_flags)?;
        }
        return Ok(arr.into());
    }

    let len = str.len();
    let mut split_start = 0;
    let mut last_index = 0;

    while last_index < len {
        // match regexp from `pos`
        splitter.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(last_index as i32))?;

        if let Some(m) = regexp_exec(ctx, splitter.into(), str.into())? {
            let mut next_index = get_last_index(ctx, splitter.into())? as usize;

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
                arr.define_property_value_u32(
                    ctx,
                    idx as u32,
                    JSValue::make_string(sub),
                    prop_flags,
                )?;
                idx += 1;
                if idx == limit {
                    return Ok(arr.into());
                }

                // append captures 1.. to array
                let ncaps = m.get_length_prop(ctx)? as usize;
                for c in 1..ncaps {
                    let sub = m.get_property_u32(ctx, c as u32)?;
                    arr.define_property_value_u32(ctx, idx as _, sub, prop_flags)?;
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
    let rem = str.sub_string(ctx, split_start..len)?;
    arr.define_property_value_u32(ctx, idx as u32, JSValue::make_string(rem), prop_flags)?;

    Ok(arr.into())
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Push `\<prefix>` followed by `digits` lowercase hex digits of `n`.
fn push_hex_escape(
    buf: &mut StringBuf,
    prefix: u8,
    n: u32,
    digits: usize,
) -> Result<(), std::collections::TryReserveError> {
    buf.push_u8(b'\\')?;
    buf.push_u8(prefix)?;
    let mut tmp = [0u8; 8];
    let mut v = n;
    for i in (0..digits).rev() {
        tmp[i] = HEX_DIGITS[(v & 0xf) as usize];
        v >>= 4;
    }
    buf.push_u8_slice(&tmp[..digits])
}

/// `EncodeForRegExpEscape ( c )` from the RegExp.escape proposal.
fn encode_for_regexp_escape(
    buf: &mut StringBuf,
    cp: u32,
) -> Result<(), std::collections::TryReserveError> {
    // 1. SyntaxCharacter or U+002F (SOLIDUS): `\` + the code point.
    if cp == 0x2f
        || matches!(
            cp,
            0x5e | 0x24
                | 0x5c
                | 0x2e
                | 0x2a
                | 0x2b
                | 0x3f
                | 0x28
                | 0x29
                | 0x5b
                | 0x5d
                | 0x7b
                | 0x7d
                | 0x7c
        )
    {
        buf.push_u8(b'\\')?;
        buf.push_u32(cp)?;
        return Ok(());
    }

    // 2. Table 64: ControlEscape sequences.
    match cp {
        0x09 => return buf.push_u8_slice(b"\\t"),
        0x0a => return buf.push_u8_slice(b"\\n"),
        0x0b => return buf.push_u8_slice(b"\\v"),
        0x0c => return buf.push_u8_slice(b"\\f"),
        0x0d => return buf.push_u8_slice(b"\\r"),
        _ => {}
    }

    // 3-5. other punctuators, WhiteSpace/LineTerminator, or lone surrogates.
    let other_punctuator = matches!(
        cp,
        0x2c | 0x2d
            | 0x3d
            | 0x3c
            | 0x3e
            | 0x23
            | 0x26
            | 0x21
            | 0x25
            | 0x3a
            | 0x3b
            | 0x40
            | 0x7e
            | 0x27
            | 0x60
            | 0x22
    );
    if other_punctuator || is_whitespace_or_lineterminator(cp) || (0xd800..=0xdfff).contains(&cp) {
        if cp <= 0xff {
            // `\xHH`
            push_hex_escape(buf, b'x', cp, 2)?;
        } else {
            // `\uHHHH` for each UTF-16 code unit.
            let mut u16 = [0u16; 2];
            for cu in CharCode::from(cp).encode_utf16(&mut u16) {
                push_hex_escape(buf, b'u', *cu as u32, 4)?;
            }
        }
        return Ok(());
    }

    // 6. UTF16EncodeCodePoint(c): leave the code point unchanged.
    buf.push_u32(cp)
}

/// ECMAScript WhiteSpace or LineTerminator code points (the ones not already
/// handled by the ControlEscape table: TAB/VT/FF, LF/CR).
fn is_whitespace_or_lineterminator(cp: u32) -> bool {
    matches!(
        cp,
        0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

/// `RegExp.escape ( string )` (tc39/proposal-regexp-escape).
pub fn js_regexp_escape(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // spec step 1: if the argument is not a String, throw a TypeError.
    let Some(s) = cc.arg(0).as_string(false) else {
        return Err(ctx.throw_type_error("RegExp.escape: argument is not a string"));
    };

    let mut buf =
        StringBuf::with_capacity(s.len(), s.is_wide()).map_err(|_| ctx.throw_out_of_memory())?;

    let mut first = true;
    for (_, c) in s.content().chars() {
        let cp = c.to_u32();
        let is_initial = first;
        first = false;

        // spec step 4a: a leading DecimalDigit or AsciiLetter is escaped as
        // `\xHH` so the result cannot be mis-read as a numeric/identity escape.
        if is_initial
            && ((0x30..=0x39).contains(&cp)
                || (0x41..=0x5a).contains(&cp)
                || (0x61..=0x7a).contains(&cp))
        {
            push_hex_escape(&mut buf, b'x', cp, 2).map_err(|_| ctx.throw_out_of_memory())?;
        } else {
            encode_for_regexp_escape(&mut buf, cp).map_err(|_| ctx.throw_out_of_memory())?;
        }
    }

    ctx.create_string_with_buf(buf).map(JSValue::make_string)
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
    FieldDef::FUNC("exec", 1, js_regexp_exec, 0),
    FieldDef::FUNC("compile", 2, js_regexp_compile, 0),
    FieldDef::FUNC("test", 1, js_regexp_test, 0),
    FieldDef::FUNC("toString", 0, js_regexp_toString, 0),
    FieldDef::FUNC("[Symbol.replace]", 2, js_regexp_Symbol_replace, 0),
    FieldDef::FUNC("[Symbol.match]", 1, js_regexp_Symbol_match, 0),
    FieldDef::FUNC("[Symbol.matchAll]", 1, js_regexp_Symbol_matchAll, 0),
    FieldDef::FUNC("[Symbol.search]", 1, js_regexp_Symbol_search, 0),
    FieldDef::FUNC("[Symbol.split]", 2, js_regexp_Symbol_split, 0),
];

pub const js_regexp_funcs: &[FieldDef] = &[
    SPECIES_FIELD,
    FieldDef::FUNC("escape", 1, js_regexp_escape, 0),
];

fn regexp_exec_default(
    ctx: &mut ScopedContext,
    this: JSValue,
    str: StringRef,
) -> Result<Option<Array>, i32> {
    // Compute the start offset first. Reading `lastIndex` runs ToLength, which may
    // invoke user code (e.g. `lastIndex.valueOf()`) that recompiles `this` via
    // `RegExp.prototype.compile`, freeing the previously compiled bytecode. We must
    // therefore fetch the compiled regexp *after* this call, not before, otherwise
    // `re` would be a dangling reference (use-after-free -> SIGSEGV).
    let mut from = get_last_index(ctx, this)? as usize;

    let re: &RegExpData = this.class_payload(ctx, JS_CLASS_REGEXP)?;
    let re_flags = re.flags();

    if !re_flags.has_global() && !re_flags.has_sticky() {
        from = 0;
    }

    if let Some(cap_list) = re.capture(str.content(), from) {
        let (arr, last_index) =
            make_regexp_capture_array(ctx, str.into(), &cap_list, re_flags.has_indices())?;

        if re_flags.has_global() || re_flags.has_sticky() {
            this.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(last_index as u32))?;
        }
        Ok(Some(arr))
    } else {
        if from >= str.content().len() || re_flags.has_sticky() || re_flags.has_global() {
            this.set_property(ctx, Atom::CONST_lastIndex, JSValue::from(0))?;
        }
        Ok(None)
    }
}

/// Invoke regexp.exec() method or call standard `regexp_exec_internal`.
/// returns match result object or None.
pub(super) fn regexp_exec(
    ctx: &mut ScopedContext,
    regexp: JSValue,
    str: StringRef,
) -> Result<Option<Object>, i32> {
    if let Some(o) = regexp.as_object() {
        let exec = o.get_property(ctx, Atom::CONST_exec)?;
        // RegExpExec: only a *callable* custom `exec` bypasses the builtin
        // matcher; non-callable (incl. undefined/null) falls back to it.
        if !exec.match_native_callback(Callback::Func(js_regexp_exec), 0) && exec.is_callable() {
            // call method function
            let s = JSValue::make_string(str);
            let ret = exec.call(ctx, regexp, std::slice::from_ref(&s))?;

            return if let Some(o) = ret.as_object() {
                Ok(Some(o))
            } else if ret.is_null() {
                Ok(None)
            } else {
                Err(ctx.throw_type_error("RegExp exec method must return an object or null"))
            };
        }
    }

    regexp_exec_default(ctx, regexp, str.into()).map(|a| a.map(|a| a.into()))
}
