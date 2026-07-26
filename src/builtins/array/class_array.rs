// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use fallible_collections::TryCollect;
use gc_lite::GcNode;

use crate::{
    JS_CLASS_ARRAY,
    array::{Array, ArrayPayload, alloc_fast_array_slots},
    atom::{Atom, JS_ATOM_MAX_INT},
    builtins::{
        iterator::{
            JSIteratorKindEnum, ObjectIter, js_array_iterator_next, js_create_array_iterator,
        },
        js_get_this,
        object::js_object_toString,
        typed_array::{js_typed_array___speciesCreate, js_typed_array_get_length_internal},
    },
    bytecode::arithmatic::StrictEqMode,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{CallContext, FieldDef},
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_STR_OR_INT_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_THROW, Object,
        ObjectPayload,
    },
    runtime::js_check_stack_overflow,
    string::{StringBuf, StringRef},
    value::JSValue,
};

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

pub(crate) fn js_array_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let (mut arr, len) = if cc.length() == 1 && cc.arg(0).is_number() {
        // new Array(capacity) case
        let cap = cc.arg(0).to_array_length(ctx, true)?;
        let arr = Array::try_from(
            ctx.create_instance_with_ctor(
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
            ctx.create_instance_with_ctor(
                new_target,
                ObjectPayload::ARRAY(ArrayPayload::new(list)),
            )
            .map_err(|_| -1)?,
        )
        .unwrap();
        (arr, len)
    };

    arr.set_length(ctx, len, true)?;
    Ok(arr.into())
}

fn JS_ArraySpeciesCreate(ctx: &mut ScopedContext, obj: JSValue, len_val: JSValue) -> JSResult {
    if !obj.is_array(ctx)? {
        let cc = CallContext::new(JSValue::UNDEFINED, std::slice::from_ref(&len_val), 0, None);
        return js_array_constructor(ctx, cc);
    }

    let mut ctor = obj.get_property(ctx, Atom::CONST_constructor)?;

    if let Some(o) = ctor.as_object()
        && o.is_constructor()
    {
        // legacy web compatibility
        let realm = o.realm(ctx.into())?;
        if realm != ContextPtr::from(ctx.gc_ref()) && ctor.is_same(realm.array_ctor().into()) {
            ctor = JSValue::UNDEFINED;
        }
    }

    if let Some(o) = ctor.as_object() {
        let species = o.get_property(ctx, Atom::CONST_Symbol_species)?;
        ctor = if species.is_null() {
            JSValue::UNDEFINED
        } else {
            species
        };
    }

    if ctor.is_undefined() {
        js_array_constructor(
            ctx,
            CallContext::new_simple(JSValue::UNDEFINED, std::slice::from_ref(&len_val)),
        )
    } else {
        ctor.call_ctor(ctx, std::slice::from_ref(&len_val))
    }
}

/// check `obj[Symbol.isConcatSpreabable]` property
pub fn JS_isConcatSpreadable(ctx: &mut ScopedContext, obj: JSValue) -> Result<bool, i32> {
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

fn js_array_at(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let mut idx: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
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

    Ok(ret)
}

fn js_array_with(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let mut idx: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
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
        dst[idx as usize] = cc.arg(1);
        dst
    } else {
        // slow path
        let mut dst = alloc_fast_array_slots(ctx, len as _)?;

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
        dst.push(cc.arg(1));

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
        let arr = Array::from_values(ctx, new_lst)?;
        debug_assert_eq!(arr.get_length_prop(ctx).unwrap(), len);
        Ok(arr.into())
    }
    #[cfg(not(debug_assertions))]
    Array::from_values(ctx, new_lst).map(|a| a.into())
}

fn js_array_concat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this: JSValue = cc.this().objectify(ctx)?.into();
    let mut arr = JS_ArraySpeciesCreate(ctx, this, JSValue::from(0))?;

    let mut n = 0;

    for i in -1..cc.length() as i32 {
        let v = if i < 0 { this } else { cc.arg(i as usize) };

        if JS_isConcatSpreadable(ctx, v)? {
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

            arr.define_property_value_i64(ctx, n as _, v, JS_PROP_C_W_E | JS_PROP_THROW)?;
            n += 1;
        }
    }

    arr.set_property(ctx, Atom::CONST_length, JSValue::from(n))?;
    Ok(arr)
}

