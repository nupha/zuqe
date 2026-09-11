// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use fallible_collections::TryCollect;
use gc_lite::GcNode;

use crate::{
    JS_CLASS_ARRAY,
    array::{Array, ArrayPayload, alloc_fast_array_slots},
    atom::{Atom, JS_ATOM_MAX_INT},
    builtins::{
        common::{SPECIES_FIELD, arg_rel_index},
        iterator::{
            JSIteratorKindEnum, ObjectIter, js_array_iterator_next, js_create_array_iterator,
        },
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
        PayloadPtr, ThrowPolicy,
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

    let (values, len) = if cc.length() == 1 && cc.arg(0).is_number() {
        // new Array(capacity) case
        (Vec::new(), cc.arg(0).to_array_length(ctx, true)?)
    } else {
        // new Array(el1, el2, ...) case
        let list = try_collect_values(ctx, cc.arguments().iter().copied())?;
        let len = list.len() as u32;
        (list, len)
    };

    let mut arr = Array::try_from(ctx.ordinary_create_from_constructor_payload(
        new_target,
        PayloadPtr::new(crate::JS_CLASS_ARRAY, ArrayPayload::new(values)),
    )?)
    .unwrap();

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

//
// shared helpers for the Array.prototype methods below
//

/// `ToObject(this)` + `LengthOfArrayLike`: the common prologue of the generic
/// (non typed-array) Array.prototype methods.
fn this_obj_and_len(ctx: &mut ScopedContext, cc: &CallContext) -> Result<(Object, i64), i32> {
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    Ok((obj, len))
}

/// Collect values into a `Vec`, converting allocation failure into a pending
/// out-of-memory exception.
fn try_collect_values(
    ctx: &mut ScopedContext,
    iter: impl Iterator<Item = JSValue>,
) -> Result<Vec<JSValue>, i32> {
    iter.try_collect::<Vec<_>>()
        .map_err(|_| ctx.throw_out_of_memory())
}

/// Throw RangeError when `len` exceeds the maximum length a copy method
/// (`with`/`toReversed`/`toSorted`) is willing to materialize.
fn ensure_u32_len(ctx: &mut ScopedContext, len: i64) -> Result<(), i32> {
    if len > u32::MAX as i64 {
        return Err(ctx.throw_range_error("invalid array length"));
    }
    Ok(())
}

/// Throw TypeError when `new_len` exceeds the spec's 2^53-1 array length limit.
fn ensure_array_len_limit(ctx: &mut ScopedContext, new_len: i64) -> Result<(), i32> {
    if new_len > MAX_SAFE_INTEGER {
        return Err(ctx.throw_type_error("Array loo long"));
    }
    Ok(())
}

/// Read one element for the callback-driven methods: `dense` receivers (typed
/// arrays) always yield a value, generic receivers report holes as `None`.
fn element_at(
    ctx: &mut ScopedContext,
    obj: JSValue,
    k: i64,
    dense: bool,
) -> Result<Option<JSValue>, i32> {
    if dense {
        obj.get_property_i64(ctx, k).map(Some)
    } else {
        obj.try_get_property_i64(ctx, k)
    }
}

/// `comparefn` argument of `sort`/`toSorted`: `undefined` means default
/// ordering, anything else must be callable.
fn opt_comparefn(ctx: &mut ScopedContext, cc: &CallContext) -> Result<Option<JSValue>, i32> {
    let f = cc.arg(0);
    if f.is_undefined() {
        Ok(None)
    } else {
        f.ensure_callable(ctx)?;
        Ok(Some(f))
    }
}

/// Copy the elements of `obj` at `range` into `dst`, reading holes as
/// `undefined`. Should a getter fail, pad `dst` with `undefined` up to
/// `pad_end` before propagating, keeping the buffer at its expected final
/// size.
fn push_elements_padded(
    ctx: &mut ScopedContext,
    obj: Object,
    dst: &mut Vec<JSValue>,
    range: std::ops::Range<i64>,
    pad_end: i64,
) -> Result<(), i32> {
    for i in range {
        match obj.try_get_property_i64(ctx, i) {
            Ok(v) => dst.push(v.unwrap_or(JSValue::UNDEFINED)),
            Err(_) => {
                for _ in i..pad_end {
                    dst.push(JSValue::UNDEFINED);
                }
                return Err(-1);
            }
        }
    }
    Ok(())
}

/// Snapshot the elements `0..len` of `obj` into a `Vec` (holes become
/// `undefined`), optionally in reverse. Fast arrays copy the payload wholesale;
/// generic receivers fall back to per-index reads, whose (observable) query
/// order follows the requested direction.
fn array_values_snapshot(
    ctx: &mut ScopedContext,
    obj: Object,
    len: i64,
    rev: bool,
) -> Result<Vec<JSValue>, i32> {
    if len == 0 {
        return Ok(Vec::new());
    }

    if obj.is_fast_array()
        && let Some(arr_data) = obj.payload_array()
    {
        let it = arr_data.values().iter().copied();
        return if rev {
            try_collect_values(ctx, it.rev())
        } else {
            try_collect_values(ctx, it)
        };
    }

    let mut lst = alloc_fast_array_slots(ctx, len as _)?;
    if rev {
        for i in (0..len).rev() {
            lst.push(
                obj.try_get_property_i64(ctx, i)?
                    .unwrap_or(JSValue::UNDEFINED),
            );
        }
    } else {
        for i in 0..len {
            lst.push(
                obj.try_get_property_i64(ctx, i)?
                    .unwrap_or(JSValue::UNDEFINED),
            );
        }
    }
    Ok(lst)
}

fn js_array_at(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (obj, len) = this_obj_and_len(ctx, &cc)?;

    let mut idx: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
    if idx < 0 {
        idx += len;
    }
    if !(0..len).contains(&idx) {
        return Ok(JSValue::UNDEFINED);
    }

    // fast array: read the payload slot directly
    if obj.is_fast_array()
        && let Some(a) = obj.payload_array()
        && let Some(v) = a.values().get(idx as usize)
    {
        return Ok(*v);
    }

    Ok(obj
        .try_get_property_i64(ctx, idx)?
        .unwrap_or(JSValue::UNDEFINED))
}

fn js_array_with(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (obj, len) = this_obj_and_len(ctx, &cc)?;

    let mut idx: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
    if idx < 0 {
        idx += len;
    }
    if idx < 0 || idx >= len {
        return Err(ctx.throw_range_error("out of bound"));
    }
    ensure_u32_len(ctx, len)?;

    let new_lst = if obj.is_fast_array()
        && let Some(src) = obj.as_Array().map(|a| a.payload())
        && src.len() == len as usize
    {
        // `this` is fast array: copy list, then patch the target slot
        let mut dst = try_collect_values(ctx, src.values().iter().copied())?;
        dst[idx as usize] = cc.arg(1);
        dst
    } else {
        // slow path: element-wise copy with the target slot replaced
        let mut dst = alloc_fast_array_slots(ctx, len as _)?;
        push_elements_padded(ctx, obj, &mut dst, 0..idx, len)?;
        dst.push(cc.arg(1));
        push_elements_padded(ctx, obj, &mut dst, (idx + 1)..len, len)?;
        dst
    };

    debug_assert_eq!(new_lst.len(), len as usize);

    // make Array with values
    let arr = Array::from_values(ctx, new_lst)?;
    debug_assert_eq!(arr.get_length_prop(ctx).unwrap(), len);
    Ok(arr.into())
}

fn js_array_concat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this: JSValue = cc.this().objectify(ctx)?.into();
    let mut arr = JS_ArraySpeciesCreate(ctx, this, JSValue::from(0))?;

    let mut n = 0;

    for i in -1..cc.length() as i32 {
        let v = if i < 0 { this } else { cc.arg(i as usize) };

        if JS_isConcatSpreadable(ctx, v)? {
            let len = v.get_length_prop(ctx)?;
            ensure_array_len_limit(ctx, n + len)?;

            for k in 0..len {
                if let Some(val) = v.try_get_property_i64(ctx, k)? {
                    arr.define_property_value_i64(ctx, n, val, JS_PROP_C_W_E | JS_PROP_THROW)?;
                }
                n += 1;
            }
        } else {
            // about to append one more element
            ensure_array_len_limit(ctx, n + 1)?;

            arr.define_property_value_i64(ctx, n as _, v, JS_PROP_C_W_E | JS_PROP_THROW)?;
            n += 1;
        }
    }

    arr.set_property(ctx, Atom::CONST_length, JSValue::from(n))?;
    Ok(arr)
}

pub(crate) fn js_array_every(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let special = cc.magic();
    let dense = special & special_TA != 0;

    let obj: JSValue;
    let len: i64;

    if dense {
        obj = cc.this();
        len = js_typed_array_get_length_internal(ctx, obj)? as i64;
    } else {
        let (o, l) = this_obj_and_len(ctx, &cc)?;
        obj = o.into();
        len = l;
    }

    let func = cc.arg(0);
    let this_arg = cc.arg(1);
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

    let mut n = 0;

    for k in 0..len {
        let Some(val) = element_at(ctx, obj, k, dense)? else {
            continue;
        };

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
                ret.set_property_value(
                    ctx,
                    JSValue::from(k as i32),
                    res,
                    ThrowPolicy::Always,
                    false,
                )?;
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
    let rev = special & special_reduceRight != 0;

    let this = cc.this();
    let this_obj: Object;
    let len: i64;

    if special & special_TA != 0 {
        this_obj = this
            .as_object()
            .ok_or_else(|| ctx.throw_type_error_not_an_object())?;
        len = js_typed_array_get_length_internal(ctx, this)? as i64;
    } else {
        this_obj = this.objectify(ctx)?;
        len = this_obj.get_length_prop(ctx)?;
    }

    let func = cc.arg(0);
    func.ensure_callable(ctx)?;

    // `None` = no initial value: the first present element seeds the accumulator
    let init = (cc.length() > 1).then(|| cc.arg(1));

    if special & special_TA != 0 || len <= JS_ATOM_MAX_INT as i64 {
        // Sequential scan over the full range (reversed for reduceRight).
        // Typed arrays are dense so every read succeeds; for generic receivers
        // every index is a small int, so `try_get_property_i64` takes the
        // allocation-free fast path, and the scan matches the spec's sequential
        // semantics (e.g. getters that add properties while visited).
        let dense = special & special_TA != 0;
        let idxs = (0..len).map(|k| if rev { len - 1 - k } else { k });
        return reduce_fold(ctx, this_obj, func, init, dense, idxs);
    }

    // Huge length: scanning `0..len` would allocate an atom per index and
    // exhaust the atom table. Enumerate only the actually-present integer
    // indices (own + inherited) instead. `reduce` already skips holes per
    // spec, so this is exactly equivalent.
    let mut idxs = collect_present_indices(ctx, this_obj, len)?;
    if rev {
        idxs.reverse();
    }
    reduce_fold(ctx, this_obj, func, init, false, idxs.into_iter())
}

/// Shared `reduce`/`reduceRight` loop: fold `func` over the elements of
/// `this_obj` at `idxs`. `acc = None` means no initial value was given, so the
/// first present element seeds the accumulator; if none exists, throw per
/// spec. `dense` receivers (typed arrays) have no holes and are read
/// unconditionally; otherwise absent indices are skipped.
fn reduce_fold(
    ctx: &mut ScopedContext,
    this_obj: Object,
    func: JSValue,
    mut acc: Option<JSValue>,
    dense: bool,
    idxs: impl Iterator<Item = i64>,
) -> JSResult {
    // per spec the callback's 4th argument is the coerced object O, not the
    // original (possibly primitive) `this` value
    let this_val: JSValue = this_obj.into();

    for k1 in idxs {
        let Some(val) = element_at(ctx, this_val, k1, dense)? else {
            continue;
        };
        acc = Some(match acc {
            Some(a) => {
                let args = [a, val, JSValue::from(k1), this_val];
                func.call(ctx, JSValue::UNDEFINED, &args)?
            }
            None => val,
        });
    }

    acc.ok_or_else(|| ctx.throw_type_error("empty array"))
}

fn js_array_fill(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (obj, len) = this_obj_and_len(ctx, &cc)?;
    let mut obj: JSValue = obj.into();

    let mut start = arg_rel_index(ctx, &cc, 1, len, 0)?;
    let end = arg_rel_index(ctx, &cc, 2, len, len)?;

    // XXX: should special case fast arrays
    while start < end {
        obj.set_property_i64(ctx, start, cc.arg(0))?;
        start += 1;
    }

    Ok(obj)
}

fn js_array_find(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mode = cc.magic();
    let last = matches!(mode, special_findLast | special_findLastIndex);
    let want_index = matches!(mode, special_findIndex | special_findLastIndex);

    let (obj, len) = this_obj_and_len(ctx, &cc)?;
    let obj: JSValue = obj.into();

    let func = cc.arg(0);
    func.ensure_callable(ctx)?;

    let this_val = cc.this();
    let this_arg = cc.arg(1);

    // TODO: add fast path for fast arrays
    for i in 0..len {
        let k = if last { len - 1 - i } else { i };
        let index_val = JSValue::from(k);
        let val = obj.get_property_value(ctx, index_val)?;
        let res = func.call(ctx, this_arg, &[val, index_val, this_val])?;

        if res.to_bool()? {
            // found
            return Ok(if want_index { index_val } else { val });
        }
    }

    Ok(if want_index {
        JSValue::from(-1)
    } else {
        JSValue::UNDEFINED
    })
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
    let (this_obj, len) = this_obj_and_len(ctx, &cc)?;

    if len > 0 {
        let start = arg_rel_index(ctx, &cc, 1, len, 0)?;

        let target = cc.arg(0);

        // fast path: fast array
        if this_obj.is_fast_array()
            && let Some((i, _)) = this_obj.payload_array().and_then(|a| {
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
    let (this_obj, len) = this_obj_and_len(ctx, &cc)?;
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
            let idxs = collect_present_indices(ctx, this_obj, len)?;
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
    let (obj, length) = this_obj_and_len(ctx, &cc)?;
    let target = cc.arg(0);

    if length > 0 {
        let start = arg_rel_index(ctx, &cc, 1, length, 0)?;

        // fast path: fast array
        if obj.is_fast_array()
            && let Some(arr_data) = obj.payload_array()
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
    #[cfg(feature = "intl402")]
    let to_locale_string = cc.magic() == 1;
    #[cfg(not(feature = "intl402"))]
    let to_locale_string = false;

    let (this_obj, len) = this_obj_and_len(ctx, &cc)?;

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
                let s = el.invoke(ctx, Atom::CONST_toLocaleString, &[cc.arg(0), cc.arg(1)])?;
                s.to_string(ctx)?
            } else if let Some(s) = el.as_string(true) {
                s
            } else {
                el.to_string(ctx)?
            };
            sbuf.push_content(ss.content())
                .map_err(|_| ctx.throw_out_of_memory())?
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

    let (mut this_obj, length) = this_obj_and_len(ctx, &cc)?;
    let mut res = JSValue::UNDEFINED;
    let mut new_len = 0;

    if length > 0 {
        new_len = length - 1;

        res = if this_obj.is_fast_array()
            && let Some(arr_data) = this_obj.payload_array_mut()
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

    let (mut this_obj, len) = this_obj_and_len(ctx, &cc)?;

    let new_len = len + cc.length() as i64;
    ensure_array_len_limit(ctx, new_len)?;

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
    let (mut this, len) = this_obj_and_len(ctx, &cc)?;

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

    // slow array: swap the `l`/`h` pair, deleting the target slot when the
    // source side is a hole
    let mut l = 0;
    let mut h = len - 1;

    while l < h {
        let lval = this.try_get_property_i64(ctx, l)?;
        let hval = this.try_get_property_i64(ctx, h)?;

        match (lval, hval) {
            (Some(lv), Some(hv)) => {
                this.set_property_i64(ctx, l, hv)?;
                this.set_property_i64(ctx, h, lv)?;
            }
            (None, Some(hv)) => {
                this.set_property_i64(ctx, l, hv)?;
                this.delete_property_i64(ctx, h, JS_PROP_THROW)?;
            }
            (Some(lv), None) => {
                this.delete_property_i64(ctx, l, JS_PROP_THROW)?;
                this.set_property_i64(ctx, h, lv)?;
            }
            (None, None) => {}
        }

        l += 1;
        h -= 1;
    }

    Ok(this.into())
}

fn js_array_toReversed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (this, len) = this_obj_and_len(ctx, &cc)?;
    ensure_u32_len(ctx, len)?;

    // Query order is observable; test262 expects descending order.
    let new_lst = array_values_snapshot(ctx, this, len, true)?;
    Array::from_values(ctx, new_lst).map(JSValue::from)
}

fn js_array_sort(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let cmpfn = opt_comparefn(ctx, &cc)?;
    let mut obj = cc.this().objectify(ctx)?;
    Array::sort(ctx, &mut obj, cmpfn)?;
    Ok(obj.into())
}

fn js_array_toSorted(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let cmpfn = opt_comparefn(ctx, &cc)?;

    let (this, len) = this_obj_and_len(ctx, &cc)?;
    ensure_u32_len(ctx, len)?;

    let new_lst = array_values_snapshot(ctx, this, len, false)?;
    let mut arr = Array::from_values(ctx, new_lst)?;
    Array::sort(ctx, &mut arr, cmpfn)?;
    Ok(arr.into())
}

pub(crate) fn js_array_slice(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_splice = cc.magic() != 0;

    let (mut this_obj, len) = this_obj_and_len(ctx, &cc)?;
    let start = arg_rel_index(ctx, &cc, 0, len, 0)?;

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
        ensure_array_len_limit(ctx, len + item_count as i64 - del_count)?;
        del_count
    } else {
        item_count = 0;
        del_count = 0;
        let end = arg_rel_index(ctx, &cc, 1, len, len)?;
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
        && let Some(arr_data) = this_obj.payload_array_mut()
    {
        let lst = try_collect_values(
            ctx,
            arr_data
                .values()
                .iter()
                .skip(start as usize)
                .take(count as usize)
                .copied(),
        )?;

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
    let (mut this_obj, len) = this_obj_and_len(ctx, &cc)?;

    let start = arg_rel_index(ctx, &cc, 0, len, 0)?;

    let mut del = if cc.length() > 0 { len - start } else { 0 };
    if cc.length() > 1 {
        del = cc.arg(1).to_number(ctx)?.to_i64_clamp(0, del, 0);
    }

    let add = (cc.length() as i64 - 2).max(0);

    let new_len = len + add - del;
    if new_len > MAX_SAFE_INTEGER {
        return Err(ctx.throw_type_error("invalid array length"));
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
            // slow path: sparse array (on error, pad up to `new_len`)
            push_elements_padded(ctx, this_obj, &mut new_lst, 0..start, new_len)?;
            for i in 0..add {
                new_lst.push(cc.arg((2 + i) as usize));
            }
            push_elements_padded(ctx, this_obj, &mut new_lst, (start + del)..len, new_len)?;
        }
    }

    // create result array
    Array::from_values(ctx, new_lst).map(|a| a.into())
}

fn js_array_copyWithin(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (mut this_obj, len) = this_obj_and_len(ctx, &cc)?;

    let to: i64 = arg_rel_index(ctx, &cc, 0, len, 0)?;
    let start: i64 = arg_rel_index(ctx, &cc, 1, len, 0)?;
    let end = arg_rel_index(ctx, &cc, 2, len, len)?;

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

    let (source, source_len) = this_obj_and_len(ctx, &cc)?;
    let source: JSValue = source.into();

    let mut depth = 1;
    let mut mapper_func = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if map != 0 {
        mapper_func = cc.arg(0);
        this_arg = cc.arg(1);
        mapper_func.ensure_callable(ctx)?;
    } else if !cc.arg(0).is_undefined() {
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
        return Err(ctx.throw_stack_overflow());
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
                    return Err(ctx.throw_type_error("Array too long"));
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
    #[cfg(feature = "intl402")]
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

/// Array.fromAsync (ES2024) defined in JS source: the algorithm needs
/// sequential awaits over iterator/promise results, which the VM's async
/// machinery already provides. Evaluated once at context setup (ctx.rs).
///
/// Spec notes kept here since they are easy to get wrong:
/// - async-iterable values are stored as-is (NOT awaited); sync-iterable
///   values go through AsyncFromSyncIteratorContinuation, which awaits them
///   and closes the iterator when a thenable value rejects;
/// - errors from next()/awaiting its result do NOT close the iterator,
///   errors from mapper/define/length DO (IfAbruptCloseAsyncIterator);
/// - non-callable mapper rejects the returned promise (async built-in);
/// - Construct(C, «len») must not observe iterators, so no spread-call.
pub(crate) const FROM_ASYNC_JS: &str = r#"
(function () {
    var MAX_SAFE = 9007199254740991;
    var DATA = { value: undefined, writable: true, enumerable: true, configurable: true };
    // intrinsics captured at load: globalThis.Symbol may be patched later
    var SYM_ITERATOR = Symbol.iterator;
    var SYM_ASYNC_ITERATOR = Symbol.asyncIterator;

    function isObject(v) {
        return v !== null && (typeof v === "object" || typeof v === "function");
    }

    function getMethod(obj, key) {
        var f = obj[key];
        if (f !== undefined && f !== null) {
            if (typeof f !== "function") throw new TypeError("not a function");
            return f;
        }
        return undefined;
    }

    function toLength(v) {
        v = Math.trunc(+v);
        if (!(v > 0)) return 0;
        return v > MAX_SAFE ? MAX_SAFE : v;
    }

    // Construct(C) / Construct(C, «len»), or undefined when C is not a
    // constructor. `class extends C` probes constructibility without
    // invoking C; no spread-call, which would observe the iterator protocol.
    function construct(C, hasLen, len) {
        if (typeof C !== "function") return undefined;
        try { void (class extends C {}); } catch (e) { return undefined; }
        return hasLen ? new C(len) : new C();
    }

    // AsyncIteratorClose with an abrupt completion: run return() when
    // present, awaiting its result; an error from return() replaces ours.
    async function closeIterator(iterator) {
        var ret = getMethod(iterator, "return");
        if (ret !== undefined) await ret.call(iterator);
    }

    var fromAsync = async function fromAsync(items, mapper = undefined, thisArg = undefined) {
        var C = this;

        var mapping = mapper !== undefined;
        if (mapping && typeof mapper !== "function") throw new TypeError("mapper is not a function");

        var asyncMethod = getMethod(items, SYM_ASYNC_ITERATOR);
        var syncMethod = asyncMethod === undefined ? getMethod(items, SYM_ITERATOR) : undefined;

        if (asyncMethod !== undefined || syncMethod !== undefined) {
            // iterator path
            var method = asyncMethod !== undefined ? asyncMethod : syncMethod;
            var iterator = method.call(items);
            if (!isObject(iterator)) throw new TypeError("iterator is not an object");
            var next = getMethod(iterator, "next");

            var A = construct(C, false);
            if (A === undefined) A = [];

            var k = 0;
            while (true) {
                if (k >= MAX_SAFE) {
                    await closeIterator(iterator);
                    throw new TypeError("iteration index overflow");
                }
                var result = await next.call(iterator);
                if (!isObject(result)) throw new TypeError("iterator result is not an object");
                if (result.done) break;
                var value = result.value;
                try {
                    if (syncMethod !== undefined) value = await value;
                    if (mapping) value = await mapper.call(thisArg, value, k);
                    DATA.value = value;
                    Object.defineProperty(A, k, DATA);
                } catch (e) {
                    await closeIterator(iterator);
                    throw e;
                }
                k++;
            }
            A.length = k;
            return A;
        }

        // array-like path
        var arrayLike = Object(items);
        var len = toLength(arrayLike.length);

        var A = construct(C, true, len);
        if (A === undefined) A = new Array(len);

        var k = 0;
        while (k < len) {
            var value = await arrayLike[k];
            if (mapping) value = await mapper.call(thisArg, value, k);
            DATA.value = value;
            Object.defineProperty(A, k, DATA);
            k++;
        }
        A.length = len;
        return A;
    };

    // a built-in async function: [[Prototype]] is %Function.prototype%, and
    // per the merged spec the property is {writable, non-enumerable, config}
    Object.setPrototypeOf(fromAsync, Function.prototype);
    Object.defineProperty(Array, "fromAsync", {
        value: fromAsync,
        writable: true,
        enumerable: false,
        configurable: true,
    });
})();
"#;

fn js_array_from(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // from(items, mapfn = void 0, this_arg = void 0)

    let items = cc.arg(0);
    let map_fn = cc.arg(1);
    let this_arg = cc.arg(2);

    let mapping = !map_fn.is_undefined();
    if mapping {
        map_fn.ensure_callable(ctx)?;
    }

    // check and use [Symbol.iterator] method
    let it = items.get_property(ctx, Atom::CONST_Symbol_iterator)?;

    let (res, len): (JSValue, i64) = if !it.is_undefined() {
        // `items` has [Symbol.iterator] method
        let mut arr = if cc.this().is_constructor() {
            cc.this().call_ctor(ctx, &[])?
        } else {
            Array::create(ctx)?.into()
        };

        let iter = items.get_iterator_must(ctx, false)?;
        let it = ObjectIter::new(ctx, iter, Atom::CONST_next)?;

        let mut k: i64 = 0;
        for el in it {
            let mut val = el?;
            if mapping {
                val = map_fn.call(ctx, this_arg, &[val, JSValue::from(k as i32)])?;
            }
            arr.create_data_property_u32(ctx, k as _, val, JS_PROP_THROW)?;
            k += 1;
        }

        (arr, k)
    } else {
        // array-like path
        let src = items.objectify(ctx)?;
        let len = src.get_length_prop(ctx)?;
        let len_val = JSValue::from(len);

        let mut arr = if cc.this().is_constructor() {
            cc.this().call_ctor(ctx, std::slice::from_ref(&len_val))?
        } else {
            Array::with_capacity(ctx, len as usize)?.into()
        };

        for i in 0..len {
            let mut el = src.get_property_i64(ctx, i)?;
            if mapping {
                el = map_fn.call(ctx, this_arg, &[el, JSValue::from(i as i32)])?;
            }
            arr.create_data_property_u32(ctx, i as _, el, JS_PROP_THROW)?;
        }

        (arr, len)
    };

    res.set_property(ctx, Atom::CONST_length, JSValue::from(len as u32))?;
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
    SPECIES_FIELD,
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

    /// the (source, destination) pair of the i-th copy step, honoring direction
    #[inline]
    const fn src_dst(
        dir: i32,
        start_pos: usize,
        to_pos: usize,
        count: usize,
        i: usize,
    ) -> (usize, usize) {
        if dir < 0 {
            (start_pos + count - i - 1, to_pos + count - i - 1)
        } else {
            (start_pos + i, to_pos + i)
        }
    }

    let mut start_pos = start_pos;
    let mut to_pos = to_pos;
    let mut count = count;

    // not for class ARGUMENTS
    if obj.is_fast_array() && obj.class_id() == JS_CLASS_ARRAY {
        let arr_data = obj.payload_array_mut().unwrap();
        let len = arr_data.len();
        let mut done_count = count;

        for i in 0..count {
            let (from, to) = src_dst(dir, start_pos, to_pos, count, i);

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
        let (from, to) = src_dst(dir, start_pos, to_pos, count, i);

        if let Some(v) = obj.try_get_property_i64(ctx, from as _)? {
            obj.set_property_i64(ctx, to as _, v)?;
        } else {
            obj.delete_property_i64(ctx, to as _, JS_PROP_THROW)?;
        }
    }

    Ok(())
}
