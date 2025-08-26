use crate::{
    JS_ALIAS_DEF, JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_NATIVE_ITERATOR_NEXT_DEF,
    JS_PROP_STRING_DEF,
    array::{JS_CopySubArray, JS_ToArrayLengthFree},
    atom::consts::{
        JS_ATOM_Symbol_isConcatSpreadable, JS_ATOM_Symbol_iterator, JS_ATOM_Symbol_species,
        JS_ATOM_constructor, JS_ATOM_join, JS_ATOM_length, JS_ATOM_set,
    },
    bytecode::arithmatic::JSStrictEqModeEnum,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, JS_CallConstructor, JS_GetFunctionRealm},
    iterator::{JSIteratorKindEnum, js_array_iterator_next, js_create_array_iterator},
    js_core::{js_get_this, object::js_object_toString},
    number::MAX_SAFE_INTEGER,
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_THROW, ObjectDetail, js_get_length64},
    runtime::js_check_stack_overflow,
    string::{JSString, StringBuf},
    typed_array::{js_typed_array___speciesCreate, js_typed_array_get_length_internal},
    value::JSValue,
};

use super::ArrayObjPayload;

pub const special_find: i32 = 0;
pub const special_findIndex: i32 = 1;
pub const special_findLast: i32 = 2;
pub const special_findLastIndex: i32 = 3;

pub const special_indexOf: i32 = 0;
pub const special_lastIndexOf: i32 = 1;
pub const special_includes: i32 = -1;

pub const special_every: i32 = 0;
pub const special_some: i32 = 1;
pub const special_forEach: i32 = 2;
pub const special_map: i32 = 3;
pub const special_filter: i32 = 4;
pub const special_TA: i32 = 8;

pub const special_reduce: i32 = 0;
pub const special_reduceRight: i32 = 1;

pub fn js_array_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let obj = ctx
        .create_object_apply_ctor(new_target, ObjectDetail::ARRAY(ArrayObjPayload::default()))
        .map_err(|_| -1)?;

    if cc.length() == 1 && cc.arg(0).is_number() {
        let len = unsafe { JS_ToArrayLengthFree(ctx, cc.arg(0).clone(), true)? };
        obj.set_property(ctx, JS_ATOM_length, JSValue::from(len))?;
    } else {
        for (i, a) in cc.arguments().iter().enumerate() {
            obj.set_property_u32(ctx, i as u32, a.clone())?;
        }
    }

    Ok(obj.into())
}

fn JS_ArraySpeciesCreate(ctx: &JSContext, obj: &JSValue, len_val: &JSValue) -> JSResult {
    if !obj.is_array(ctx)? {
        return js_array_constructor(
            ctx,
            CallContext::new(
                ctx,
                JSValue::UNDEFINED,
                std::slice::from_ref(len_val),
                0,
                None,
            ),
        );
    }

    let mut ctor = obj.get_property(ctx, JS_ATOM_constructor)?;

    if ctor.is_constructor() {
        // legacy web compatibility
        let realm = JS_GetFunctionRealm(ctx, &ctor)?;
        if !realm.eq(ctx) && ctor.is_same(&realm.0.borrow().array_ctor) {
            ctor = JSValue::UNDEFINED;
        }
    }

    if ctor.is_object() {
        let species = ctor.get_property(ctx, JS_ATOM_Symbol_species)?;
        ctor = species;
        if ctor.is_null() {
            ctor = JSValue::UNDEFINED;
        }
    }

    if ctor.is_undefined() {
        js_array_constructor(
            ctx,
            CallContext::new(
                ctx,
                JSValue::UNDEFINED,
                std::slice::from_ref(len_val),
                0,
                None,
            ),
        )
    } else {
        JS_CallConstructor(ctx, &ctor, std::slice::from_ref(len_val)).err()
    }
}

pub unsafe fn JS_isConcatSpreadable(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    if !obj.is_object() {
        return Ok(false);
    }

    let tmp = obj.get_property(ctx, JS_ATOM_Symbol_isConcatSpreadable)?;

    if !tmp.is_undefined() {
        tmp.to_bool()
    } else {
        obj.is_array(ctx)
    }
}

pub fn js_array_at(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let mut idx: i64 = cc.arg(0).coerce_to_i64_saturating(ctx)?;
    if idx < 0 {
        idx = len + idx;
    }

    let ret: JSValue = if idx < 0 {
        // bad index
        JSValue::UNDEFINED
    } else if let Some(val) = obj
        .with_fast_array_values(|a| {
            if a.len() == len as usize {
                Some(a[idx as usize].clone())
            } else {
                None
            }
        })
        .flatten()
    {
        // fast array element
        val
    } else {
        // slow array
        obj.try_get_property_i64(ctx, idx)?
            .unwrap_or(JSValue::UNDEFINED)
    };

    Ok(ret)
}

