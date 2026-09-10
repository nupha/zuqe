// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use smallvec::SmallVec;

use crate::{
    JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_RAWJSON, JS_CLASS_STRING,
    ScopedContext,
    atom::Atom,
    builtins::iterator::JSIteratorKindEnum,
    error::JSResult,
    object::{JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_PROP_C_W_E},
    string::{StringBuf, StringData, StringRef},
    value::JSValue,
};

struct JSONStringifyContext {
    replacer_func: JSValue,
    replacer_props: Option<Vec<StringRef>>,
    stack: SmallVec<[JSValue; 16]>,
    gap: StringData,
    empty: StringRef,
    buf: StringBuf,
}

impl<'a> ScopedContext<'a> {
    pub(super) fn json_stringify(
        &mut self,
        obj: JSValue,
        replacer: JSValue,
        mut space: JSValue,
    ) -> JSResult {
        let mut jsc = JSONStringifyContext {
            replacer_func: JSValue::UNDEFINED,
            replacer_props: None,
            empty: Atom::CONST_empty_string.force_string(self).unwrap(),
            gap: StringData::Empty,
            buf: StringBuf::Empty,
            stack: SmallVec::new(),
        };

        if replacer.is_callable() {
            jsc.replacer_func = replacer;
        } else if replacer.is_array(self)? {
            let n = replacer.get_length_prop(self)?;

            let mut keylst = Vec::<StringRef>::new();
            keylst
                .try_reserve_exact(n as usize)
                .map_err(|_| self.throw_out_of_memory())?;

            for i in 0..n {
                let k: StringRef = {
                    let v = replacer.get_property_i64(self, i)?;

                    // Per ES spec JSON.stringify step 4:
                    // - String/Number primitive -> ToString(v)
                    // - Object with [[StringData]] or [[NumberData]] -> ToString(v)
                    // - everything else is ignored
                    // `ToString` must go through ToPrimitive so an overridden
                    // `toString`/`valueOf` is honored; do NOT unwrap objects.
                    // `as_string(false)` only matches a *string primitive*, which
                    // is exactly what the `is_string()` guard expressed before.
                    if let Some(s) = v.as_string(false) {
                        s
                    } else if v.is_number() {
                        v.to_string(self)?
                    } else if let Some(o) = v.as_object()
                        && matches!(o.class_id(), JS_CLASS_STRING | JS_CLASS_NUMBER)
                    {
                        v.to_string(self)?
                    } else {
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

        if let Some(p) = space.as_object() {
            match p.class_id() {
                JS_CLASS_NUMBER => {
                    space = space.to_number(self)?.into();
                }
                JS_CLASS_STRING => {
                    space = space.to_string(self).map(JSValue::make_string)?;
                }
                _ => {}
            }
        }

        if space.is_number() {
            let n = space.to_number(self)?.to_i32_clamp(0, 10, 0) as usize;
            jsc.gap = if n == 0 {
                StringData::Empty
            } else {
                StringData::Ascii({
                    let mut a = Vec::<u8>::with_capacity(n);
                    a.extend(std::iter::repeat_n(b' ', n));
                    a.into()
                })
            };
        } else if let Some(s) = space.as_string(true) {
            let mut b =
                StringBuf::with_capacity(s.len(), false).map_err(|_| self.throw_out_of_memory())?;
            b.push_content_range(s.content(), 0..10)
                .map_err(|_| self.throw_out_of_memory())?;
            jsc.gap = self.create_string_with_buf(b)?.content().clone();
        } else {
            jsc.gap = StringData::Empty;
        }

        let mut wrapper: JSValue = self.create_base_object()?.into();
        wrapper.define_property_value(self, Atom::CONST_empty_string, obj, JS_PROP_C_W_E)?;

        let val = js_json_check(self, &jsc, wrapper, obj, jsc.empty)?;
        if val.is_undefined() {
            return Ok(JSValue::UNDEFINED);
        }

        js_json_to_str(self, &mut jsc, val, &StringBuf::Empty)?;

        jsc.buf.shrink_to_fit();
        self.create_string_with_buf(jsc.buf)
            .map(JSValue::make_string)
    }
}

fn js_json_check(
    ctx: &mut ScopedContext,
    jsc: &JSONStringifyContext,
    holder: JSValue,
    val: JSValue,
    key: StringRef,
) -> JSResult {
    let mut val = val;
    let key = JSValue::make_string(key);

    // check for object.toJSON method
    // ECMA specifies this is done only for Object and BigInt */
    if val.is_object() || val.is_bigint() {
        let f = val.get_property(ctx, Atom::CONST_toJSON)?;
        if f.is_callable() {
            val = f.call(ctx, val, std::slice::from_ref(&key))?;
        }
    }

    let ret = if !jsc.replacer_func.is_undefined() {
        jsc.replacer_func.call(ctx, holder, &[key, val])?
    } else {
        val
    };

    if let Some(o) = ret.as_object() {
        if o.is_callable() {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(ret)
        }
    } else if ret.is_null() || ret.is_bool() || ret.is_string() || ret.is_int() || ret.is_float64()
    {
        Ok(ret)
    } else if ret.is_bigint() {
        // SerializeJSONValue: a BigInt value is not serializable → TypeError.
        Err(ctx.throw_type_error("Do not know how to serialize a BigInt"))
    } else {
        debug_assert!(!ret.is_exception());
        Ok(JSValue::UNDEFINED)
    }
}

fn js_json_to_str(
    ctx: &mut ScopedContext,
    jsc: &mut JSONStringifyContext,
    val: JSValue,
    indent: &StringBuf,
) -> Result<(), i32> {
    match val.tag() {
        crate::JSValueTag::Object => {
            let o = unsafe { val.as_object_unchecked() };
            match o.class_id() {
                JS_CLASS_STRING => {
                    return val
                        .to_string(ctx)?
                        .content()
                        .quote()
                        .and_then(|qs| jsc.buf.push(&qs))
                        .map_err(|_| ctx.throw_out_of_memory());
                }
                JS_CLASS_NUMBER => {
                    return jsc
                        .buf
                        .push_str(&format!("{}", o.to_number(ctx)?))
                        // .push_content(JSValue::from(o.to_number(ctx)?).to_string(ctx)?.content())
                        .map_err(|_| ctx.throw_out_of_memory());
                }
                JS_CLASS_BOOLEAN => {
                    return jsc
                        .buf
                        .push_content(val.to_string(ctx)?.content())
                        .map_err(|_| ctx.throw_out_of_memory());
                }
                JS_CLASS_RAWJSON => {
                    let raw = val.get_property(ctx, Atom::CONST_rawJSON)?;
                    return jsc
                        .buf
                        .push_content(raw.to_string(ctx)?.content())
                        .map_err(|_| ctx.throw_out_of_memory());
                }
                JS_CLASS_BIG_INT => {
                    // Boxed BigInt: unwrap [[BigIntData]] then SerializeJSONValue
                    // rejects the BigInt primitive with a TypeError.
                    return Err(ctx.throw_type_error("Do not know how to serialize a BigInt"));
                }
                _ => {}
            }

            // check for circular ref
            if jsc.stack.iter().any(|v| val.eq(v)) {
                return Err(ctx.throw_type_error("circular reference"));
            }

            let mut indent1 = StringBuf::with_capacity(indent.len() + jsc.gap.len(), false)
                .map_err(|_| ctx.throw_out_of_memory())?;
            indent1.push(indent).unwrap();
            indent1.push_content(&jsc.gap).unwrap();

            let (sep, sep1) = if !jsc.gap.is_empty() {
                let mut s1 = StringBuf::with_capacity(indent1.len() + 1, false)
                    .map_err(|_| ctx.throw_out_of_memory())?;
                if !jsc.gap.is_empty() {
                    s1.push_u8(b'\n').unwrap();
                }
                s1.push(&indent1).unwrap();
                (s1, StringBuf::from_str(" ")?)
            } else {
                (StringBuf::Empty, StringBuf::Empty)
            };

            // push current val to stack
            jsc.stack
                .try_reserve(16)
                .map_err(|_| ctx.throw_out_of_memory())?;
            jsc.stack.push(val);

            if val.is_array(ctx)? {
                let len = val.get_length_prop(ctx)?;
                jsc.buf
                    .push_u8(b'[')
                    .map_err(|_| ctx.throw_out_of_memory())?;

                for i in 0..len {
                    if i > 0 {
                        jsc.buf
                            .push_u8(b',')
                            .map_err(|_| ctx.throw_out_of_memory())?;
                    }

                    jsc.buf.push(&sep).map_err(|_| ctx.throw_out_of_memory())?;

                    let nth = ctx.create_string_from_utf8(format!("{i}").into_bytes())?;
                    let prop_val = val.get_property_i64(ctx, i)?;
                    let mut v = js_json_check(ctx, jsc, val, prop_val, nth)?;
                    if v.is_undefined() {
                        v = JSValue::NULL;
                    }
                    js_json_to_str(ctx, jsc, v, &indent1)?;
                }

                jsc.buf
                    .try_reserve(indent.len() + 2)
                    .map_err(|_| ctx.throw_out_of_memory())?;
                if len > 0 && !jsc.gap.is_empty() {
                    jsc.buf.push_u8(b'\n').unwrap();
                    jsc.buf.push(indent).unwrap();
                }
                jsc.buf.push_u8(b']').unwrap();
            } else {
                let tab: Box<[StringRef]> = if let Some(lst) = jsc.replacer_props.as_ref() {
                    // Unsafe: use raw pointer to avoid borrowing jsc,
                    // because jsc will be borrowed mut later.
                    // guarantee `replacer_props` won't be modified in this function
                    let lst = unsafe { std::slice::from_raw_parts(lst.as_ptr(), lst.len()) };
                    lst.to_vec().into()
                } else {
                    // query own property names.
                    let kvs = val.get_own_property_kvs(
                        ctx,
                        JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK,
                        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
                    )?;

                    let mut lst = Vec::<StringRef>::new();
                    lst.try_reserve_exact(kvs.len())
                        .map_err(|_| ctx.throw_out_of_memory())?;
                    for (key, _) in kvs {
                        lst.push(key.as_string(true).unwrap());
                    }
                    lst.into()
                };

                jsc.buf
                    .push_u8(b'{')
                    .map_err(|_| ctx.throw_out_of_memory())?;

                let mut has_content = false;
                for k in tab.iter() {
                    let key = *k;
                    let prop_val = val.get_property_value(ctx, JSValue::make_string(key))?;

                    let v = js_json_check(ctx, jsc, val, prop_val, key)?;
                    if !v.is_undefined() {
                        if has_content {
                            jsc.buf
                                .push_u8(b',')
                                .map_err(|_| ctx.throw_out_of_memory())?;
                        }

                        let qkey = key
                            .content()
                            .quote()
                            .map_err(|_| ctx.throw_out_of_memory())?;

                        jsc.buf
                            .try_reserve(sep.len() + qkey.len() + sep1.len() + 1)
                            .map_err(|_| ctx.throw_out_of_memory())?;
                        jsc.buf.push(&sep).unwrap();
                        jsc.buf.push(&qkey).unwrap();
                        jsc.buf.push_u8(b':').unwrap();
                        jsc.buf.push(&sep1).unwrap();

                        js_json_to_str(ctx, jsc, v, &indent1)?;
                        has_content = true;
                    }
                }

                if has_content && !jsc.gap.is_empty() {
                    jsc.buf
                        .try_reserve(indent.len() + 1)
                        .map_err(|_| ctx.throw_out_of_memory())?;
                    jsc.buf.push_u8(b'\n').unwrap();
                    jsc.buf.push(indent).unwrap();
                }

                jsc.buf
                    .push_u8(b'}')
                    .map_err(|_| ctx.throw_out_of_memory())?;
            }

            jsc.stack.pop();
            Ok(())
        }

        crate::JSValueTag::String => {
            let s = unsafe { val.as_string_unchecked() };
            s.content()
                .quote()
                .and_then(|qs| jsc.buf.push(&qs))
                .map_err(|_| ctx.throw_out_of_memory())
        }

        crate::JSValueTag::Int | crate::JSValueTag::Bool | crate::JSValueTag::Null => jsc
            .buf
            .push_content(val.to_string(ctx)?.content())
            .map_err(|_| ctx.throw_out_of_memory()),
        crate::JSValueTag::Float64 => {
            let d = unsafe { val.as_float_unchecked() };
            if !d.is_finite() {
                jsc.buf.push_str("null")
            } else {
                jsc.buf.push_content(val.to_string(ctx)?.content())
            }
            .map_err(|_| ctx.throw_out_of_memory())
        }

        _ => Ok(()),
    }
}