pub(crate) fn js_array_every(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let special = cc.magic();

    let obj: JSValue;
    let len: i64;

    if special & special_TA != 0 {
        obj = cc.this();
        len = js_typed_array_get_length_internal(ctx, obj)? as i64;
    } else {
        obj = cc.this().objectify(ctx)?.into();
        len = obj.get_length_prop(ctx)?;
    }

    let func = cc.arg(0);
    let this_arg = if cc.length() > 1 {
        cc.arg(1)
    } else {
        JSValue::UNDEFINED
    };
    func.ensure_callable(ctx)?;

    let mut ret: JSValue = match special {
        s if s == special_every || s == special_every | special_TA => JSValue::from(true),
        s if s == special_some || s == special_some | special_TA => JSValue::from(false),
        special_map => JS_ArraySpeciesCreate(ctx, obj, JSValue::from(len))?,
        special_filter => JS_ArraySpeciesCreate(ctx, obj, JSValue::from(0))?,
        s if s == special_map | special_TA => js_typed_array___speciesCreate(
            ctx,
            JSValue::UNDEFINED,
            &[obj, JSValue::from(len as i32)],
        )?,
        s if s == special_filter | special_TA => Array::create(ctx)?.into(),
        _ => JSValue::UNDEFINED,
    };

    // protect ret before func.call()
    // if len > 0
    //     && let Some(n) = ret.gc_node_ptr()
    // {
    //     ctx.gc_heap_mut().protect_node_v2(n);
    // }

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
                        ret = JSValue::FALSE;
                        break;
                    }
                }
                s if (s == special_some || s == special_some | special_TA)
                    && res.to_bool().unwrap_or(true) =>
                {
                    ret = JSValue::TRUE;
                    break;
                }
                special_map => {
                    ret.define_property_value_i64(ctx, k, res, JS_PROP_C_W_E | JS_PROP_THROW)?;
                }
                s if s == special_map | special_TA => {
                    ret.set_property_value(ctx, JSValue::from(k as i32), res, JS_PROP_THROW)?;
                }
                s if (s == special_filter || s == special_filter | special_TA)
                    && res.to_bool().unwrap_or(true) =>
                {
                    ret.define_property_value_i64(ctx, n, val, JS_PROP_C_W_E | JS_PROP_THROW)?;
                    n += 1;
                }
                _ => {}
            }
        }
    }

    if special == special_filter | special_TA {
        let arr = js_typed_array___speciesCreate(
            ctx,
            JSValue::UNDEFINED,
            &[obj, JSValue::from(n as i32)],
        )?;

        // node `arr` will be used as `this_obj` when invoking method,
        // so that StackFrame will protect arr (this_obj) from being gc collected.
        arr.invoke(ctx, Atom::CONST_set, std::slice::from_ref(&ret))?;
        ret = arr;
    }

    Ok(ret)
}

pub(crate) fn js_array_reduce(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let special = cc.magic();

    let this = cc.this();
    let this_obj: Object;
    let len: i64;

    if special & special_TA != 0 {
        this_obj = this.as_object().ok_or_else(|| {
            ctx.throw_type_error_not_an_object();
            -1
        })?;
        len = js_typed_array_get_length_internal(ctx, this)? as i64;
    } else {
        this_obj = this.objectify(ctx)?;
        len = this_obj.get_length_prop(ctx)?;
    }

    let func = cc.arg(0);
    func.ensure_callable(ctx)?;

    let mut acc: JSValue;
    if special & special_TA != 0 {
        // typed arrays are dense: `len` is the exact element count, so iterate
        // the full range directly.
        let mut k = 0;
        if cc.length() > 1 {
            acc = cc.arg(1);
        } else {
            if len == 0 {
                ctx.throw_type_error("empty array");
                return Err(-1);
            }
            let k1 = if special & special_reduceRight != 0 {
                len - 1
            } else {
                0
            };
            acc = this_obj.get_property_i64(ctx, k1)?;
            k = 1;
        }

        let mut val;
        while k < len {
            let k1 = if special & special_reduceRight != 0 {
                len - k - 1
            } else {
                k
            };
            k += 1;

            val = this_obj.get_property_i64(ctx, k1)?;
            let index_val = JSValue::from(k1);
            acc = func.call(ctx, JSValue::UNDEFINED, &[acc, val, index_val, this])?;
        }

        return Ok(acc);
    }

    if len <= JS_ATOM_MAX_INT as i64 {
        // Sequential scan. Every index is a small int, so `try_get_property_i64`
        // takes the allocation-free fast path; this also matches the spec's
        // sequential semantics (e.g. getters that add properties while visited).
        let mut k = 0;
        if cc.length() > 1 {
            acc = cc.arg(1);
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
                if let Some(r) = this_obj.try_get_property_i64(ctx, k1)? {
                    acc = r;
                    break;
                }
            }
        }
        while k < len {
            let k1 = if special & special_reduceRight != 0 {
                len - k - 1
            } else {
                k
            };
            k += 1;
            if let Some(v) = this_obj.try_get_property_i64(ctx, k1)? {
                let index_val = JSValue::from(k1);
                acc = func.call(ctx, JSValue::UNDEFINED, &[acc, v, index_val, this])?;
            }
        }
        return Ok(acc);
    }

    // Huge length: scanning `0..len` would allocate an atom per index and
    // exhaust the atom table. Enumerate only the actually-present integer
    // indices (own + inherited) instead. `reduce` already skips holes per
    // spec, so this is exactly equivalent.
    let mut idxs = collect_present_indices(ctx, this_obj, len)?;
    if special & special_reduceRight != 0 {
        idxs.reverse();
    }

    let mut it = idxs.into_iter();
    if cc.length() > 1 {
        acc = cc.arg(1);
    } else {
        match it.next() {
            Some(k1) => {
                acc = this_obj
                    .try_get_property_i64(ctx, k1)?
                    .unwrap_or(JSValue::UNDEFINED);
            }
            None => {
                ctx.throw_type_error("empty array");
                return Err(-1);
            }
        }
    }

    let mut val;
    for k1 in it {
        val = this_obj
            .try_get_property_i64(ctx, k1)?
            .unwrap_or(JSValue::UNDEFINED);
        let index_val = JSValue::from(k1);
        acc = func.call(ctx, JSValue::UNDEFINED, &[acc, val, index_val, this])?;
    }

    Ok(acc)
}