pub fn js_array_with(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let mut idx: i64 = cc.arg(0).coerce_to_i64_saturating(ctx)?;
    if idx < 0 {
        idx = len + idx;
    }

    if idx < 0 || idx >= len {
        ctx.throw_range_error("out of bound");
        return Err(-1);
    }

    let arr = ctx.create_array(std::cmp::max(len, 0) as usize)?;

    arr.0
        .with_fast_array_values_mut(|dst| -> Result<(), i32> {
            if obj
                .with_fast_array_values(|src| {
                    if src.len() == len as usize {
                        // fast array
                        for v in &src[0..(idx as usize)] {
                            dst.push(v.clone());
                        }

                        // the modified element
                        dst.push(cc.arg(1).clone());

                        for v in &src[idx as usize..] {
                            dst.push(v.clone());
                        }

                        Some(())
                    } else {
                        None
                    }
                })
                .flatten()
                .is_none()
            {
                // slow array
                // 0..idx
                for i in 0..idx {
                    match obj.try_get_property_i64(ctx, i) {
                        Ok(Some(v)) => {
                            dst.push(v);
                        }
                        Ok(None) => {
                            dst.push(JSValue::UNDEFINED);
                        }
                        Err(_) => {
                            // fill rest with undefined
                            for _ in i..len {
                                dst.push(JSValue::UNDEFINED);
                            }
                            return Err(-1);
                        }
                    }
                }

                // updated value
                dst.push(cc.arg(1).clone());

                // idx..end
                for i in idx..len {
                    match obj.try_get_property_i64(ctx, i) {
                        Ok(Some(v)) => {
                            dst.push(v);
                        }
                        Ok(None) => {
                            dst.push(JSValue::UNDEFINED);
                        }
                        Err(_) => {
                            // fill rest with undefined
                            for _ in i..len {
                                dst.push(JSValue::UNDEFINED);
                            }
                            return Err(-1);
                        }
                    }
                }
            }

            Ok(())
        })
        .unwrap()?;

    arr.0
        .set_property(ctx, JS_ATOM_length, JSValue::from(len))?;
    Ok(arr.into())
}

pub fn js_array_concat(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let arr = JS_ArraySpeciesCreate(ctx, &obj, &JSValue::Int(0))?;

    let mut n = 0;

    for i in -1..cc.length() as i32 {
        let e = if i < 0 { &obj } else { cc.arg(i as usize) };
        let spreadable = unsafe { JS_isConcatSpreadable(ctx, e)? };

        if spreadable {
            let len = js_get_length64(ctx, &e)?;

            if n + len > MAX_SAFE_INTEGER {
                ctx.throw_type_error("Array loo long");
                return Err(-1);
            }

            for k in 0..len {
                if let Some(val) = e.try_get_property_i64(ctx, k)? {
                    arr.define_property_value_i64(ctx, n, val, JS_PROP_C_W_E | JS_PROP_THROW)?;
                }
                n += 1;
            }
        } else {
            if n >= MAX_SAFE_INTEGER {
                ctx.throw_type_error("Array loo long");
                return Err(-1);
            }

            arr.define_property_value_i64(ctx, n, e.clone(), JS_PROP_C_W_E | JS_PROP_THROW)?;
            n += 1;
        }
    }

    arr.set_property(ctx, JS_ATOM_length, JSValue::from(n))?;
    Ok(arr)
}

pub fn js_array_every(ctx: &JSContext, cc: CallContext) -> JSResult {
    let special = cc.magic();

    let obj: JSValue;
    let len: i64;

    if special & special_TA as i32 != 0 {
        obj = cc.this().clone();
        len = unsafe { js_typed_array_get_length_internal(ctx, &obj)? } as i64;
    } else {
        obj = cc.this().to_object(ctx)?.into();
        len = js_get_length64(ctx, &obj)?;
    }

    let func = cc.arg(0);
    let this_arg = if cc.length() > 1 {
        cc.arg(1)
    } else {
        &JSValue::UNDEFINED
    };
    func.ensure_callable(ctx)?;

    let mut ret: JSValue;
    match special {
        s if s == special_every || s == special_every | special_TA => {
            ret = JSValue::Bool(true);
        }
        s if s == special_some || s == special_some | special_TA => {
            ret = JSValue::Bool(false);
        }
        special_map => {
            ret = JS_ArraySpeciesCreate(ctx, &obj, &JSValue::from(len))?;
        }
        special_filter => {
            ret = JS_ArraySpeciesCreate(ctx, &obj, &JSValue::Int(0))?;
        }
        s if s == special_map | special_TA => {
            ret = js_typed_array___speciesCreate(
                ctx,
                &JSValue::UNDEFINED,
                &[obj.clone(), JSValue::from(len as i32)],
            )?;
        }
        s if s == special_filter | special_TA => {
            ret = ctx.create_array(0)?.into();
        }
        _ => {
            ret = JSValue::UNDEFINED;
        }
    }

    let mut n = 0;
    let mut present: bool;

    for k in 0..len {
        let mut val = JSValue::UNDEFINED;

        if special & special_TA as i32 != 0 {
            val = obj.get_property_i64(ctx, k)?;
            present = true;
        } else {
            match obj.try_get_property_i64(ctx, k)? {
                Some(r) => {
                    val = r;
                    present = true;
                }
                None => {
                    present = false;
                }
            }
        }

        if present {
            let index_val = JSValue::from(k).err()?;
            let res = func.call(ctx, &this_arg, &[val.clone(), index_val, obj.clone()])?;

            match special {
                s if s == special_every || s == special_every | special_TA => {
                    if let Ok(false) = res.to_bool() {
                        ret = JSValue::Bool(false);
                        break;
                    }
                }
                s if s == special_some || s == special_some | special_TA => {
                    if res.to_bool().unwrap_or(true) {
                        ret = JSValue::Bool(true);
                        break;
                    }
                }
                special_map => {
                    ret.define_property_value_i64(ctx, k, res, JS_PROP_C_W_E | JS_PROP_THROW)?;
                }
                s if s == special_map | special_TA => {
                    ret.set_property_value(ctx, &JSValue::Int(k as i32), res, JS_PROP_THROW)?;
                }
                s if s == special_filter || s == special_filter | special_TA => {
                    if res.to_bool().unwrap_or(true) {
                        ret.define_property_value_i64(ctx, n, val, JS_PROP_C_W_E | JS_PROP_THROW)?;
                        n += 1;
                    }
                }
                _ => {}
            }
        }
    }

    if special == (special_filter | special_TA) as i32 {
        let arr = js_typed_array___speciesCreate(
            ctx,
            &JSValue::UNDEFINED,
            &[obj, JSValue::Int(n as i32)],
        )?;
        arr.invoke(ctx, JS_ATOM_set, &[ret, JSValue::Int(n as i32)])?;
        ret = arr;
    }

    Ok(ret)
}

