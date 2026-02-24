// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use fallible_collections::TryCollect;
use gc_lite::GcPartitionId;

use crate::{
    JS_CLASS_ARRAY,
    atom::Atom,
    builtins::{
        iterator::{
            JSIteratorKindEnum, ObjectIter, js_array_iterator_next, js_create_array_iterator,
        },
        js_get_this,
        object::js_object_toString,
        typed_array::{js_typed_array___speciesCreate, js_typed_array_get_length_internal},
    },
    bytecode::arithmatic::StrictEqMode,
    context::{ContextImpl, ContextPtr},
    error::JSResult,
    function::{CallContext, FieldDef, JS_CallConstructor},
    number::MAX_SAFE_INTEGER,
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_THROW, Object, ObjectPayload},
    runtime::js_check_stack_overflow,
    string::{StringBuf, StringRef},
    value::JSValue,
};

use super::{Array, ArrayPayload};

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

pub(crate) fn js_array_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let new_target = *cc.this();

    let (mut arr, len) = if cc.length() == 1 && cc.arg(0).is_number() {
        // new Array(capacity) case
        let cap = cc.arg(0).to_array_length(ctx, true)?;
        let arr = Array::try_from(
            ctx.create_object_from_ctor(
                new_target,
                ObjectPayload::ARRAY(ArrayPayload::new(Vec::new())),
            )
            .map_err(|_| -1)?,
        )
        .unwrap();

        (arr, cap)
    } else {
        // new Array(el1, el2, ...) case
        let list = cc
            .arguments()
            .iter()
            .cloned()
            .try_collect::<Vec<_>>()
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        let len = list.len() as u32;

        let arr = Array::try_from(
            ctx.create_object_from_ctor(new_target, ObjectPayload::ARRAY(ArrayPayload::new(list)))
                .map_err(|_| -1)?,
        )
        .unwrap();
        (arr, len)
    };

    arr.set_length(ctx, len, true)?;
    Ok(arr.into())
}

fn JS_ArraySpeciesCreate(ctx: ContextPtr, obj: JSValue, len_val: JSValue) -> JSResult {
    if !obj.is_array(&ctx)? {
        return js_array_constructor(CallContext::new(
            ctx,
            JSValue::UNDEFINED,
            std::slice::from_ref(&len_val),
            0,
            None,
        ));
    }

    let mut ctor = obj.get_property(&ctx, Atom::CONST_constructor)?;

    if let Some(o) = ctor.as_object()
        && o.is_constructor()
    {
        // legacy web compatibility
        let realm = o.realm(ctx)?;
        if realm.id() != ctx.id() && ctor.is_same(realm.array_ctor) {
            ctor = JSValue::UNDEFINED;
        }
    }

    #[cfg(feature = "builtin_symbol")]
    if let Some(o) = ctor.as_object() {
        let species = o.get_property(&ctx, Atom::CONST_Symbol_species)?;
        ctor = if species.is_null() {
            JSValue::UNDEFINED
        } else {
            species
        };
    }

    if ctor.is_undefined() {
        js_array_constructor(CallContext::new_simple(
            ctx,
            JSValue::UNDEFINED,
            std::slice::from_ref(&len_val),
        ))
    } else {
        JS_CallConstructor(&ctx, ctor, std::slice::from_ref(&len_val)).err()
    }
}

/// check `obj[Symbol.isConcatSpreabable]` property
#[cfg(feature = "builtin_symbol")]
pub fn JS_isConcatSpreadable(ctx: &ContextImpl, obj: &JSValue) -> Result<bool, i32> {
    if obj.is_object() {
        let s = obj.get_property(ctx, Atom::CONST_Symbol_isConcatSpreadable)?;
        if !s.is_undefined() {
            s.to_bool()
        } else {
            obj.is_array(ctx)
        }
    } else {
        Ok(false)
    }
}

fn js_array_at(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let mut idx: i64 = cc.arg(0).to_i64_saturating(ctx)?;
    if idx < 0 {
        idx += len;
    }

    let ret = if (0..len).contains(&idx) {
        let mut val: Option<JSValue> = if obj.is_fast_array()
            && let Some(a) = obj.payload().array()
        {
            a.values().get(idx as usize).copied()
        } else {
            None
        };
        if val.is_none() {
            val = obj.try_get_property_i64(ctx, idx)?;
        }
        val.unwrap_or(JSValue::UNDEFINED)
    } else {
        JSValue::UNDEFINED
    };

    // LEGACY
    // let ret: JSValue = if idx < 0 || idx >= len {
    //     JSValue::UNDEFINED // bad index
    // } else if let Some(val) = obj
    //     .with_fast_array_values(|lst| lst.get(idx as usize).cloned())
    //     .flatten()
    // {
    //     // fast array element
    //     val
    // } else {
    //     // sparse array
    //     obj.try_get_property_i64(ctx, idx)?
    //         .unwrap_or(JSValue::UNDEFINED)
    // };

    Ok(ret)
}