fn js_array_fill(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj: JSValue = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;

    let mut start = if cc.length() > 1 && !cc.arg(1).is_undefined() {
        cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len, len)
    } else {
        0
    };

    let end = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_number(ctx)?.to_i64_clamp(0, len, len)
    } else {
        len
    };

    // XXX: should special case fast arrays
    while start < end {
        obj.set_property_i64(ctx, start, cc.arg(0))?;
        start += 1;
    }

    Ok(obj)
}

fn js_array_find(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mode = cc.magic();

    let obj: JSValue = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;

    let func = cc.arg(0);
    func.ensure_callable(ctx)?;

    let this_val = cc.this();
    let this_arg = if cc.length() > 1 {
        cc.arg(1)
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
            JSValue::from(-1)
        } else {
            JSValue::UNDEFINED
        },
    )
}

/// Collect the integer indices that are actually present on `obj`, including
/// those inherited through the prototype chain, bounded to `0..len`.
///
/// `indexOf`/`lastIndexOf`/`reduce` must consider inherited integer-indexed
/// properties (generic array-like receivers such as `Boolean`/`String` objects
/// or custom prototypes). But scanning the full `0..len` range for a huge sparse
/// array (length near 2^53) would allocate an atom per index and exhaust the
/// atom table. Enumerating only the present integer keys stays cheap (their
/// count is tiny) while remaining semantically equivalent to the spec's
/// `HasProperty`/`Get` walk, and also honors inherited elements.
fn collect_present_indices(
    ctx: &mut ScopedContext,
    obj: Object,
    len: i64,
) -> Result<Vec<i64>, i32> {
    let mut idxs: Vec<i64> = Vec::new();
    let mut current: Option<Object> = Some(obj);
    let mut depth: u32 = 0;

    while let Some(mut o) = current {
        // guard against pathological / cyclic prototype chains
        if depth > 128 {
            break;
        }
        depth += 1;

        let names = o.get_own_property_names(ctx, JS_GPN_STR_OR_INT_MASK)?;
        for pk in names.iter() {
            // `to_i64` resolves both genuine integer keys and canonical
            // integer-string keys (e.g. generic array-likes that store elements
            // as string properties on the prototype, or indices above
            // JS_ATOM_MAX_INT stored as string atoms), so inherited elements and
            // large-but-valid indices are honored while non-index keys
            // (`length`, `foo`, symbols) are skipped.
            if let Some(idx) = pk.atom.to_i64(ctx.rt())
                && idx >= 0
                && idx < len
            {
                idxs.push(idx);
            }
        }

        current = o.prototype(ctx)?;
    }

    idxs.sort_unstable();
    idxs.dedup();
    Ok(idxs)
}

