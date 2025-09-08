use crate::{
    JS_CLASS_NUMBER, JS_CLASS_STRING,
    array::{js_array_includes, js_array_pop, js_array_push},
    atom::consts::{JS_ATOM_empty_string, JS_ATOM_toJSON},
    context::JSContext,
    error::JSResult,
    function::CallContext,
    iterator::JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    js_core::object::js_object_keys,
    object::JS_PROP_C_W_E,
    string::{StringBuf, StringContent},
    value::JSValue,
};

#[derive(Default)]
pub(super) struct JSONStringifyContext {
    replacer_func: JSValue,
    stack: JSValue,         // Vec<JSValue>
    property_list: JSValue, // Vec<JSValue>
    gap: StringContent,
    empty: JSValue,
    buf: StringBuf,
}

pub fn JS_JSONStringify(
    ctx: &JSContext,
    obj: JSValue,
    replacer: JSValue,
    space: JSValue,
) -> JSResult {
    let mut space: JSValue = space;

    let mut jsc = JSONStringifyContext {
        stack: ctx.create_array()?.into(),
        empty: JS_ATOM_empty_string.to_string(ctx).unwrap().into(),
        ..Default::default()
    };

    if replacer.is_callable() {
        jsc.replacer_func = replacer;
    } else {
        if replacer.is_array(ctx)? {
            /* XXX: enumeration is not fully correct */
            jsc.property_list = ctx.create_array()?.into();
            let mut j = 0;
            for i in 0..replacer.get_length_prop(ctx)? {
                let mut v = replacer.get_property_i64(ctx, i)?;
                if let Some(p) = v.object() {
                    if matches!(p.class_id(), JS_CLASS_STRING | JS_CLASS_NUMBER) {
                        v = v.to_string(ctx)?.into();
                    } else {
                        continue;
                    }
                } else if v.is_number() {
                    v = v.to_string(ctx)?.into();
                } else if !v.is_string() {
                    continue;
                }

                let present = js_array_includes(
                    ctx,
                    CallContext::new_simple(
                        ctx,
                        jsc.property_list.clone(),
                        std::slice::from_ref(&v),
                    ),
                )?
                .to_bool()?;

                if !present {
                    jsc.property_list.set_property_i64(ctx, j, v)?;
                    j += 1;
                }
            }
        }
    }

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
    ctx.create_string_with_buf(jsc.buf).map(JSValue::String)
}

fn js_json_check(
    ctx: &JSContext,
    jsc: &mut JSONStringifyContext,
    holder: &JSValue,
    mut val: JSValue,
    key: JSValue,
) -> JSResult {
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
                Ok(ret)
            } else {
                Ok(JSValue::UNDEFINED)
            }
        }
        JSValue::NULL
        | JSValue::Bool(_)
        | JSValue::String(_)
        | JSValue::Int(_)
        | JSValue::Float64(_) => {
            return Ok(ret);
        }

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
    let sbuf: StringBuf = match &val {
        JSValue::Object(o) => {
            if let Some(pv) = o.0.payload.borrow().value() {
                // object of primitive value
                match pv {
                    JSValue::String(s) => {
                        return s
                            .content()
                            .quote()
                            .and_then(|qs| jsc.buf.push(&qs))
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            });
                    }
                    JSValue::Float64(d) => {
                        return jsc.buf.push_str(&format!("{d}")).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        });
                    }
                    JSValue::Int(n) => {
                        return jsc.buf.push_str(&format!("{n}")).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        });
                    }
                    JSValue::Bool(b) => {
                        return jsc
                            .buf
                            .push_str(if b { "true" } else { "false" })
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            });
                    }
                    JSValue::Symbol(_) => {
                        // symbol is not allowed in json
                        return Ok(());
                    }
                    _ => {
                        #[cfg(debug_assertions)]
                        unreachable!();
                        #[cfg(not(debug_assertions))]
                        {
                            std::hint::unreachable_unchecked();
                        }
                    }
                }
            }

            // check for circular ref
            if js_array_includes(
                ctx,
                CallContext::new_simple(ctx, jsc.stack.clone(), std::slice::from_ref(&val)),
            )?
            .to_bool()?
            {
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
                s1.push(&indent1);
                (s1, StringBuf::from_str(" ")?)
            } else {
                (StringBuf::Empty, StringBuf::Empty)
            };

            // push current val to stack
            js_array_push(
                ctx,
                CallContext::new_simple(ctx, jsc.stack.clone(), std::slice::from_ref(&val)),
            )?;

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
                        JSValue::from(i),
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
                let tab = if !jsc.property_list.is_undefined() {
                    jsc.property_list.clone()
                } else {
                    js_object_keys(
                        ctx,
                        CallContext::new(
                            ctx,
                            JSValue::UNDEFINED,
                            std::slice::from_ref(&val),
                            JS_ITERATOR_KIND_KEY as _,
                            None,
                        ),
                    )?
                };
                let len = tab.get_length_prop(ctx)?;
                jsc.buf.push_char('{').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

                let mut has_content = false;
                for i in 0..len {
                    let prop = tab.get_property_i64(ctx, i)?;
                    let v = js_json_check(
                        ctx,
                        jsc,
                        &val,
                        val.get_property_value(ctx, &prop)?,
                        prop.clone(),
                    )?;

                    if !v.is_undefined() {
                        if has_content {
                            jsc.buf.push_char(',').map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                        }

                        let key = prop
                            .to_string(ctx)
                            .unwrap()
                            .content()
                            .quote()
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;

                        jsc.buf
                            .try_reserve(sep.len() + key.len() + sep1.len() + 1)
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                        jsc.buf.push(&sep).unwrap();
                        jsc.buf.push(&key).unwrap();
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

            js_array_pop(ctx, CallContext::new_simple(ctx, jsc.stack.clone(), &[]))?;
            return Ok(());
        }

        JSValue::String(s) => s.content().quote().map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?,
        JSValue::Float64(d) => {
            if d.is_finite() {
                StringBuf::from_str(format!("{d}").as_str())
            } else {
                StringBuf::from_str("null")
            }?
        }
        JSValue::Int(i) => StringBuf::from_str(format!("{i}").as_str())?,
        JSValue::Bool(b) => StringBuf::from_str(if *b { "true" } else { "false" })?,
        JSValue::NULL => StringBuf::from_str("null")?,
        _ => {
            return Ok(());
        }
    };

    jsc.buf.push(&sbuf).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })
}