pub fn js_array_reduce(ctx: &JSContext, cc: CallContext) -> JSResult {
    let special = cc.magic();

    let obj: JSValue;
    let len: i64;
    if special & special_TA as i32 != 0 {
        obj = cc.this().clone();
        len = unsafe { js_typed_array_get_length_internal(ctx, &obj)? as i64 };
    } else {
        obj = cc.this().to_object(ctx)?.into();
        len = js_get_length64(ctx, &obj)?;
    }

    let func = cc.arg(0);
    func.ensure_callable(ctx)?;

    let mut acc: JSValue;
    let mut k = 0;

    if cc.length() > 1 {
        acc = cc.arg(1).clone();
    } else {
        loop {
            if k >= len {
                ctx.throw_type_error("empty array");
                return Err(-1);
            }

            let k1 = if special & special_reduceRight != 0 {
                len - k - 1
            } else {
                k
            };
            k += 1;

            if special & special_TA as i32 != 0 {
                acc = obj.get_property_i64(ctx, k1)?;
                break;
            } else {
                match obj.try_get_property_i64(ctx, k1)? {
                    Some(r) => {
                        acc = r;
                        break;
                    }
                    None => {}
                }
            }
        }
    }

    let mut val = JSValue::UNDEFINED;
    while k < len {
        let k1 = if special & special_reduceRight != 0 {
            len - k - 1
        } else {
            k
        };
        k += 1;

        let present = if special & special_TA as i32 != 0 {
            val = obj.get_property_i64(ctx, k1)?;
            true
        } else {
            match obj.try_get_property_i64(ctx, k1)? {
                Some(r) => {
                    val = r;
                    true
                }
                None => false,
            }
        };

        if present {
            let index_val = JSValue::from(k1 as i64).err()?;
            acc = func.call(
                ctx,
                &JSValue::UNDEFINED,
                &[acc, val.clone(), index_val, obj.clone()],
            )?;
        }
    }

    Ok(acc)
}

pub fn js_array_fill(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let mut start = if cc.length() > 1 && !cc.arg(1).is_undefined() {
        cc.arg(1).coerce_to_i64_clamp(ctx, 0, len, len)?
    } else {
        0
    };

    let end = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).coerce_to_i64_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    // XXX: should special case fast arrays
    while start < end {
        obj.set_property_i64(ctx, start, cc.arg(0).clone())?;
        start += 1;
    }

    Ok(obj)
}

pub fn js_array_find(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mode = cc.magic();
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let func = cc.arg(0);
    func.ensure_callable(ctx)?;

    let this_val = cc.this();
    let this_arg = if cc.length() > 1 {
        &cc.arg(1)
    } else {
        &JSValue::UNDEFINED
    };

    let mut k: i64;
    let dir: i64;
    let end: i64;

    if matches!(mode, special_findLast | special_findLastIndex) {
        k = len - 1;
        dir = -1;
        end = -1;
    } else {
        k = 0;
        dir = 1;
        end = len;
    }

    // TODO: add fast path for fast arrays
    while k != end {
        let index_val = JSValue::from(k);
        let val = obj.get_property_value(ctx, &index_val)?;
        let res = func.call(
            ctx,
            this_arg,
            &[val.clone(), index_val.clone(), this_val.clone()],
        )?;

        if res.to_bool()? {
            // found
            return if matches!(mode, special_findIndex | special_findLastIndex) {
                Ok(index_val)
            } else {
                Ok(val)
            };
        }

        k += dir;
    }

    Ok(
        if matches!(mode, special_findIndex | special_findLastIndex) {
            JSValue::Int(-1)
        } else {
            JSValue::UNDEFINED
        },
    )
}