fn js_array_indexOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc.this().objectify(ctx)?;
    let len = this_obj.get_length_prop(ctx)?;

    if len > 0 {
        let start = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len, len)
        } else {
            0
        };

        let target = cc.arg(0);

        // fast path: fast array
        if this_obj.is_fast_array()
            && let Some((i, _)) = this_obj.payload().array().and_then(|a| {
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
        if len <= JS_ATOM_MAX_INT as i64 {
            // Sequential scan. Every index here is a small int, so
            // `try_get_property_i64` takes the allocation-free fast path; this
            // also matches the spec's sequential semantics (e.g. getters that
            // add properties while being visited).
            for i in start..len {
                if this_obj
                    .try_get_property_i64(ctx, i)?
                    .is_some_and(|v| target.strict_eq_with_mode(v, StrictEqMode::JS_EQ_STRICT))
                {
                    return Ok(JSValue::from(i));
                }
            }
        } else {
            // Huge length: scanning `start..len` would allocate an atom per
            // index (past JS_ATOM_MAX_INT) and exhaust the atom table. Enumerate
            // only the actually-present integer indices (own + inherited)
            // instead; this is equivalent for every non-undefined search target.
            let idxs = collect_present_indices(ctx, this_obj, len)?;
            for i in idxs {
                if i < start {
                    continue;
                }
                if let Some(v) = this_obj.try_get_property_i64(ctx, i)?
                    && target.strict_eq_with_mode(v, StrictEqMode::JS_EQ_STRICT)
                {
                    return Ok(JSValue::from(i));
                }
            }
        }
    }

    Ok(JSValue::from(-1)) // -1 means not found
}

pub(crate) fn js_array_lastIndexOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj: JSValue = cc.this().objectify(ctx)?.into();
    let len = this_obj.get_length_prop(ctx)?;
    let target = cc.arg(0);

    let mut res: i64 = -1;

    if len > 0 {
        let cnt = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i64_clamp(-1, len - 1, len)
        } else {
            len - 1
        };

        if cnt <= JS_ATOM_MAX_INT as i64 {
            // Sequential scan (descending). Every index is a small int, so no
            // atom is allocated; matches the spec's sequential semantics.
            for n in (0..=cnt).rev() {
                if let Some(v) = this_obj.try_get_property_i64(ctx, n)?
                    && target.strict_eq_with_mode(v, StrictEqMode::JS_EQ_STRICT)
                {
                    res = n;
                    break;
                }
            }
        } else {
            // Huge length: scanning `0..=cnt` would allocate an atom per index
            // and exhaust the atom table. Enumerate only the actually-present
            // integer indices (own + inherited) instead, in descending order.
            let o = this_obj.as_object().unwrap();
            let idxs = collect_present_indices(ctx, o, len)?;
            for n in idxs.iter().rev() {
                if *n > cnt {
                    continue;
                }
                if let Some(v) = this_obj.try_get_property_i64(ctx, *n)?
                    && target.strict_eq_with_mode(v, StrictEqMode::JS_EQ_STRICT)
                {
                    res = *n;
                    break;
                }
            }
        }
    }

    Ok(JSValue::from(res))
}

fn js_array_includes(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let length = obj.get_length_prop(ctx)?;
    let target = cc.arg(0);

    if length > 0 {
        let start = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i64_clamp(0, length, length)
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
                return Ok(JSValue::TRUE);
            } else if target.is_undefined() && arr_data.len() < length as usize {
                // fast array has less elements than prop `length` suggests includes `undefined`
                return Ok(JSValue::TRUE);
            }
        }

        // slow path: sparse array
        for i in start..length {
            let val = obj.get_property_i64(ctx, i)?;
            if target.strict_eq_with_mode(val, StrictEqMode::JS_EQ_SAME_VALUE_ZERO) {
                return Ok(JSValue::TRUE);
            }
        }
    }

    Ok(JSValue::FALSE) // not found
}

