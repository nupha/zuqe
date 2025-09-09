use std::borrow::Cow;

use smallvec::SmallVec;

use crate::{
    JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING,
    atom::{
        JS_FreeAtom,
        consts::{JS_ATOM_empty_string, JS_ATOM_toJSON},
    },
    context::JSContext,
    error::JSResult,
    function::CallContext,
    iterator::JSIteratorKindEnum::{self, JS_ITERATOR_KIND_KEY},
    js_core::object::js_object_keys,
    object::{JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E},
    string::{JSString, StringBuf, StringContent},
    value::JSValue,
};

pub(super) struct JSONStringifyContext {
    replacer_func: JSValue,
    replacer_props: Option<Vec<JSString>>,
    stack: SmallVec<[JSValue; 16]>,
    gap: StringContent,
    empty: JSString,
    buf: StringBuf,
}

pub fn JS_JSONStringify(
    ctx: &JSContext,
    obj: JSValue,
    replacer: JSValue,
    space: JSValue,
) -> JSResult {
    let mut jsc = JSONStringifyContext {
        replacer_func: JSValue::UNDEFINED,
        stack: SmallVec::new(),
        replacer_props: None,
        empty: JS_ATOM_empty_string.to_string(ctx).unwrap(),
        gap: StringContent::Empty,
        buf: StringBuf::Empty,
    };

    if replacer.is_callable() {
        jsc.replacer_func = replacer;
    } else {
        if replacer.is_array(ctx)? {
            let n = replacer.get_length_prop(ctx)?;
            let mut keylst = Vec::<JSString>::new();
            keylst.try_reserve_exact(n as usize).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            for i in 0..n {
                let k: JSString = match replacer.get_property_i64(ctx, i)? {
                    JSValue::Object(o) => {
                        if matches!(o.class_id(), JS_CLASS_STRING | JS_CLASS_NUMBER) {
                            JSValue::Object(o).to_string(ctx)?
                        } else {
                            continue; // symbols are ignored
                        }
                    }
                    JSValue::String(s) => s,
                    v if v.is_number() => v.to_string(ctx)?,
                    _ => {
                        continue; // ignored
                    }
                };

                // add non-duplicated property key
                if !keylst.iter().any(|x| k.eq(x)) {
                    keylst.push(k);
                }
            }

            jsc.replacer_props.replace(keylst);
        }
    }

    let mut space: JSValue = space;
    if let Some(p) = space.object() {
        match p.class_id() {
            JS_CLASS_NUMBER => {
                space = space.to_number(ctx)?;
            }
            JS_CLASS_STRING => {
                space = space.to_string(ctx)?.into();
            }
            _ => {}
        }
    }

    if space.is_number() {
        let n = space.to_i32_clamp(ctx, 0, 10, 0)? as usize;
        jsc.gap = unsafe {
            let spc = [b' '; 10];
            ctx.create_string_from_str(std::str::from_utf8_unchecked(&spc[..n]))?
                .content()
                .clone()
        };
    } else if let JSValue::String(s) = space {
        let mut b = StringBuf::with_capacity(s.len(), s.is_wide()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        b.push_content_range(s.content(), 0..10).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        jsc.gap = ctx.create_string_with_buf(b)?.content().clone();
    } else {
        jsc.gap = StringContent::Empty;
    }

    let wrapper: JSValue = ctx.create_base_object()?.into();
    wrapper.define_property_value(ctx, JS_ATOM_empty_string, obj.clone(), JS_PROP_C_W_E)?;

    let empty = jsc.empty.clone();
    let val = js_json_check(ctx, &mut jsc, &wrapper, obj, empty)?;
    if val.is_undefined() {
        return Ok(JSValue::UNDEFINED);
    }

    js_json_to_str(ctx, &mut jsc, val, &StringBuf::Empty)?;
    jsc.buf.shrink_to_fit();
    ctx.create_string_with_buf(jsc.buf).map(JSValue::String)
}

fn js_json_check(
    ctx: &JSContext,
    jsc: &JSONStringifyContext,
    holder: &JSValue,
    val: JSValue,
    key: JSString,
) -> JSResult {
    let mut val = val;
    let key = JSValue::String(key);

    if val.is_object() {
        let f = val.get_property(ctx, JS_ATOM_toJSON)?;
        if f.is_callable() {
            val = f.call(ctx, &val, std::slice::from_ref(&key))?;
        }
    }

    let ret = if !jsc.replacer_func.is_undefined() {
        jsc.replacer_func.call(ctx, holder, &[key, val])?
    } else {
        val
    };

    match &ret {
        JSValue::Object(o) => {
            if o.is_callable() {
                Ok(JSValue::UNDEFINED)
            } else {
                Ok(ret)
            }
        }
        JSValue::NULL
        | JSValue::Bool(_)
        | JSValue::String(_)
        | JSValue::Int(_)
        | JSValue::Float64(_) => Ok(ret),

        #[cfg(debug_assertions)]
        JSValue::EXCEPTION => {
            unreachable!()
        }
        _ => Ok(JSValue::UNDEFINED),
    }
}

fn js_json_to_str(
    ctx: &JSContext,
    jsc: &mut JSONStringifyContext,
    val: JSValue,
    indent: &StringBuf,
) -> Result<(), i32> {
    match &val {
        JSValue::Object(o) => {
            match o.class_id() {
                JS_CLASS_STRING => {
                    return val
                        .to_string(ctx)?
                        .content()
                        .quote()
                        .and_then(|qs| jsc.buf.push(&qs))
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        });
                }
                JS_CLASS_NUMBER => {
                    return jsc
                        .buf
                        .push_content(o.to_number(ctx)?.to_string(ctx)?.content())
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        });
                }
                JS_CLASS_BOOLEAN => {
                    return jsc
                        .buf
                        .push_content(val.to_string(ctx)?.content())
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        });
                }
                _ => {}
            }

            // check for circular ref
            if jsc.stack.iter().any(|v| val.eq(v)) {
                ctx.throw_type_error("circular reference");
                return Err(-1);
            }

            let mut indent1 =
                StringBuf::with_capacity(indent.len() + jsc.gap.len(), indent.is_wide()).map_err(
                    |_| {
                        ctx.throw_out_of_memory();
                        -1
                    },
                )?;
            indent1.push(indent).unwrap();
            indent1.push_content(&jsc.gap).unwrap();

            let (sep, sep1) = if !jsc.gap.is_empty() {
                let mut s1 = StringBuf::with_capacity(indent1.len() + 1, false).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                if !jsc.gap.is_empty() {
                    s1.push_char('\n').unwrap();
                }
                s1.push(&indent1).unwrap();
                (s1, StringBuf::from_str(" ")?)
            } else {
                (StringBuf::Empty, StringBuf::Empty)
            };

            // push current val to stack
            jsc.stack.try_reserve(16).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            jsc.stack.push(val.clone());

            if val.is_array(ctx)? {
                let len = val.get_length_prop(ctx)?;

                jsc.buf.push_char('[').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

                for i in 0..len {
                    if i > 0 {
                        jsc.buf.push_char(',').map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                    }

                    jsc.buf.push(&sep).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;

                    let mut v = js_json_check(
                        ctx,
                        jsc,
                        &val,
                        val.get_property_i64(ctx, i)?,
                        ctx.create_string_from_str(&format!("{i}"))?,
                    )?;
                    if v.is_undefined() {
                        v = JSValue::NULL;
                    }
                    js_json_to_str(ctx, jsc, v, &indent1)?;
                }

                jsc.buf.try_reserve(indent.len() + 2).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                if len > 0 && !jsc.gap.is_empty() {
                    jsc.buf.push_char('\n').unwrap();
                    jsc.buf.push(indent).unwrap();
                }
                jsc.buf.push_char(']').unwrap();
            } else {
                let tab: Cow<[JSString]> = if let Some(lst) = jsc.replacer_props.as_ref() {
                    // Unsafe: use raw pointer to avoid borrowing jsc,
                    // because jsc will be borrowed mut later.
                    // guarantee `replacer_props` won't be modified in this function
                    let lst = unsafe { std::slice::from_raw_parts(lst.as_ptr(), lst.len()) };
                    lst.into()
                } else {
                    // query own property names.
                    let kvs = val.query_own_property_kvs(
                        ctx,
                        JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK,
                        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
                    )?;

                    let mut lst = Vec::<JSString>::new();
                    lst.try_reserve_exact(kvs.len()).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                    for (key, _) in kvs {
                        lst.push(JSString::try_from(key).unwrap());
                    }
                    lst.into()
                };

                jsc.buf.push_char('{').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

                let mut has_content = false;
                for key in tab.as_ref() {
                    let v = js_json_check(
                        ctx,
                        jsc,
                        &val,
                        val.get_property_value(ctx, &key.clone().into())?,
                        key.clone(),
                    )?;

                    if !v.is_undefined() {
                        if has_content {
                            jsc.buf.push_char(',').map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                        }

                        let qkey = key.content().quote().map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;

                        jsc.buf
                            .try_reserve(sep.len() + qkey.len() + sep1.len() + 1)
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                        jsc.buf.push(&sep).unwrap();
                        jsc.buf.push(&qkey).unwrap();
                        jsc.buf.push_char(':').unwrap();
                        jsc.buf.push(&sep1).unwrap();

                        js_json_to_str(ctx, jsc, v, &indent1)?;
                        has_content = true;
                    }
                }

                if has_content && !jsc.gap.is_empty() {
                    jsc.buf.try_reserve(indent.len() + 1).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                    jsc.buf.push_char('\n').unwrap();
                    jsc.buf.push(&indent).unwrap();
                }

                jsc.buf.push_char('}').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }

            jsc.stack.pop();
            Ok(())
        }

        JSValue::String(s) => s
            .content()
            .quote()
            .and_then(|qs| jsc.buf.push(&qs))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            }),
        JSValue::Int(_) | JSValue::Bool(_) | JSValue::NULL => jsc
            .buf
            .push_content(val.to_string(ctx)?.content())
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            }),
        JSValue::Float64(d) => if !d.is_finite() {
            jsc.buf.push_str("null")
        } else {
            jsc.buf.push_content(val.to_string(ctx)?.content())
        }
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        }),
        _ => Ok(()),
    }
}