pub fn js_array_indexOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let mut res = -1;
    if len > 0 {
        let mut n = if cc.length() > 1 {
            cc.arg(1).coerce_to_i64_clamp(ctx, 0, len, len)?
        } else {
            0
        };

        let target = cc.arg(0).clone();

        // try fast array
        let found = obj
            .with_fast_array_values(|lst| {
                let start = n as usize;
                for (i, el) in lst[start..].iter().enumerate() {
                    if target.strict_eq_with_mode(el, JSStrictEqModeEnum::JS_EQ_STRICT) {
                        return Some(start + i); // found
                    }
                }
                None
            })
            .flatten();

        if let Some(i) = found {
            return Ok(JSValue::from(i as i64));
        }

        // try properties
        while n < len {
            if let Some(v) = obj.try_get_property_i64(ctx, n)? {
                let b = target.strict_eq_with_mode(&v, JSStrictEqModeEnum::JS_EQ_STRICT);
                if b {
                    res = n;
                    break;
                }
            }
            n += 1;
        }
    }

    Ok(JSValue::from(res))
}

pub fn js_array_lastIndexOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;
    let target = cc.arg(0);

    let mut res: i64 = -1;

    if len > 0 {
        let cnt = if cc.length() > 1 {
            cc.arg(1).coerce_to_i64_clamp(ctx, -1, len - 1, len)?
        } else {
            len - 1
        };

        // TODO: should special case fast arrays
        for n in (0..=cnt).rev() {
            if let Some(v) = obj.try_get_property_i64(ctx, n)? {
                let b = target.strict_eq_with_mode(&v, JSStrictEqModeEnum::JS_EQ_STRICT);
                if b {
                    res = n;
                    break;
                }
            }
        }
    }

    Ok(JSValue::from(res))
}

pub fn js_array_includes(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;
    let target = cc.arg(0);

    let mut res = false;

    if len > 0 {
        let mut n = if cc.length() > 1 {
            cc.arg(1).coerce_to_i64_clamp(ctx, 0, len, len)?
        } else {
            0
        };

        if let Some(found) = obj.with_fast_array_values(|lst| {
            let start = n as usize;
            for el in &lst[start..] {
                if target.strict_eq_with_mode(el, JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO) {
                    return true;
                }
            }
            false
        }) {
            // is fast array
            return Ok(JSValue::Bool(found));
        }

        // try properties
        while n < len {
            let val: JSValue = obj.get_property_i64(ctx, n)?.into();
            let b = target.strict_eq_with_mode(&val, JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO);
            if b {
                res = true;
                break;
            }
            n += 1;
        }
    }

    Ok(JSValue::Bool(res))
}

pub fn js_array_join(ctx: &JSContext, cc: CallContext) -> JSResult {
    let toLocaleString = cc.magic();
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let n = js_get_length64(ctx, &obj)?;

    let mut c = ',' as i32; // default separator
    let mut p: Option<JSString> = None;

    if toLocaleString == 0 && cc.length() > 0 && !cc.arg(0).is_undefined() {
        let sep = cc.arg(0).to_string(ctx)?;
        c = if sep.buf_len() == 1 && !sep.is_wide() {
            sep.content().as_bytes()[0] as i32
        } else {
            -1
        };
        p.replace(sep.clone());
    }

    let mut content = StringBuf::Empty;

    for i in 0..n {
        if i > 0 {
            // append separator
            if c >= 0 {
                let _ = content.push_u8(c as u8);
            } else {
                let _ = content.push(p.as_ref().unwrap().content());
            }
        }

        let el = obj.get_property_u32(ctx, i as u32)?;
        if !el.is_undefined_or_null() {
            let ss = if toLocaleString != 0 {
                el.to_locale_string(ctx)?
            } else if let JSValue::String(s) = el {
                s
            } else {
                el.to_string(ctx)?
            };
            content.push(ss.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?
        }
    }

    ctx.create_string_with_buf(content).map(JSValue::String)
}

pub fn js_array_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let method = obj.get_property(ctx, JS_ATOM_join)?;

    if !method.is_callable() {
        // Use intrinsic Object.prototype.toString
        js_object_toString(ctx, CallContext::new(ctx, obj, &[], 0, None))
    } else {
        method.call(ctx, &obj, &[])
    }
}

pub fn js_array_pop(ctx: &JSContext, cc: CallContext) -> JSResult {
    let shift = cc.magic();
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let mut res = JSValue::UNDEFINED;
    let mut new_len = 0;

    if len > 0 {
        new_len = len - 1;

        if let Some(r) = obj.with_fast_array_values_mut(|lst| {
            if lst.len() == len as usize {
                Some(if shift != 0 {
                    lst.remove(0)
                } else {
                    lst.pop().unwrap()
                })
            } else {
                None
            }
        }) {
            // is fast array
            res = r.unwrap_or(JSValue::UNDEFINED);
        } else {
            // slow array
            if shift != 0 {
                res = obj.get_property_i64(ctx, 0)?;
                JS_CopySubArray(ctx, &obj, 0, 1, len as usize - 1, 1)?;
            } else {
                res = obj.get_property_i64(ctx, new_len)?;
            }
            obj.delete_property_i64(ctx, new_len, JS_PROP_THROW)?;
        }
    }

    obj.set_property(ctx, JS_ATOM_length, JSValue::from(new_len))?;

    Ok(res)
}