pub(crate) fn js_array_join(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let to_locale_string = cc.magic() == 1;
    let this_obj = cc.this().objectify(ctx)?;
    let len = this_obj.get_length_prop(ctx)?;

    let mut c = b',' as i32; // default separator
    let mut p: Option<StringRef> = None;

    if !to_locale_string && cc.length() > 0 && !cc.arg(0).is_undefined() {
        let sep = cc.arg(0).to_string(ctx)?;
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

        let el = this_obj.get_property_i64(ctx, i)?;
        if !el.is_undefined_or_null() {
            let ss: StringRef = if to_locale_string {
                el.to_locale_string(ctx)?.to_string(ctx)?
            } else if let Some(s) = el.as_string(true) {
                s
            } else {
                el.to_string(ctx)?
            };
            sbuf.push_content(ss.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?
        }
    }

    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

pub(crate) fn js_array_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc.this().objectify(ctx)?;
    this_obj
        .invoke_callable(ctx, Atom::CONST_join, &[])?
        .unwrap_or_else(|| {
            // use builtin `Object.prototype.toString`
            js_object_toString(ctx, CallContext::new_simple(this_obj.into(), &[]))
        })
}

pub(crate) fn js_array_pop(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let shift = cc.magic() == 1;

    let mut this_obj = cc.this().objectify(ctx)?;
    let length = this_obj.get_length_prop(ctx)?;
    let mut res = JSValue::UNDEFINED;
    let mut new_len = 0;

    if length > 0 {
        new_len = length - 1;

        res = if this_obj.is_fast_array()
            && let Some(arr_data) = this_obj.payload_mut().array_mut()
            && arr_data.len() == length as usize
        {
            // fast array
            if shift {
                arr_data.remove(0)
            } else {
                arr_data.pop().unwrap()
            }
        } else {
            // slow path
            let val = if shift {
                let v = this_obj.get_property_i64(ctx, 0)?;
                copy_sub_array(ctx, &mut this_obj, 0, 1, length as usize - 1, 1)?;
                v
            } else {
                this_obj.get_property_i64(ctx, new_len)?
            };
            this_obj.delete_property_i64(ctx, new_len, JS_PROP_THROW)?;
            val
        };
    }

    this_obj.set_property(ctx, Atom::CONST_length, JSValue::from(new_len))?;
    Ok(res)
}

pub(crate) fn js_array_push(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let unshift = cc.magic();

    let mut this_obj = cc.this().objectify(ctx)?;
    let len = this_obj.get_length_prop(ctx)?;

    let new_len = len + cc.length() as i64;
    if new_len > MAX_SAFE_INTEGER {
        ctx.throw_type_error("Array loo long");
        return Err(-1);
    }

    let mut from = len;
    if unshift != 0 && cc.length() > 0 {
        copy_sub_array(ctx, &mut this_obj, cc.length() as _, 0, len as usize, -1)?;
        from = 0;
    }

    for i in 0..cc.length() {
        this_obj.set_property_i64(ctx, from + i as i64, cc.arg(i))?;
    }

    this_obj.set_property(ctx, Atom::CONST_length, JSValue::from(new_len))?;

    Ok(JSValue::from(new_len))
}

fn js_array_reverse(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this().objectify(ctx)?;
    let len = this.get_length_prop(ctx)?;

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
        let l_present = match this.try_get_property_i64(ctx, l)? {
            Some(v) => {
                lval = v;
                true
            }
            None => false,
        };

        let h_present = match this.try_get_property_i64(ctx, h)? {
            Some(v) => {
                hval = v;
                true
            }
            None => false,
        };

        if h_present {
            this.set_property_i64(ctx, l, hval)?;

            if l_present {
                this.set_property_i64(ctx, h, lval)?;
                lval = JSValue::UNDEFINED;
            } else {
                this.delete_property_i64(ctx, h, JS_PROP_THROW)?;
            }
        } else if l_present {
            this.delete_property_i64(ctx, l, JS_PROP_THROW)?;
            this.set_property_i64(ctx, h, lval)?;
            lval = JSValue::UNDEFINED;
        }

        l += 1;
        h -= 1;
    }

    Ok(this.into())
}

fn js_array_toReversed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
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
            let mut lst = alloc_fast_array_slots(ctx, len as _)?;
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

    Array::from_values(ctx, new_lst).map(JSValue::from)
}

fn js_array_sort(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let cmpfn = {
        let f = cc.arg(0);
        if !f.is_undefined() {
            f.ensure_callable(ctx)?;
            Some(f)
        } else {
            None
        }
    };
    let mut obj = cc.this().objectify(ctx)?;
    Array::sort(ctx, &mut obj, cmpfn)?;
    Ok(obj.into())
}

fn js_array_toSorted(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let cmpfn = {
        let f = cc.arg(0);
        if !f.is_undefined() {
            f.ensure_callable(ctx)?;
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
            let mut lst = alloc_fast_array_slots(ctx, len as _)?;
            for i in 0..len {
                let v = this.try_get_property_i64(ctx, i)?;
                lst.push(v.unwrap_or(JSValue::UNDEFINED));
            }
            lst
        }
    } else {
        Vec::new()
    };

    let mut arr = Array::from_values(ctx, new_lst)?;
    Array::sort(ctx, &mut arr, cmpfn)?;
    Ok(arr.into())
}

