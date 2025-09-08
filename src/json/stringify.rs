use fallible_collections::TryCollect;
use smallvec::SmallVec;

use crate::{
    JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING,
    atom::{
        JS_FreeAtom,
        consts::{JS_ATOM_empty_string, JS_ATOM_toJSON},
    },
    context::JSContext,
    error::JSResult,
    object::{JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E},
    string::{StringBuf, StringContent},
    value::JSValue,
};

#[derive(Default)]
pub(super) struct JSONStringifyContext {
    replacer_func: JSValue,
    stack: SmallVec<[JSValue; 16]>,

    /// selected property names.
    selected_props: Option<SmallVec<[JSValue; 16]>>,

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
    let mut jsc = JSONStringifyContext {
        stack: SmallVec::new(),
        empty: JS_ATOM_empty_string.to_string(ctx).unwrap().into(),
        ..Default::default()
    };

    if replacer.is_callable() {
        jsc.replacer_func = replacer;
    } else {
        if replacer.is_array(ctx)? {
            let mut selected = SmallVec::<[JSValue; 16]>::new();

            for i in 0..replacer.get_length_prop(ctx)? {
                let k: Option<JSValue> = match replacer.get_property_i64(ctx, i)? {
                    JSValue::Object(o) => {
                        if matches!(o.class_id(), JS_CLASS_STRING | JS_CLASS_NUMBER) {
                            let k: JSValue = JSValue::Object(o).to_string(ctx)?.into();
                            Some(k)
                        } else {
                            None
                        }
                    }
                    v @ JSValue::String(_) => Some(v),
                    v if v.is_number() => {
                        let k: JSValue = v.to_string(ctx)?.into();
                        Some(k)
                    }
                    _ => {
                        continue;
                    }
                };

                // add non-duplicated property name
                if let Some(k) = k {
                    if !selected.iter().any(|x| k.eq(x)) {
                        selected.push(k);
                    }
                }
            }

            jsc.selected_props.replace(selected);
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
    let val = js_json_check(ctx, &mut jsc, &wrapper, obj, &empty)?;
    if val.is_undefined() {
        return Ok(JSValue::UNDEFINED);
    }

    js_json_to_str(ctx, &mut jsc, &val, &StringBuf::Empty)?;
    jsc.buf.shrink_to_fit();
    ctx.create_string_with_buf(jsc.buf).map(JSValue::String)
}

fn js_json_check(
    ctx: &JSContext,
    jsc: &mut JSONStringifyContext,
    holder: &JSValue,
    mut val: JSValue,
    key: &JSValue,
) -> JSResult {
    if val.is_object() {
        let f = val.get_property(ctx, JS_ATOM_toJSON)?;
        if f.is_callable() {
            val = f.call(ctx, &val, std::slice::from_ref(key))?;
        }
    }

    let ret = if !jsc.replacer_func.is_undefined() {
        jsc.replacer_func.call(ctx, holder, &[key.clone(), val])?
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
    val: &JSValue,
    indent: &StringBuf,
) -> Result<(), i32> {
    match val {
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
                        &JSValue::from(i),
                    )?;
                    if v.is_undefined() {
                        v = JSValue::NULL;
                    }

                    js_json_to_str(ctx, jsc, &v, &indent1)?;
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
                let tab = if let Some(sels) = jsc.selected_props.take() {
                    sels.into_iter().try_collect::<Vec<_>>().map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?
                } else {
                    let pks = val.to_object(ctx).and_then(|o| {
                        o.query_own_property_names(ctx, JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK)
                    })?;

                    let mut lst = Vec::<JSValue>::new();
                    lst.try_reserve_exact(pks.len()).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;

                    let mut has_err = false;
                    for pk in pks {
                        if !has_err {
                            if let Ok(k) = pk.atom.to_string_or_symbol(ctx) {
                                lst.push(k);
                            } else {
                                has_err = true;
                            }
                        }
                        JS_FreeAtom(ctx, pk.atom);
                    }
                    lst
                };

                jsc.buf.push_char('{').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

                let mut has_content = false;
                for prop in tab {
                    let el =
                        js_json_check(ctx, jsc, &val, val.get_property_value(ctx, &prop)?, &prop)?;

                    if !el.is_undefined() {
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

                        js_json_to_str(ctx, jsc, &el, &indent1)?;
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
        JSValue::Float64(_) | JSValue::Int(_) => jsc
            .buf
            .push_content(val.to_number(ctx)?.to_string(ctx)?.content())
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            }),
        JSValue::Bool(_) | JSValue::NULL => jsc
            .buf
            .push_content(val.to_string(ctx)?.content())
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            }),
        _ => Ok(()),
    }
}