pub fn js_array_push(ctx: &JSContext, cc: CallContext) -> JSResult {
    let unshift = cc.magic();

    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let newLen = len + cc.length() as i64;
    if newLen > MAX_SAFE_INTEGER {
        ctx.throw_type_error("Array loo long");
        return Err(-1);
    }

    let mut from = len;
    if unshift != 0 && cc.length() > 0 {
        JS_CopySubArray(ctx, &obj, cc.length() as _, 0, len as usize, -1)?;
        from = 0;
    }

    for i in 0..cc.length() {
        obj.set_property_i64(ctx, from + i as i64, cc.arg(i).clone())?;
    }

    obj.set_property(ctx, JS_ATOM_length, JSValue::from(newLen))?;

    Ok(JSValue::from(newLen))
}

pub fn js_array_reverse(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    if len <= 1 {
        return Ok(obj);
    }

    // try fast array
    let done = obj
        .with_fast_array_values_mut(|lst| {
            if lst.len() == len as usize {
                lst.reverse();
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
    if done {
        return Ok(obj); // fast array, done
    }

    // slow array
    let mut l = 0;
    let mut h = len - 1;
    let mut lval = JSValue::UNDEFINED;
    let mut hval = JSValue::UNDEFINED;

    while l < h {
        let l_present = match obj.try_get_property_i64(ctx, l)? {
            Some(v) => {
                lval = v;
                true
            }
            None => false,
        };

        let h_present = match obj.try_get_property_i64(ctx, h)? {
            Some(v) => {
                hval = v;
                true
            }
            None => false,
        };

        if h_present {
            obj.set_property_i64(ctx, l, hval.clone())?;

            if l_present {
                obj.set_property_i64(ctx, h, lval)?;
                lval = JSValue::UNDEFINED;
            } else {
                obj.delete_property_i64(ctx, h, JS_PROP_THROW)?;
            }
        } else {
            if l_present {
                obj.delete_property_i64(ctx, l, JS_PROP_THROW)?;
                obj.set_property_i64(ctx, h, lval)?;
                lval = JSValue::UNDEFINED;
            }
        }

        l += 1;
        h -= 1;
    }

    Ok(obj)
}

pub fn js_array_toReversed(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let new_len = js_get_length64(ctx, &obj)?;

    let res_arr = ctx.create_array(std::cmp::max(new_len, 0) as _)?;

    if new_len > 0 {
        res_arr
            .0
            .with_fast_array_values_mut(|dst| {
                let fast_done = obj
                    .with_fast_array_values(|src| {
                        if src.len() == new_len as usize {
                            for v in src.iter().rev() {
                                dst.push(v.clone());
                            }
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);

                let mut has_err = false;
                if !fast_done {
                    // slow array
                    // Query order is observable; test262 expects descending order.
                    for i in (0..new_len).rev() {
                        if has_err {
                            // Exception: set remaining elements to undefined.
                            dst.push(JSValue::UNDEFINED);
                        } else if let Ok(v) = obj.try_get_property_i64(ctx, i) {
                            dst.push(v.unwrap_or(JSValue::UNDEFINED));
                        } else {
                            dst.push(JSValue::UNDEFINED);
                            has_err = true;
                        }
                    }
                }

                if has_err { Err(-1) } else { Ok(()) }
            })
            .unwrap()?;

        res_arr
            .0
            .set_property(ctx, JS_ATOM_length, JSValue::from(new_len))?;
    }

    Ok(res_arr.into())
}

pub fn js_array_sort(ctx: &JSContext, _: CallContext) -> JSResult {
    ctx.throw_internal_error("sort is not implemented yet");
    Err(-1)
}

pub fn js_array_toSorted(ctx: &JSContext, _: CallContext) -> JSResult {
    ctx.throw_internal_error("toSorted is not implemented yet");
    Err(-1)
}

pub fn js_array_slice(ctx: &JSContext, cc: CallContext) -> JSResult {
    let splice = cc.magic();
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;
    let start = cc.arg(0).coerce_to_i64_clamp(ctx, 0, len, len)?;

    let item_count: u32;
    let mut del_count: i64 = 0;

    let count = if splice != 0 {
        if cc.length() == 0 {
            item_count = 0;
            del_count = 0;
        } else if cc.length() == 1 {
            item_count = 0;
            del_count = len - start;
        } else {
            item_count = (cc.length() - 2) as u32;
            del_count = cc.arg(1).coerce_to_i64_clamp(ctx, 0, len - start, 0)?;
        }
        if len + item_count as i64 - del_count > MAX_SAFE_INTEGER {
            ctx.throw_type_error("Array loo long");
            return Err(-1);
        }
        del_count
    } else {
        item_count = 0; /* avoid warning */
        let final_ = if !cc.arg(1).is_undefined() {
            cc.arg(1).coerce_to_i64_clamp(ctx, 0, len, len)?
        } else {
            len
        };
        std::cmp::max(final_ - start, 0)
    };

    let len_val = JSValue::from(count);
    let arr = JS_ArraySpeciesCreate(ctx, &obj, &len_val)?;

    let final_ = start + count;
    let mut k = start;
    let mut n = 0;

    obj.with_fast_array_values(|lst| -> Result<(), i32> {
        // in case of fast array
        // XXX: should share code with fast array constructor
        while k < final_ && k < lst.len() as i64 {
            arr.create_data_property_u32(ctx, n as _, lst[k as usize].clone(), JS_PROP_THROW)?;
            k += 1;
            n += 1;
        }
        Ok(())
    })
    .unwrap()?;

    // Copy the remaining elements if any (handle case of inherited properties)
    while k < final_ {
        if let Some(v) = obj.try_get_property_i64(ctx, k)? {
            arr.create_data_property_u32(ctx, n as _, v, JS_PROP_THROW)?;
        }
        k += 1;
        n += 1;
    }

    arr.set_property(ctx, JS_ATOM_length, JSValue::from(n))?;

    if splice != 0 {
        let new_len = len + item_count as i64 - del_count;

        if item_count as i64 != del_count {
            JS_CopySubArray(
                ctx,
                &obj,
                start as usize + item_count as usize,
                start as usize + del_count as usize,
                len as usize - (start + del_count) as usize,
                if item_count as i64 <= del_count {
                    1
                } else {
                    -1
                },
            )?;

            while k > new_len {
                k -= 1;
                obj.delete_property_i64(ctx, k, JS_PROP_THROW)?;
            }
        }

        for i in 0..item_count {
            obj.set_property_i64(ctx, start + i as i64, cc.arg(i as usize + 2).clone())?;
        }

        obj.set_property(ctx, JS_ATOM_length, JSValue::from(new_len))?;
    }

    Ok(arr)
}

pub fn js_array_toSpliced(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let start = if cc.length() > 0 {
        cc.arg(0).coerce_to_i64_clamp(ctx, 0, len, len)?
    } else {
        0
    };

    let mut del = 0;
    if cc.length() > 0 {
        del = len - start;
    }
    if cc.length() > 1 {
        del = cc.arg(1).coerce_to_i64_clamp(ctx, 0, del, 0)?;
    }

    let mut add = 0_i64;
    if cc.length() > 2 {
        add = cc.length() as i64 - 2;
    }

    let new_len = len + add - del;
    if new_len > MAX_SAFE_INTEGER {
        ctx.throw_type_error("invalid array length");
        return Err(-1);
    }

    // create result array
    let res_arr = ctx.create_array(std::cmp::max(new_len, 0) as _)?;
    if new_len <= 0 {
        return Ok(res_arr.into());
    }

    res_arr
        .0
        .with_fast_array_values_mut(|new_lst| -> Result<(), i32> {
            macro_rules! error {
                ($i:expr) => {
                    // fill rest with undefined, because array finalizer will free elements too.
                    for _ in $i..new_len {
                        new_lst.push(JSValue::UNDEFINED);
                    }
                    return Err(-1);
                };
            }

            let fast_done = obj
                .with_fast_array_values(|src| {
                    if src.len() == len as usize {
                        for v in &src[..start as usize] {
                            new_lst.push(v.clone());
                        }
                        for j in 0..add {
                            new_lst.push(cc.arg((2 + j) as usize).clone());
                        }
                        for v in &src[(start + del) as usize..] {
                            new_lst.push(v.clone());
                        }

                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);

            if !fast_done {
                // slow array
                for i in 0..start {
                    if let Ok(val) = obj.try_get_property_i64(ctx, i) {
                        new_lst.push(val.unwrap_or(JSValue::UNDEFINED));
                    } else {
                        error!(i);
                    }
                }

                for j in 0..add {
                    new_lst.push(cc.arg((2 + j) as usize).clone());
                }

                for i in (start + del)..len {
                    if let Ok(val) = obj.try_get_property_i64(ctx, i) {
                        new_lst.push(val.unwrap_or(JSValue::UNDEFINED));
                    } else {
                        error!(i);
                    }
                }
            }

            Ok(())
        })
        .unwrap()?;

    res_arr
        .0
        .set_property(ctx, JS_ATOM_length, JSValue::from(new_len))?;

    Ok(res_arr.into())
}

pub fn js_array_copyWithin(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj: JSValue = cc.this().to_object(ctx)?.into();
    let len = js_get_length64(ctx, &obj)?;

    let from: i64 = cc.arg(1).coerce_to_i64_clamp(ctx, 0, len, len)?;
    let to: i64 = cc.arg(0).coerce_to_i64_clamp(ctx, 0, len, len)?;
    let final_ = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).coerce_to_i64_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    let count = std::cmp::min(final_ - from, len - to);
    JS_CopySubArray(
        ctx,
        &obj,
        to as _,
        from as _,
        count as _,
        if from < to && to < from + count {
            -1
        } else {
            1
        },
    )?;

    Ok(obj)
}

pub fn js_array_flatten(ctx: &JSContext, cc: CallContext) -> JSResult {
    let map = cc.magic();

    let source: JSValue = cc.this().to_object(ctx)?.into();
    let source_len = js_get_length64(ctx, &source)?;

    let mut depth = 1;
    let mut mapper_func = &JSValue::UNDEFINED;
    let mut this_arg = &JSValue::UNDEFINED;

    if map != 0 {
        mapper_func = cc.arg(0);
        if cc.length() > 1 {
            this_arg = cc.arg(1);
        }
        mapper_func.ensure_callable(ctx)?;
    } else {
        if cc.length() > 0 && !cc.arg(0).is_undefined() {
            depth = cc.arg(0).to_i32_saturating(ctx)?;
        }
    }

    let res_arr = JS_ArraySpeciesCreate(ctx, &source, &JSValue::Int(0))?;

    JS_FlattenIntoArray(
        ctx,
        &res_arr,
        &source,
        source_len,
        0,
        depth as _,
        mapper_func,
        this_arg,
    )?;

    Ok(res_arr)
}

fn JS_FlattenIntoArray(
    ctx: &JSContext,
    target: &JSValue,
    source: &JSValue,
    source_len: i64,
    mut target_index: i64,
    depth: i32,
    mapper_func: &JSValue,
    this_arg: &JSValue,
) -> Result<i64, i32> {
    if js_check_stack_overflow(ctx.rt(), 0) {
        ctx.throw_stack_overflow();
        return Err(-1);
    }

    let mut sourceIndex: i64 = 0;

    while sourceIndex < source_len {
        match source.try_get_property_i64(ctx, sourceIndex)? {
            Some(mut el) => {
                if !mapper_func.is_undefined() {
                    el = mapper_func.call(
                        ctx,
                        this_arg,
                        &[el, JSValue::from(sourceIndex), source.clone()],
                    )?;
                }

                if depth > 0 {
                    if el.is_array(ctx)? {
                        let el_len = js_get_length64(ctx, &el)?;
                        let new_targetIndex = JS_FlattenIntoArray(
                            ctx,
                            target,
                            &el,
                            el_len,
                            target_index,
                            depth - 1,
                            &JSValue::UNDEFINED,
                            &JSValue::UNDEFINED,
                        )?;

                        target_index = new_targetIndex;
                        sourceIndex += 1;
                        continue;
                    }
                }

                if target_index >= MAX_SAFE_INTEGER {
                    ctx.throw_type_error("Array too long");
                    return Err(-1);
                }

                target.define_property_value_i64(
                    ctx,
                    target_index,
                    el,
                    JS_PROP_C_W_E | JS_PROP_THROW,
                )?;

                target_index += 1;
                sourceIndex += 1;
            }

            None => {
                sourceIndex += 1;
                continue;
            }
        }
    }

    Ok(target_index)
}

pub static js_array_proto_funcs: [FieldDef; 39] = [
    JS_CFUNC_NATIVE_DEF!("at", 1, js_array_at, 0),
    JS_CFUNC_NATIVE_DEF!("with", 2, js_array_with, 0),
    JS_CFUNC_NATIVE_DEF!("concat", 1, js_array_concat, 0),
    JS_CFUNC_NATIVE_DEF!("every", 1, js_array_every, special_every as i16),
    JS_CFUNC_NATIVE_DEF!("some", 1, js_array_every, special_some as i16),
    JS_CFUNC_NATIVE_DEF!("forEach", 1, js_array_every, special_forEach as i16),
    JS_CFUNC_NATIVE_DEF!("map", 1, js_array_every, special_map as i16),
    JS_CFUNC_NATIVE_DEF!("filter", 1, js_array_every, special_filter as i16),
    JS_CFUNC_NATIVE_DEF!("reduce", 1, js_array_reduce, special_reduce as i16),
    JS_CFUNC_NATIVE_DEF!(
        "reduceRight",
        1,
        js_array_reduce,
        special_reduceRight as i16
    ),
    JS_CFUNC_NATIVE_DEF!("fill", 1, js_array_fill, 0),
    JS_CFUNC_NATIVE_DEF!("find", 1, js_array_find, special_find as i16),
    JS_CFUNC_NATIVE_DEF!("findIndex", 1, js_array_find, special_findIndex as i16),
    JS_CFUNC_NATIVE_DEF!("findLast", 1, js_array_find, special_findLast as i16),
    JS_CFUNC_NATIVE_DEF!(
        "findLastIndex",
        1,
        js_array_find,
        special_findLastIndex as i16
    ),
    JS_CFUNC_NATIVE_DEF!("indexOf", 1, js_array_indexOf, 0),
    JS_CFUNC_NATIVE_DEF!("lastIndexOf", 1, js_array_lastIndexOf, 0),
    JS_CFUNC_NATIVE_DEF!("includes", 1, js_array_includes, 0),
    JS_CFUNC_NATIVE_DEF!("join", 1, js_array_join, 0),
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_array_toString, 0),
    JS_CFUNC_NATIVE_DEF!("toLocaleString", 0, js_array_join, 1),
    JS_CFUNC_NATIVE_DEF!("pop", 0, js_array_pop, 0),
    JS_CFUNC_NATIVE_DEF!("push", 1, js_array_push, 0),
    JS_CFUNC_NATIVE_DEF!("shift", 0, js_array_pop, 1),
    JS_CFUNC_NATIVE_DEF!("unshift", 1, js_array_push, 1),
    JS_CFUNC_NATIVE_DEF!("reverse", 0, js_array_reverse, 0),
    JS_CFUNC_NATIVE_DEF!("toReversed", 0, js_array_toReversed, 0),
    JS_CFUNC_NATIVE_DEF!("sort", 1, js_array_sort, 0),
    JS_CFUNC_NATIVE_DEF!("toSorted", 1, js_array_toSorted, 0),
    JS_CFUNC_NATIVE_DEF!("slice", 2, js_array_slice, 0),
    JS_CFUNC_NATIVE_DEF!("splice", 2, js_array_slice, 1),
    JS_CFUNC_NATIVE_DEF!("toSpliced", 2, js_array_toSpliced, 0),
    JS_CFUNC_NATIVE_DEF!("copyWithin", 2, js_array_copyWithin, 0),
    JS_CFUNC_NATIVE_DEF!("flatMap", 1, js_array_flatten, 1),
    JS_CFUNC_NATIVE_DEF!("flat", 0, js_array_flatten, 0),
    JS_CFUNC_NATIVE_DEF!(
        "values",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16
    ),
    JS_ALIAS_DEF!("[Symbol.iterator]", "values"),
    JS_CFUNC_NATIVE_DEF!(
        "keys",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "entries",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16
    ),
];

pub static js_array_iterator_proto_funcs: [FieldDef; 2] = [
    JS_NATIVE_ITERATOR_NEXT_DEF!("next", 0, js_array_iterator_next, 0),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "Array Iterator",
        JS_PROP_CONFIGURABLE
    ),
];

//
// static array functions
//

pub fn js_array_from(ctx: &JSContext, cc: CallContext) -> JSResult {
    // from(items, mapfn = void 0, this_arg = void 0)

    let items = cc.arg(0);
    let mut mapping = false;
    let mut mapfn = &JSValue::UNDEFINED;
    let mut this_arg = &JSValue::UNDEFINED;

    if cc.length() > 1 {
        mapfn = cc.arg(1);
        if !mapfn.is_undefined() {
            mapfn.ensure_callable(ctx)?;
            mapping = true;
            if cc.length() > 2 {
                this_arg = cc.arg(2);
            }
        }
    }

    let mut k: i64 = 0;

    let res: JSValue = if !items
        .get_property(ctx, JS_ATOM_Symbol_iterator)?
        .is_undefined()
    {
        // `items` is iterable
        let arr = if cc.this().is_constructor() {
            JS_CallConstructor(ctx, cc.this(), &[]).err()?
        } else {
            ctx.create_array(0)?.into()
        };

        let it = items.iter(ctx)?;
        for el in it {
            let mut el = el?;
            if mapping {
                el = mapfn.call(ctx, this_arg, &[el, JSValue::Int(k as i32)])?;
            }
            arr.define_property_value_i64(ctx, k, el, JS_PROP_C_W_E | JS_PROP_THROW)?;
            k += 1;
        }

        arr
    } else {
        let array_like: JSValue = items.to_object(ctx)?.into();
        let len = js_get_length64(ctx, &array_like)?;
        let arr = if cc.this().is_constructor() {
            JS_CallConstructor(ctx, cc.this(), &[JSValue::from(len)]).err()?
        } else {
            js_array_constructor(
                ctx,
                CallContext::new(ctx, JSValue::UNDEFINED, &[JSValue::from(len)], 0, None),
            )?
        };

        for i in 0..len {
            let mut el = array_like.get_property_i64(ctx, i)?;
            if mapping {
                el = mapfn.call(ctx, this_arg, &[el, JSValue::Int(i as i32)])?;
            }
            arr.define_property_value_i64(ctx, i, el, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }

        k = len;
        arr
    };

    res.set_property(ctx, JS_ATOM_length, JSValue::from(k as u32))?;
    Ok(res)
}

pub fn js_array_of(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = if cc.this().is_constructor() {
        JS_CallConstructor(ctx, cc.this(), &[JSValue::Int(cc.length() as i32)]).err()?
    } else {
        ctx.create_array(0)?.into()
    };

    for i in 0..cc.length() {
        obj.create_data_property_u32(ctx, i as _, cc.arg(i).clone(), JS_PROP_THROW)?;
    }

    obj.set_property(ctx, JS_ATOM_length, JSValue::from(cc.length() as u32))?;

    Ok(obj)
}

pub fn js_array_isArray(ctx: &JSContext, cc: CallContext) -> JSResult {
    Ok(JSValue::Bool(cc.arg(0).is_array(ctx)?))
}

pub static js_array_funcs: [FieldDef; 4] = [
    JS_CFUNC_NATIVE_DEF!("isArray", 1, js_array_isArray, 0),
    JS_CFUNC_NATIVE_DEF!("from", 1, js_array_from, 0),
    JS_CFUNC_NATIVE_DEF!("of", 0, js_array_of, 0),
    JS_CGETSET_DEF!("[Symbol.species]", js_get_this, NONE),
];