pub(crate) fn js_array_slice(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_splice = cc.magic() != 0;

    let mut this_obj = cc.this().objectify(ctx)?;
    let len = this_obj.get_length_prop(ctx)?;
    let start = cc.arg(0).to_number(ctx)?.to_i64_clamp(0, len, len);

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
            del_count = cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len - start, 0);
            item_count = (cc.length() - 2) as u32;
        }
        if len + item_count as i64 - del_count > MAX_SAFE_INTEGER {
            ctx.throw_type_error("Array loo long");
            return Err(-1);
        }
        del_count
    } else {
        item_count = 0;
        del_count = 0;
        let end = if !cc.arg(1).is_undefined() {
            cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len, len)
        } else {
            len
        };
        std::cmp::max(end - start, 0)
    };

    let end = start + count;

    // make return Array
    let len_val = JSValue::from(count);
    let mut res_arr = JS_ArraySpeciesCreate(ctx, this_obj.into(), len_val)?;

    let mut k = start;
    let mut n = 0;

    // Special case fast arrays
    if let Some(mut arr1) = res_arr.as_object_Array()
        && arr1.is_fast_array()
        && this_obj.is_fast_array()
        && let Some(arr_data) = this_obj.payload_mut().array_mut()
    {
        let lst = arr_data
            .values()
            .iter()
            .skip(start as usize)
            .take(count as usize)
            .copied()
            .try_collect::<Vec<_>>()
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        if !lst.is_empty() {
            n = lst.len() as i64;
            k += n;
            arr1.set_fast_array_values(ctx, lst)?;
        }
    }

    // Copy the remaining elements if any (handle case of inherited properties)
    while k < end {
        if let Some(v) = this_obj.try_get_property_i64(ctx, k)? {
            res_arr.create_data_property_u32(ctx, n as _, v, JS_PROP_THROW)?;
        }
        k += 1;
        n += 1;
    }

    res_arr.set_property(ctx, Atom::CONST_length, JSValue::from(n))?;

    if is_splice {
        let new_len = len + item_count as i64 - del_count;
        if item_count as i64 != del_count {
            copy_sub_array(
                ctx,
                &mut this_obj,
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
                this_obj.delete_property_i64(ctx, i, JS_PROP_THROW)?;
            }
        }

        for i in 0..item_count {
            this_obj.set_property_i64(ctx, start + i as i64, cc.arg(i as usize + 2))?;
        }

        this_obj.set_property(ctx, Atom::CONST_length, JSValue::from(new_len))?;
    }

    Ok(res_arr)
}