fn js_array_with(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let mut idx: i64 = cc.arg(0).to_i64_saturating(ctx)?;
    if idx < 0 {
        idx += len;
    }
    if idx < 0 || idx >= len {
        ctx.throw_range_error("out of bound");
        return Err(-1);
    }
    if len > u32::MAX as i64 {
        ctx.throw_range_error("invalid array length");
        return Err(-1);
    }

    let new_lst = if obj.is_fast_array()
        && let Some(src) = obj.as_Array().map(|a| a.payload())
    {
        // `this` is fast array: copy list
        debug_assert_eq!(src.len(), len as usize);

        let mut dst = src
            .values()
            .iter()
            .copied()
            .try_collect::<Vec<JSValue>>()
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        // modify element
        dst[idx as usize] = *cc.arg(1);
        dst
    } else {
        // slow path
        let mut dst = Vec::<JSValue>::new();
        dst.try_reserve_exact(len as usize).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

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

        // the modified element
        dst.push(*cc.arg(1));

        // idx+1..end
        for i in (idx + 1)..len {
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
        dst
    };

    debug_assert_eq!(new_lst.len(), len as usize);

    // make Array with values
    #[cfg(debug_assertions)]
    {
        let arr = ctx.create_array_with_values(new_lst)?;
        debug_assert_eq!(arr.get_length_prop(ctx).unwrap(), len);
        Ok(arr.into())
    }
    #[cfg(not(debug_assertions))]
    ctx.create_array_with_values(new_lst).map(|a| a.into())
}

fn js_array_concat(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let this: JSValue = cc.this().objectify(ctx)?.into();
    let mut arr = JS_ArraySpeciesCreate(cc.ctx_ptr(), this, JSValue::Int(0))?;

    let mut n = 0;

    for i in -1..cc.length() as i32 {
        let v = if i < 0 { &this } else { cc.arg(i as usize) };

        let spreadable: bool = {
            #[cfg(feature = "builtin_symbol")]
            {
                JS_isConcatSpreadable(ctx, v)?
            }
            #[cfg(not(feature = "builtin_symbol"))]
            false
        };
        if spreadable {
            let len = v.get_length_prop(ctx)?;
            if n + len > MAX_SAFE_INTEGER {
                ctx.throw_type_error("Array loo long");
                return Err(-1);
            }

            for k in 0..len {
                if let Some(val) = v.try_get_property_i64(ctx, k)? {
                    arr.define_property_value_i64(ctx, n, val, JS_PROP_C_W_E | JS_PROP_THROW)?;
                }
                n += 1;
            }
        } else {
            if n >= MAX_SAFE_INTEGER {
                ctx.throw_type_error("Array loo long");
                return Err(-1);
            }

            arr.define_property_value_i64(ctx, n, *v, JS_PROP_C_W_E | JS_PROP_THROW)?;
            n += 1;
        }
    }

    arr.set_property(ctx, Atom::CONST_length, JSValue::from(n))?;
    Ok(arr)
}

pub(crate) fn js_array_every(cc: CallContext) -> JSResult {
    let special = cc.magic();
    let ctx = cc.ctx();

    let obj: JSValue;
    let len: i64;

    if special & special_TA != 0 {
        obj = *cc.this();
        len = js_typed_array_get_length_internal(ctx, obj)? as i64;
    } else {
        obj = cc.this().objectify(ctx)?.into();
        len = obj.get_length_prop(ctx)?;
    }

    let func = cc.arg(0);
    let this_arg = if cc.length() > 1 {
        *cc.arg(1)
    } else {
        JSValue::UNDEFINED
    };
    func.ensure_function(ctx)?;

    let mut ret: JSValue = match special {
        s if s == special_every || s == special_every | special_TA => JSValue::Bool(true),
        s if s == special_some || s == special_some | special_TA => JSValue::Bool(false),
        special_map => JS_ArraySpeciesCreate(cc.ctx_ptr(), obj, JSValue::from(len))?,
        special_filter => JS_ArraySpeciesCreate(cc.ctx_ptr(), obj, JSValue::Int(0))?,
        s if s == special_map | special_TA => js_typed_array___speciesCreate(
            cc.ctx_ptr(),
            JSValue::UNDEFINED,
            &[obj, JSValue::from(len as i32)],
        )?,
        s if s == special_filter | special_TA => ctx.create_array()?.into(),
        _ => JSValue::UNDEFINED,
    };

    // Note: `func.call()` statement may trigger gc collect inside,
    // we must protect allocated `ret` from being collected.
    let mut _ng = if len > 0
        && let Some(a) = ret.gc_node_ptr()
    {
        Some(ctx.rt_ref().gc_heap().protect_node(a))
    } else {
        None
    };

    let mut n = 0;
    let mut present: bool;

    for k in 0..len {
        let mut val = JSValue::UNDEFINED;

        if special & special_TA != 0 {
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
            let index_val = JSValue::from(k);

            // Note: func.call() may perform gc collect inside
            let res = func.call(ctx, this_arg, &[val, index_val, obj])?;

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
                    ret.set_property_value(ctx, JSValue::Int(k as i32), res, JS_PROP_THROW)?;
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

    if special == special_filter | special_TA {
        let arr = js_typed_array___speciesCreate(
            cc.ctx_ptr(),
            JSValue::UNDEFINED,
            &[obj, JSValue::Int(n as i32)],
        )?;

        // node `arr` will be used as `this_obj` when invoking method,
        // so that StackFrame will protect arr (this_obj) from being gc collected.
        arr.invoke(ctx, Atom::CONST_set, std::slice::from_ref(&ret))?;
        ret = arr;
    }

    Ok(ret)
}

pub(crate) fn js_array_reduce(cc: CallContext) -> JSResult {
    let special = cc.magic();
    let ctx = cc.ctx();

    let obj: JSValue;
    let len: i64;
    if special & special_TA != 0 {
        obj = *cc.this();
        len = js_typed_array_get_length_internal(ctx, obj)? as i64;
    } else {
        obj = cc.this().objectify(ctx)?.into();
        len = obj.get_length_prop(ctx)?;
    }

    let func = cc.arg(0);
    func.ensure_function(ctx)?;

    let mut acc: JSValue;
    let mut k = 0;

    if cc.length() > 1 {
        acc = *cc.arg(1);
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

            if special & special_TA != 0 {
                acc = obj.get_property_i64(ctx, k1)?;
                break;
            } else if let Some(r) = obj.try_get_property_i64(ctx, k1)? {
                acc = r;
                break;
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

        let present = if special & special_TA != 0 {
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
            let index_val = JSValue::from(k1);
            acc = func.call(ctx, JSValue::UNDEFINED, &[acc, val, index_val, obj])?;
        }
    }

    Ok(acc)
}

fn js_array_fill(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let mut obj: JSValue = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;

    let mut start = if cc.length() > 1 && !cc.arg(1).is_undefined() {
        cc.arg(1).to_i64_clamp(ctx, 0, len, len)?
    } else {
        0
    };

    let end = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_i64_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    // XXX: should special case fast arrays
    while start < end {
        obj.set_property_i64(ctx, start, *cc.arg(0))?;
        start += 1;
    }

    Ok(obj)
}

fn js_array_find(cc: CallContext) -> JSResult {
    let mode = cc.magic();
    let ctx = cc.ctx();

    let obj: JSValue = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;

    let func = cc.arg(0);
    func.ensure_function(ctx)?;

    let this_val = *cc.this();
    let this_arg = if cc.length() > 1 {
        *cc.arg(1)
    } else {
        JSValue::UNDEFINED
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
        let val = obj.get_property_value(ctx, index_val)?;
        let res = func.call(ctx, this_arg, &[val, index_val, this_val])?;

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

fn js_array_indexOf(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let this = cc.this().objectify(ctx)?;
    let len = this.get_length_prop(ctx)?;

    if len > 0 {
        let start = if cc.length() > 1 {
            cc.arg(1).to_i64_clamp(ctx, 0, len, len)?
        } else {
            0
        };

        let target = *cc.arg(0);

        // fast path: fast array
        if this.is_fast_array()
            && let Some((i, _)) = this.payload().array().and_then(|a| {
                a.values()
                    .iter()
                    .enumerate()
                    .skip(start as usize)
                    .find(|(_, v)| target.strict_eq_with_mode(**v, StrictEqMode::JS_EQ_STRICT))
            })
        {
            return Ok(JSValue::from(i as u32));
        }

        // slow path
        for i in start..len {
            if this
                .try_get_property_i64(ctx, i)?
                .is_some_and(|v| target.strict_eq_with_mode(v, StrictEqMode::JS_EQ_STRICT))
            {
                return Ok(JSValue::from(i as u32));
            }
        }
    }

    Ok(JSValue::Int(-1)) // -1 means not found
}

pub(crate) fn js_array_lastIndexOf(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let obj: JSValue = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;
    let target = cc.arg(0);

    let mut res: i64 = -1;

    if len > 0 {
        let cnt = if cc.length() > 1 {
            cc.arg(1).to_i64_clamp(ctx, -1, len - 1, len)?
        } else {
            len - 1
        };

        // TODO: should special case fast arrays
        for n in (0..=cnt).rev() {
            if let Some(v) = obj.try_get_property_i64(ctx, n)? {
                let b = target.strict_eq_with_mode(v, StrictEqMode::JS_EQ_STRICT);
                if b {
                    res = n;
                    break;
                }
            }
        }
    }

    Ok(JSValue::from(res))
}

fn js_array_includes(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let obj = cc.this().objectify(ctx)?;
    let length = obj.get_length_prop(ctx)?;
    let target = cc.arg(0);

    if length > 0 {
        let start = if cc.length() > 1 {
            cc.arg(1).to_i64_clamp(ctx, 0, length, length)?
        } else {
            0
        };

        // fast path: fast array
        if obj.is_fast_array()
            && let Some(arr_data) = obj.payload().array()
        {
            if arr_data
                .values()
                .iter()
                .skip(start as usize)
                .any(|&el| target.strict_eq_with_mode(el, StrictEqMode::JS_EQ_SAME_VALUE_ZERO))
            {
                return Ok(JSValue::Bool(true));
            } else if target.is_undefined() && arr_data.len() < length as usize {
                // fast array has less elements than prop `length` suggests includes `undefined`
                return Ok(JSValue::Bool(true));
            }
        }

        // slow path: sparse array
        for i in start..length {
            let val = obj.get_property_i64(ctx, i)?;
            if target.strict_eq_with_mode(val, StrictEqMode::JS_EQ_SAME_VALUE_ZERO) {
                return Ok(JSValue::Bool(true));
            }
        }
    }

    Ok(JSValue::Bool(false)) // not found
}

pub(crate) fn js_array_join(cc: CallContext) -> JSResult {
    let cx = cc.ctx();

    let to_locale_string = cc.magic() == 1;
    let obj = cc.this().objectify(cx)?;
    let len = obj.get_length_prop(cx)?;

    let mut c = b',' as i32; // default separator
    let mut p: Option<StringRef> = None;

    if !to_locale_string && cc.length() > 0 && !cc.arg(0).is_undefined() {
        let sep = cc.arg(0).to_string(cx)?;
        c = if let Some(a) = sep.content().ascii()
            && a.len() == 1
        {
            a[0] as i32
        } else {
            -1
        };
        p.replace(sep);
    }

    let mut sbuf = StringBuf::Empty;

    for i in 0..len {
        if i > 0 {
            // append separator
            if c >= 0 {
                let _ = sbuf.push_u8(c as u8);
            } else {
                let _ = sbuf.push_content(p.as_ref().unwrap().content());
            }
        }

        let el = obj.get_property_i64(cx, i)?;
        if !el.is_undefined_or_null() {
            let ss = if to_locale_string {
                el.to_locale_string(cx)?.to_string(cx)?
            } else if let JSValue::String(s) = el {
                s
            } else {
                el.to_string(cx)?
            };
            sbuf.push_content(ss.content()).map_err(|_| {
                cx.throw_out_of_memory();
                -1
            })?
        }
    }

    cx.create_string_with_buf(sbuf).map(JSValue::String)
}

pub(crate) fn js_array_toString(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let obj = cc.this().objectify(ctx)?;
    obj.invoke_callable(ctx, Atom::CONST_join, &[])?
        .unwrap_or_else(|| {
            // use builtin `Object.prototype.toString`
            js_object_toString(CallContext::new(cc.ctx_ptr(), obj.into(), &[], 0, None))
        })
}

pub(crate) fn js_array_pop(cc: CallContext) -> JSResult {
    let shift = cc.magic() == 1;
    let cx = cc.ctx();
    let mut this = cc.this().objectify(cx)?;

    let length = this.get_length_prop(cx)?;

    let mut res = JSValue::UNDEFINED;
    let mut new_len = 0;

    if length > 0 {
        new_len = length - 1;

        res = if this.is_fast_array()
            && let Some(arr_data) = this.payload_mut().array_mut()
        {
            // fast array
            debug_assert_eq!(arr_data.len(), length as usize);
            if shift {
                arr_data.remove(0)
            } else {
                arr_data.pop().unwrap()
            }
        } else {
            // slow path
            let val = if shift {
                let v = this.get_property_i64(cx, 0)?;
                copy_sub_array(cx, &mut this, 0, 1, length as usize - 1, 1)?;
                v
            } else {
                this.get_property_i64(cx, new_len)?
            };
            this.delete_property_i64(cx, new_len, JS_PROP_THROW)?;
            val
        };

        // LEGACY
        // if let Some(r) = obj
        //     .with_fast_array_values_mut(|lst| {
        //         if lst.len() == arr_len as usize {
        //             Some(if shift {
        //                 lst.remove(0)
        //             } else {
        //                 lst.pop().unwrap()
        //             })
        //         } else {
        //             None
        //         }
        //     })
        //     .flatten()
        // {
        //     // fast array pop/shift
        //     res = r;
        // } else {
        //     // slow path
        //     if shift {
        //         res = obj.get_property_i64(cx, 0)?;
        //         copy_sub_array(cx, &mut obj, 0, 1, arr_len as usize - 1, 1)?;
        //     } else {
        //         res = obj.get_property_i64(cx, new_len)?;
        //     }
        //     obj.delete_property_i64(cx, new_len, JS_PROP_THROW)?;
        // }
    }

    this.set_property(cx, Atom::CONST_length, JSValue::from(new_len))?;
    Ok(res)
}

pub(crate) fn js_array_push(cc: CallContext) -> JSResult {
    let unshift = cc.magic();
    let cx = cc.ctx();

    let mut obj = cc.this().objectify(cx)?;
    let len = obj.get_length_prop(cx)?;

    let new_len = len + cc.length() as i64;
    if new_len > MAX_SAFE_INTEGER {
        cx.throw_type_error("Array loo long");
        return Err(-1);
    }

    let mut from = len;
    if unshift != 0 && cc.length() > 0 {
        copy_sub_array(cx, &mut obj, cc.length() as _, 0, len as usize, -1)?;
        from = 0;
    }

    for i in 0..cc.length() {
        obj.set_property_i64(cx, from + i as i64, *cc.arg(i))?;
    }

    obj.set_property(cx, Atom::CONST_length, JSValue::from(new_len))?;

    Ok(JSValue::from(new_len))
}

fn js_array_reverse(cc: CallContext) -> JSResult {
    let cx = cc.ctx();
    let mut this = cc.this().objectify(cx)?;
    let len = this.get_length_prop(cx)?;

    if len <= 1 {
        return Ok(this.into());
    }

    // fast array
    if this.is_fast_array()
        && let Some(arr_data) = this.as_Array_mut().map(|a| a.payload_mut())
        && arr_data.len() == len as usize
    {
        arr_data.reverse();
        return Ok(this.into()); // fast path exit
    }

    // slow array
    let mut l = 0;
    let mut h = len - 1;
    let mut lval = JSValue::UNDEFINED;
    let mut hval = JSValue::UNDEFINED;

    while l < h {
        let l_present = match this.try_get_property_i64(cx, l)? {
            Some(v) => {
                lval = v;
                true
            }
            None => false,
        };

        let h_present = match this.try_get_property_i64(cx, h)? {
            Some(v) => {
                hval = v;
                true
            }
            None => false,
        };

        if h_present {
            this.set_property_i64(cx, l, hval)?;

            if l_present {
                this.set_property_i64(cx, h, lval)?;
                lval = JSValue::UNDEFINED;
            } else {
                this.delete_property_i64(cx, h, JS_PROP_THROW)?;
            }
        } else if l_present {
            this.delete_property_i64(cx, l, JS_PROP_THROW)?;
            this.set_property_i64(cx, h, lval)?;
            lval = JSValue::UNDEFINED;
        }

        l += 1;
        h -= 1;
    }

    Ok(this.into())
}

fn js_array_toReversed(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let this = cc.this().objectify(ctx)?;

    let len = {
        let n = this.get_length_prop(ctx)?;
        if n > u32::MAX as i64 {
            ctx.throw_range_error("invalid array length");
            return Err(-1);
        } else {
            n as u32
        }
    };

    // make new array values list
    let new_lst: Vec<JSValue> = if len > 0 {
        if this.is_fast_array()
            && let Some(arr_data) = this.payload().array()
        {
            // fast path
            arr_data
                .values()
                .iter()
                .rev()
                .copied()
                .try_collect::<Vec<JSValue>>()
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?
        } else {
            // slow path
            let mut lst = Vec::<JSValue>::new();
            lst.try_reserve_exact(len as usize).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            // Query order is observable; test262 expects descending order.
            for i in (0..len).rev() {
                lst.push(
                    this.try_get_property_i64(ctx, i as i64)?
                        .unwrap_or(JSValue::UNDEFINED),
                );
            }
            lst
        }
    } else {
        Vec::new()
    };

    ctx.create_array_with_values(new_lst).map(JSValue::from)
}

fn js_array_sort(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let cmpfn = {
        let f = cc.arg(0);
        if !f.is_undefined() {
            f.ensure_function(ctx)?;
            Some(f)
        } else {
            None
        }
    };
    let mut obj = cc.this().objectify(ctx)?;
    Array::sort(ctx, &mut obj, cmpfn)?;
    Ok(obj.into())
}

fn js_array_toSorted(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let cmpfn = {
        let f = cc.arg(0);
        if !f.is_undefined() {
            f.ensure_function(ctx)?;
            Some(f)
        } else {
            None
        }
    };

    let this = cc.this().objectify(ctx)?;
    let len = this.get_length_prop(ctx)?;
    if len > u32::MAX as i64 {
        ctx.throw_range_error("invalid array length");
        return Err(-1);
    }

    let new_lst: Vec<JSValue> = if len > 0 {
        if this.is_fast_array()
            && let Some(arr_data) = this.payload().array()
        {
            debug_assert_eq!(arr_data.len(), len as usize);
            arr_data
                .values()
                .iter()
                .copied()
                .try_collect::<Vec<JSValue>>()
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?
        } else {
            // slow path
            let mut lst = Vec::<JSValue>::new();
            lst.try_reserve_exact(len as usize).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            for i in 0..len {
                let v = this.try_get_property_i64(ctx, i)?;
                lst.push(v.unwrap_or(JSValue::UNDEFINED));
            }
            lst
        }
    } else {
        Vec::new()
    };

    let mut arr = ctx.create_array_with_values(new_lst)?;
    Array::sort(ctx, &mut arr, cmpfn)?;
    Ok(arr.into())
}

pub(crate) fn js_array_slice(cc: CallContext) -> JSResult {
    let is_splice = cc.magic() != 0;
    let cx = cc.ctx();

    let mut obj = cc.this().objectify(cx)?;
    let len = obj.get_length_prop(cx)?;
    let start = cc.arg(0).to_i64_clamp(cx, 0, len, len)?;

    let item_count: u32;
    let del_count: i64;

    let count = if is_splice {
        if cc.length() == 0 {
            del_count = 0;
            item_count = 0;
        } else if cc.length() == 1 {
            del_count = len - start;
            item_count = 0;
        } else {
            del_count = cc.arg(1).to_i64_clamp(cx, 0, len - start, 0)?;
            item_count = (cc.length() - 2) as u32;
        }
        if len + item_count as i64 - del_count > MAX_SAFE_INTEGER {
            cx.throw_type_error("Array loo long");
            return Err(-1);
        }
        del_count
    } else {
        item_count = 0;
        del_count = 0;
        let end = if !cc.arg(1).is_undefined() {
            cc.arg(1).to_i64_clamp(cx, 0, len, len)?
        } else {
            len
        };
        std::cmp::max(end - start, 0)
    };

    let end = start + count;

    // make return Array
    let len_val = JSValue::from(count);
    let mut res_arr = JS_ArraySpeciesCreate(cc.ctx_ptr(), obj.into(), len_val)?;

    let mut k = start;
    let mut n = 0;

    // Special case fast arrays
    if let Some(arr1) = res_arr.as_object_Array_mut()
        && arr1.is_fast_array()
        && obj.is_fast_array()
        && let Some(arr_data) = obj.payload_mut().array_mut()
    {
        let lst = arr_data
            .values()
            .iter()
            .skip(start as usize)
            .take(count as usize)
            .copied()
            .try_collect::<Vec<_>>()
            .map_err(|_| {
                cx.throw_out_of_memory();
                -1
            })?;

        if !lst.is_empty() {
            n = lst.len() as i64;
            k += n;
            arr1.set_fast_array_values(cx, lst)?;
        }
    }

    // Copy the remaining elements if any (handle case of inherited properties)
    while k < end {
        if let Some(v) = obj.try_get_property_i64(cx, k)? {
            res_arr.define_property_value_i64(cx, n, v, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }
        k += 1;
        n += 1;
    }

    res_arr.set_property(cx, Atom::CONST_length, JSValue::from(n))?;

    if is_splice {
        let new_len = len + item_count as i64 - del_count;
        if item_count as i64 != del_count {
            copy_sub_array(
                cx,
                &mut obj,
                start as usize + item_count as usize,
                start as usize + del_count as usize,
                len as usize - (start + del_count) as usize,
                if item_count as i64 <= del_count {
                    1
                } else {
                    -1
                },
            )?;

            for i in (new_len..len).rev() {
                obj.delete_property_i64(cx, i, JS_PROP_THROW)?;
            }
        }

        for i in 0..item_count {
            obj.set_property_i64(cx, start + i as i64, *cc.arg(i as usize + 2))?;
        }

        obj.set_property(cx, Atom::CONST_length, JSValue::from(new_len))?;
    }

    Ok(res_arr)
}

fn js_array_toSpliced(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let mut obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let start = if cc.length() > 0 {
        cc.arg(0).to_i64_clamp(ctx, 0, len, len)?
    } else {
        0
    };

    let mut del = 0;
    if cc.length() > 0 {
        del = len - start;
    }
    if cc.length() > 1 {
        del = cc.arg(1).to_i64_clamp(ctx, 0, del, 0)?;
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

    let mut new_lst = Vec::<JSValue>::new();
    new_lst
        .try_reserve_exact(std::cmp::max(new_len, 0) as usize)
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

    if new_len > 0 {
        if obj.is_fast_array()
            && let Some(arr_data) = obj.as_Array_mut().map(|a| a.payload_mut())
            && arr_data.len() == len as usize
        {
            // fast array
            new_lst.extend(arr_data.values().iter().take(start as usize));
            for j in 0..add {
                new_lst.push(*cc.arg((2 + j) as usize));
            }
            new_lst.extend(arr_data.values().iter().skip((start + del) as usize));
        } else {
            // slow path: sparse array
            for i in 0..start {
                obj.try_get_property_i64(ctx, i).inspect_err(|_| {
                    // error: fill rest with undefined
                    new_lst.extend(std::iter::repeat_n(
                        JSValue::UNDEFINED,
                        (new_len - i) as usize,
                    ));
                })?;
            }

            for i in 0..add {
                new_lst.push(*cc.arg((2 + i) as usize));
            }

            for i in (start + del)..len {
                obj.try_get_property_i64(ctx, i).inspect_err(|_| {
                    // error: fill rest with undefined
                    new_lst.extend(std::iter::repeat_n(
                        JSValue::UNDEFINED,
                        (new_len - i) as usize,
                    ));
                })?;
            }
        }
    }

    // create result array (will update gc xref inside)
    let res_arr = ctx.create_array_with_values(new_lst)?;
    Ok(res_arr.into())

    // LEGACY
    // // create result array
    // let mut res_arr = ctx.create_array_with_capacity(std::cmp::max(new_len, 0) as usize)?;
    // if new_len <= 0 {
    //     return Ok(res_arr.into());
    // }

    // res_arr
    //     .with_fast_array_values_mut(|new_lst| -> Result<(), i32> {
    //         macro_rules! error {
    //             ($i:expr) => {
    //                 // fill rest with undefined, because array finalize will free elements too.
    //                 for _ in $i..new_len {
    //                     new_lst.push(JSValue::UNDEFINED);
    //                 }
    //                 return Err(-1);
    //             };
    //         }

    //         // fast path
    //         if !obj
    //             .with_fast_array_values(|lst| {
    //                 if lst.len() == len as usize {
    //                     for v in &lst[..start as usize] {
    //                         new_lst.push(*v);
    //                     }
    //                     for j in 0..add {
    //                         new_lst.push(*cc.arg((2 + j) as usize));
    //                     }
    //                     for v in &lst[(start + del) as usize..] {
    //                         new_lst.push(*v);
    //                     }

    //                     true
    //                 } else {
    //                     false
    //                 }
    //             })
    //             .unwrap_or(false)
    //         {
    //             // slow path: sparse array
    //             for i in 0..start {
    //                 if let Ok(val) = obj.try_get_property_i64(ctx, i) {
    //                     new_lst.push(val.unwrap_or(JSValue::UNDEFINED));
    //                 } else {
    //                     error!(i);
    //                 }
    //             }

    //             for i in 0..add {
    //                 new_lst.push(*cc.arg((2 + i) as usize));
    //             }
    //             for i in (start + del)..len {
    //                 if let Ok(val) = obj.try_get_property_i64(ctx, i) {
    //                     new_lst.push(val.unwrap_or(JSValue::UNDEFINED));
    //                 } else {
    //                     error!(i);
    //                 }
    //             }
    //         }

    //         Ok(())
    //     })
    //     .unwrap()?;

    // res_arr.set_property(ctx, Atom::CONST_length, JSValue::from(new_len))?;
    // Ok(res_arr.into())
}

fn js_array_copyWithin(cc: CallContext) -> JSResult {
    let cx = cc.ctx();

    let mut obj = cc.this().objectify(cx)?;
    let len = obj.get_length_prop(cx)?;

    let to: i64 = cc.arg(0).to_i64_clamp(cx, 0, len, len)?;
    let start: i64 = cc.arg(1).to_i64_clamp(cx, 0, len, len)?;
    let end = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_i64_clamp(cx, 0, len, len)?
    } else {
        len
    };

    let count = std::cmp::min(end - start, len - to);
    if count > 0 {
        copy_sub_array(
            cx,
            &mut obj,
            to as _,
            start as _,
            count as _,
            if start < to && to < start + count {
                -1
            } else {
                1
            },
        )?;
    }

    Ok(obj.into())
}

pub(crate) fn js_array_flatten(cc: CallContext) -> JSResult {
    let map = cc.magic();
    let ctx = cc.ctx();

    let source: JSValue = cc.this().objectify(ctx)?.into();
    let source_len = source.get_length_prop(ctx)?;

    let mut depth = 1;
    let mut mapper_func = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if map != 0 {
        mapper_func = *cc.arg(0);
        if cc.length() > 1 {
            this_arg = *cc.arg(1);
        }
        mapper_func.ensure_function(ctx)?;
    } else if cc.length() > 0 && !cc.arg(0).is_undefined() {
        depth = cc.arg(0).to_i32_saturating(ctx)?;
    }

    let arr = JS_ArraySpeciesCreate(cc.ctx_ptr(), source, JSValue::Int(0))?;

    JS_FlattenIntoArray(
        ctx,
        arr,
        source,
        source_len,
        0,
        depth as _,
        mapper_func,
        this_arg,
    )?;

    Ok(arr)
}

#[allow(clippy::too_many_arguments)]
fn JS_FlattenIntoArray(
    ctx: &ContextImpl,
    mut target: JSValue,
    source: JSValue,
    source_len: i64,
    mut target_index: i64,
    depth: i32,
    mapper_func: JSValue,
    this_arg: JSValue,
) -> Result<i64, i32> {
    if js_check_stack_overflow(ctx.rt_ref(), 0) {
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
                        &[el, JSValue::from(sourceIndex), source],
                    )?;
                }

                if depth > 0 && el.is_array(ctx)? {
                    let el_len = el.get_length_prop(ctx)?;
                    let new_targetIndex = JS_FlattenIntoArray(
                        ctx,
                        target,
                        el,
                        el_len,
                        target_index,
                        depth - 1,
                        JSValue::UNDEFINED,
                        JSValue::UNDEFINED,
                    )?;

                    target_index = new_targetIndex;
                    sourceIndex += 1;
                    continue;
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

pub const js_array_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("at", 1, js_array_at, 0),
    FieldDef::CALL("with", 2, js_array_with, 0),
    FieldDef::CALL("concat", 1, js_array_concat, 0),
    FieldDef::CALL("every", 1, js_array_every, special_every as i16),
    FieldDef::CALL("some", 1, js_array_every, special_some as i16),
    FieldDef::CALL("forEach", 1, js_array_every, special_forEach as i16),
    FieldDef::CALL("map", 1, js_array_every, special_map as i16),
    FieldDef::CALL("filter", 1, js_array_every, special_filter as i16),
    FieldDef::CALL("reduce", 1, js_array_reduce, special_reduce as i16),
    FieldDef::CALL(
        "reduceRight",
        1,
        js_array_reduce,
        special_reduceRight as i16,
    ),
    FieldDef::CALL("fill", 1, js_array_fill, 0),
    FieldDef::CALL("find", 1, js_array_find, special_find as i16),
    FieldDef::CALL("findIndex", 1, js_array_find, special_findIndex as i16),
    FieldDef::CALL("findLast", 1, js_array_find, special_findLast as i16),
    FieldDef::CALL(
        "findLastIndex",
        1,
        js_array_find,
        special_findLastIndex as i16,
    ),
    FieldDef::CALL("indexOf", 1, js_array_indexOf, 0),
    FieldDef::CALL("lastIndexOf", 1, js_array_lastIndexOf, 0),
    FieldDef::CALL("includes", 1, js_array_includes, 0),
    FieldDef::CALL("join", 1, js_array_join, 0),
    FieldDef::CALL("toString", 0, js_array_toString, 0),
    FieldDef::CALL("toLocaleString", 0, js_array_join, 1),
    FieldDef::CALL("pop", 0, js_array_pop, 0),
    FieldDef::CALL("push", 1, js_array_push, 0),
    FieldDef::CALL("shift", 0, js_array_pop, 1),
    FieldDef::CALL("unshift", 1, js_array_push, 1),
    FieldDef::CALL("reverse", 0, js_array_reverse, 0),
    FieldDef::CALL("toReversed", 0, js_array_toReversed, 0),
    FieldDef::CALL("sort", 1, js_array_sort, 0),
    FieldDef::CALL("toSorted", 1, js_array_toSorted, 0),
    FieldDef::CALL("slice", 2, js_array_slice, 0),
    FieldDef::CALL("splice", 2, js_array_slice, 1),
    FieldDef::CALL("toSpliced", 2, js_array_toSpliced, 0),
    FieldDef::CALL("copyWithin", 2, js_array_copyWithin, 0),
    FieldDef::CALL("flatMap", 1, js_array_flatten, 1),
    FieldDef::CALL("flat", 0, js_array_flatten, 0),
    FieldDef::CALL(
        "values",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::ALIAS("[Symbol.iterator]", "values"),
    FieldDef::CALL(
        "keys",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16,
    ),
    FieldDef::CALL(
        "entries",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16,
    ),
];

pub const js_array_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 0, js_array_iterator_next, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Array Iterator",
        JS_PROP_CONFIGURABLE,
    ),
];

//
// static array functions
//

fn js_array_from(cc: CallContext) -> JSResult {
    // from(items, mapfn = void 0, this_arg = void 0)
    let ctx = cc.ctx();
    let items = cc.arg(0);
    let mut mapping = false;
    let mut mapfn = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if cc.length() > 1 {
        mapfn = *cc.arg(1);
        if !mapfn.is_undefined() {
            mapfn.ensure_function(ctx)?;
            mapping = true;
            if cc.length() > 2 {
                this_arg = *cc.arg(2);
            }
        }
    }

    let mut k: i64 = 0;

    let mut res = JSValue::UNDEFINED;

    // check and use [Symbol.iterator] method
    let has_iterator: bool = {
        #[cfg(feature = "builtin_symbol")]
        {
            let it = items.get_property(ctx, Atom::CONST_Symbol_iterator)?;
            let has = !it.is_undefined();

            if has {
                // `items` has [Symbol.iterator] method
                let mut arr = if cc.this().is_constructor() {
                    JS_CallConstructor(ctx, *cc.this(), &[]).err()?
                } else {
                    ctx.create_array()?.into()
                };

                let iter = items.get_iterator_must(ctx, false)?;
                let it = ObjectIter::new(ctx, iter, Atom::CONST_next)?;

                for el in it {
                    let mut val = el?;
                    if mapping {
                        val = mapfn.call(ctx, this_arg, &[val, JSValue::Int(k as i32)])?;
                    }
                    arr.define_property_value_i64(ctx, k, val, JS_PROP_C_W_E | JS_PROP_THROW)?;
                    k += 1;
                }

                res = arr;
            }
            has
        }

        #[cfg(not(feature = "builtin_symbol"))]
        false
    };

    if !has_iterator {
        let src = items.objectify(ctx)?;
        let len = src.get_length_prop(ctx)?;
        let len_val = JSValue::from(len);

        let mut arr = if cc.this().is_constructor() {
            JS_CallConstructor(ctx, *cc.this(), &[len_val]).err()?
        } else {
            ctx.create_array_with_capacity(len as usize)?.into()
        };

        for i in 0..len {
            let mut el = src.get_property_i64(ctx, i)?;
            if mapping {
                el = mapfn.call(ctx, this_arg, &[el, JSValue::Int(i as i32)])?;
            }
            arr.define_property_value_i64(ctx, i, el, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }

        k = len;
        res = arr;
    }

    // let res: JSValue = if has_iterator {
    //     // `items` has [Symbo.iterator] method
    //     let mut arr = if cc.this().is_constructor() {
    //         JS_CallConstructor(ctx, *cc.this(), &[]).err()?
    //     } else {
    //         ctx.create_array()?.into()
    //     };

    //     let iter = items.get_iterator_must(ctx, false)?;
    //     let it = ObjectIter::new(ctx, iter, Atom::CONST_next)?;

    //     for el in it {
    //         let mut val = el?;
    //         if mapping {
    //             val = mapfn.call(ctx, this_arg, &[val, JSValue::Int(k as i32)])?;
    //         }
    //         arr.define_property_value_i64(ctx, k, val, JS_PROP_C_W_E | JS_PROP_THROW)?;
    //         k += 1;
    //     }

    //     arr
    // } else {
    //     let src = items.objectify(ctx)?;
    //     let len = src.get_length_prop(ctx)?;
    //     let len_val = JSValue::from(len);

    //     let mut arr = if cc.this().is_constructor() {
    //         JS_CallConstructor(ctx, *cc.this(), &[len_val]).err()?
    //     } else {
    //         ctx.create_array_with_capacity(len as usize)?.into()
    //     };

    //     for i in 0..len {
    //         let mut el = src.get_property_i64(ctx, i)?;
    //         if mapping {
    //             el = mapfn.call(ctx, this_arg, &[el, JSValue::Int(i as i32)])?;
    //         }
    //         arr.define_property_value_i64(ctx, i, el, JS_PROP_C_W_E | JS_PROP_THROW)?;
    //     }

    //     k = len;
    //     arr
    // };

    res.set_property(ctx, Atom::CONST_length, JSValue::from(k as u32))?;
    Ok(res)
}

fn js_array_of(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let mut arr = if cc.this().is_constructor() {
        JS_CallConstructor(ctx, *cc.this(), &[JSValue::Int(cc.length() as i32)]).err()?
    } else {
        ctx.create_array()?.into()
    };

    for i in 0..cc.length() {
        arr.define_property_value_u32(ctx, i as _, *cc.arg(i), JS_PROP_THROW)?;
    }

    arr.set_property(ctx, Atom::CONST_length, JSValue::from(cc.length() as u32))?;
    Ok(arr)
}

fn js_array_isArray(cc: CallContext) -> JSResult {
    Ok(JSValue::Bool(cc.arg(0).is_array(cc.ctx())?))
}

pub const js_array_funcs: &[FieldDef] = &[
    FieldDef::CALL("isArray", 1, js_array_isArray, 0),
    FieldDef::CALL("from", 1, js_array_from, 0),
    FieldDef::CALL("of", 0, js_array_of, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

/// 将数组 obj 中从 start_pos 开始的 count 个元素复制到 to_pos 开始的位置。
/// - 正向复制（dir > 0）：从 start_pos 到 start_pos + count - 1 的元素依次复制到 to_pos 到 to_pos + count - 1。
/// - 反向复制（dir < 0）：从 start_pos + count - 1 到 start_pos 的元素依次复制到 to_pos + count - 1 到 to_pos。
fn copy_sub_array(
    ctx: &ContextImpl,
    obj: &mut Object,
    to_pos: usize,
    start_pos: usize,
    count: usize,
    dir: i32,
) -> Result<(), i32> {
    if count == 0 {
        return Ok(());
    }

    let mut start_pos = start_pos;
    let mut to_pos = to_pos;
    let mut count = count;

    // not for class ARGUMENTS
    if obj.is_fast_array() && obj.class_id() == JS_CLASS_ARRAY {
        let arr_data = obj.payload_mut().array_mut().unwrap();
        let len = arr_data.len();
        let mut done_count = count;

        for i in 0..count {
            let (from, to) = if dir < 0 {
                (start_pos + count - i - 1, to_pos + count - i - 1)
            } else {
                (start_pos + i, to_pos + i)
            };

            if from < len && to < len {
                let val = arr_data.values()[from];
                arr_data.set_nth(
                    &mut ctx.rt_ptr(),
                    to,
                    val,
                    val.gc_node().map_or(GcPartitionId::NONE, |x| x.scope_id()),
                );
            } else {
                done_count = i;
                break;
            }
        }

        // LEGACY
        // let done_count = obj
        //     .with_fast_array_values_mut(|lst| {
        //         let len = lst.len();
        //         for i in 0..count {
        //             let (from, to) = if dir < 0 {
        //                 (start_pos + count - i - 1, to_pos + count - i - 1)
        //             } else {
        //                 (start_pos + i, to_pos + i)
        //             };

        //             if from < len && to < len {
        //                 lst[to] = lst[from];
        //             } else {
        //                 return i;
        //             }
        //         }

        //         count
        //     })
        //     .unwrap();

        if done_count == count {
            return Ok(()); // early exit for fast path
        }

        // leave some parts to slow path
        debug_assert!(done_count < count);
        count -= done_count;
        start_pos += done_count;
        to_pos += done_count;
    }

    // slow path, may destruct fast array to sparse array
    for i in 0..count {
        let (from, to) = if dir < 0 {
            (start_pos + count - i - 1, to_pos + count - i - 1)
        } else {
            (start_pos + i, to_pos + i)
        };

        if let Some(v) = obj.try_get_property_i64(ctx, from as _)? {
            obj.set_property_i64(ctx, to as _, v)?;
        } else {
            obj.delete_property_i64(ctx, to as _, JS_PROP_THROW)?;
        }
    }

    Ok(())
}