fn js_array_toSpliced(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this_obj = cc.this().objectify(ctx)?;
    let len = this_obj.get_length_prop(ctx)?;

    let start = if cc.length() > 0 {
        cc.arg(0).to_number(ctx)?.to_i64_clamp(0, len, len)
    } else {
        0
    };

    let mut del = 0;
    if cc.length() > 0 {
        del = len - start;
    }
    if cc.length() > 1 {
        del = cc.arg(1).to_number(ctx)?.to_i64_clamp(0, del, 0);
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

    let mut new_lst = alloc_fast_array_slots(ctx, new_len as usize)?;
    if new_len > 0 {
        if this_obj.is_fast_array()
            && let Some(arr_data) = this_obj.as_Array_mut().map(|a| a.payload_mut())
            && arr_data.len() == len as usize
        {
            // fast array
            new_lst.extend(arr_data.values().iter().take(start as usize));
            for j in 0..add {
                new_lst.push(cc.arg((2 + j) as usize));
            }
            new_lst.extend(arr_data.values().iter().skip((start + del) as usize));
        } else {
            // slow path: sparse array
            for i in 0..start {
                let el = this_obj
                    .try_get_property_i64(ctx, i)
                    .inspect_err(|_| {
                        // error: fill rest with undefined
                        new_lst.extend(std::iter::repeat_n(
                            JSValue::UNDEFINED,
                            (new_len - i) as usize,
                        ));
                    })?
                    .unwrap_or(JSValue::UNDEFINED);
                new_lst.push(el);
            }

            for i in 0..add {
                new_lst.push(cc.arg((2 + i) as usize));
            }

            for i in (start + del)..len {
                let el = this_obj
                    .try_get_property_i64(ctx, i)
                    .inspect_err(|_| {
                        // error: fill rest with undefined
                        new_lst.extend(std::iter::repeat_n(
                            JSValue::UNDEFINED,
                            (new_len - i) as usize,
                        ));
                    })?
                    .unwrap_or(JSValue::UNDEFINED);
                new_lst.push(el);
            }
        }
    }

    // create result array
    Array::from_values(ctx, new_lst).map(|a| a.into())
}

fn js_array_copyWithin(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this_obj = cc.this().objectify(ctx)?;
    let len = this_obj.get_length_prop(ctx)?;

    let to: i64 = cc.arg(0).to_number(ctx)?.to_i64_clamp(0, len, len);
    let start: i64 = cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len, len);
    let end = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_number(ctx)?.to_i64_clamp(0, len, len)
    } else {
        len
    };

    let count = std::cmp::min(end - start, len - to);
    if count > 0 {
        copy_sub_array(
            ctx,
            &mut this_obj,
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

    Ok(this_obj.into())
}

pub(crate) fn js_array_flatten(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let map = cc.magic();

    let source: JSValue = cc.this().objectify(ctx)?.into();
    let source_len = source.get_length_prop(ctx)?;

    let mut depth = 1;
    let mut mapper_func = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if map != 0 {
        mapper_func = cc.arg(0);
        if cc.length() > 1 {
            this_arg = cc.arg(1);
        }
        mapper_func.ensure_callable(ctx)?;
    } else if cc.length() > 0 && !cc.arg(0).is_undefined() {
        depth = cc.arg(0).to_number(ctx)?.to_i32_saturating();
    }

    let arr = JS_ArraySpeciesCreate(ctx, source, JSValue::from(0))?;

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
    ctx: &mut ScopedContext,
    mut target: JSValue,
    source: JSValue,
    source_len: i64,
    mut target_index: i64,
    depth: i32,
    mapper_func: JSValue,
    this_arg: JSValue,
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
    FieldDef::FUNC("at", 1, js_array_at, 0),
    FieldDef::FUNC("with", 2, js_array_with, 0),
    FieldDef::FUNC("concat", 1, js_array_concat, 0),
    FieldDef::FUNC("every", 1, js_array_every, special_every as i16),
    FieldDef::FUNC("some", 1, js_array_every, special_some as i16),
    FieldDef::FUNC("forEach", 1, js_array_every, special_forEach as i16),
    FieldDef::FUNC("map", 1, js_array_every, special_map as i16),
    FieldDef::FUNC("filter", 1, js_array_every, special_filter as i16),
    FieldDef::FUNC("reduce", 1, js_array_reduce, special_reduce as i16),
    FieldDef::FUNC(
        "reduceRight",
        1,
        js_array_reduce,
        special_reduceRight as i16,
    ),
    FieldDef::FUNC("fill", 1, js_array_fill, 0),
    FieldDef::FUNC("find", 1, js_array_find, special_find as i16),
    FieldDef::FUNC("findIndex", 1, js_array_find, special_findIndex as i16),
    FieldDef::FUNC("findLast", 1, js_array_find, special_findLast as i16),
    FieldDef::FUNC(
        "findLastIndex",
        1,
        js_array_find,
        special_findLastIndex as i16,
    ),
    FieldDef::FUNC("indexOf", 1, js_array_indexOf, 0),
    FieldDef::FUNC("lastIndexOf", 1, js_array_lastIndexOf, 0),
    FieldDef::FUNC("includes", 1, js_array_includes, 0),
    FieldDef::FUNC("join", 1, js_array_join, 0),
    FieldDef::FUNC("toString", 0, js_array_toString, 0),
    FieldDef::FUNC("toLocaleString", 0, js_array_join, 1),
    FieldDef::FUNC("pop", 0, js_array_pop, 0),
    FieldDef::FUNC("push", 1, js_array_push, 0),
    FieldDef::FUNC("shift", 0, js_array_pop, 1),
    FieldDef::FUNC("unshift", 1, js_array_push, 1),
    FieldDef::FUNC("reverse", 0, js_array_reverse, 0),
    FieldDef::FUNC("toReversed", 0, js_array_toReversed, 0),
    FieldDef::FUNC("sort", 1, js_array_sort, 0),
    FieldDef::FUNC("toSorted", 1, js_array_toSorted, 0),
    FieldDef::FUNC("slice", 2, js_array_slice, 0),
    FieldDef::FUNC("splice", 2, js_array_slice, 1),
    FieldDef::FUNC("toSpliced", 2, js_array_toSpliced, 0),
    FieldDef::FUNC("copyWithin", 2, js_array_copyWithin, 0),
    FieldDef::FUNC("flatMap", 1, js_array_flatten, 1),
    FieldDef::FUNC("flat", 0, js_array_flatten, 0),
    FieldDef::FUNC(
        "values",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    FieldDef::ALIAS("[Symbol.iterator]", "values"),
    FieldDef::FUNC(
        "keys",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16,
    ),
    FieldDef::FUNC(
        "entries",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16,
    ),
];

pub const js_array_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 0, js_array_iterator_next, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Array Iterator",
        JS_PROP_CONFIGURABLE,
    ),
];

//
// static array functions
//

fn js_array_from(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // from(items, mapfn = void 0, this_arg = void 0)

    let items = cc.arg(0);
    let mut mapping = false;
    let mut map_fn = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if cc.length() > 1 {
        map_fn = cc.arg(1);
        if !map_fn.is_undefined() {
            map_fn.ensure_callable(ctx)?;
            mapping = true;
            if cc.length() > 2 {
                this_arg = cc.arg(2);
            }
        }
    }

    let mut k: i64 = 0;
    let mut res = JSValue::UNDEFINED;

    // check and use [Symbol.iterator] method
    let has_iterator: bool = {
        {
            // protect items - NOT NEEDED ANY MORE
            // if let Some(n) = items.gc_node_ptr() {
            //     ctx.gc_heap_mut().protect_node_v2(n);
            // }

            let it = items.get_property(ctx, Atom::CONST_Symbol_iterator)?;
            let has = !it.is_undefined();

            if has {
                // `items` has [Symbol.iterator] method
                let mut arr = if cc.this().is_constructor() {
                    cc.this().call_ctor(ctx, &[])?
                } else {
                    Array::create(ctx)?.into()
                };

                // NOT NEEDED ANY MORE
                // if let Some(n) = arr.gc_node_ptr() {
                //     _ng.add(n);
                // }

                let iter = items.get_iterator_must(ctx, false)?;
                let it = ObjectIter::new(ctx, iter, Atom::CONST_next)?;

                for el in it {
                    let mut val = el?;
                    if mapping {
                        val = map_fn.call(ctx, this_arg, &[val, JSValue::from(k as i32)])?;
                    }
                    arr.create_data_property_u32(ctx, k as _, val, JS_PROP_THROW)?;
                    k += 1;
                }

                res = arr;
            }
            has
        }
    };

    if !has_iterator {
        let src = items.objectify(ctx)?;
        let len = src.get_length_prop(ctx)?;
        let len_val = JSValue::from(len);

        let mut arr = if cc.this().is_constructor() {
            cc.this().call_ctor(ctx, std::slice::from_ref(&len_val))?
        } else {
            Array::with_capacity(ctx, len as usize)?.into()
        };

        // protect arr, src nodes - NOT NEEDED ANY MORE
        // ctx.gc_heap_mut().protect_node_v2(src.gc_node_ptr());
        // if let Some(n) = arr.gc_node_ptr() {
        //     ctx.gc_heap_mut().protect_node_v2(n);
        // }

        for i in 0..len {
            let mut el = src.get_property_i64(ctx, i)?;
            if mapping {
                el = map_fn.call(ctx, this_arg, &[el, JSValue::from(i as i32)])?;
            }
            arr.create_data_property_u32(ctx, i as _, el, JS_PROP_THROW)?;
        }

        k = len;
        res = arr;
    }

    res.set_property(ctx, Atom::CONST_length, JSValue::from(k as u32))?;
    Ok(res)
}

fn js_array_of(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let mut arr = if this.is_constructor() {
        this.call_ctor(
            ctx,
            std::slice::from_ref(&JSValue::from(cc.length() as i32)),
        )?
    } else {
        Array::create(ctx)?.into()
    };

    let cnt = cc.length();
    for i in 0..cnt {
        arr.create_data_property_u32(ctx, i as _, cc.arg(i), JS_PROP_THROW)?;
    }

    arr.set_property(ctx, Atom::CONST_length, JSValue::from(cnt as u32))?;
    Ok(arr)
}

fn js_array_isArray(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    Ok(JSValue::from(cc.arg(0).is_array(ctx)?))
}

pub const js_array_funcs: &[FieldDef] = &[
    FieldDef::FUNC("isArray", 1, js_array_isArray, 0),
    FieldDef::FUNC("from", 1, js_array_from, 0),
    FieldDef::FUNC("of", 0, js_array_of, 0),
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

/// 将数组 obj 中从 start_pos 开始的 count 个元素复制到 to_pos 开始的位置。
/// - 正向复制（dir > 0）：从 start_pos 到 start_pos + count - 1 的元素依次复制到 to_pos 到 to_pos + count - 1。
/// - 反向复制（dir < 0）：从 start_pos + count - 1 到 start_pos 的元素依次复制到 to_pos + count - 1 到 to_pos。
fn copy_sub_array(
    ctx: &mut ScopedContext,
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
                arr_data.set_nth(to, val);
            } else {
                done_count = i;
                break;
            }
        }

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
