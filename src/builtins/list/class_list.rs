// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use fallible_collections::TryCollect;

use crate::{
    JS_CLASS_LIST,
    atom::Atom,
    builtins::{
        js_get_this,
        object::js_object_toString,
    },
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{CallContext, FieldDef},
    number::MAX_SAFE_INTEGER,
    object::{JSProperty, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_THROW, Object},
    runtime::js_check_stack_overflow,
    string::{StringBuf, StringRef},
    value::JSValue,
};

use super::{List, ListElement};

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

pub const special_reduce: i32 = 0;
pub const special_reduceRight: i32 = 1;

pub(crate) fn js_list_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let (mut list, len) = if cc.length() == 1 && cc.arg(0).is_number() {
        let cap = cc.arg(0).to_array_length(ctx, true)?;
        let list = List::try_from(
            ctx.create_instance_with_ctor(
                new_target,
                ObjectPayload::LIST(super::ListPayload::new(Vec::new())),
            )
            .map_err(|_| -1)?,
        )
        .unwrap();
        (list, cap)
    } else {
        let mut lst: Vec<Box<dyn ListElement>> = Vec::new();
        for i in 0..cc.length() {
            lst.push(ListElement::box_from_value(ctx, cc.arg(i))?);
        }
        let len = lst.len() as u32;
        let list = List::try_from(
            ctx.create_instance_with_ctor(
                new_target,
                ObjectPayload::LIST(super::ListPayload::new(lst)),
            )
            .map_err(|_| -1)?,
        )
        .unwrap();
        (list, len)
    };

    list.set_length(ctx, len, true)?;
    Ok(list.into())
}

fn js_list_at(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let mut idx: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
    if idx < 0 {
        idx += len;
    }

    let ret = if (0..len).contains(&idx) {
        let val = if obj.class_id() == JS_CLASS_LIST && obj.is_fast_array() {
            obj.payload().list().and_then(|l| {
                l.values().get(idx as usize).map(|el| el.to_value(ctx))
            })
        } else {
            None
        };
        val.unwrap_or_else(|| obj.get_property_i64(ctx, idx))?
    } else {
        JSValue::UNDEFINED
    };

    Ok(ret)
}

fn js_list_with(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
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
        ctx.throw_range_error("invalid list length");
        return Err(-1);
    }

    let new_val = cc.arg(1);

    if obj.class_id() == JS_CLASS_LIST && obj.is_fast_array() {
        if let Some(src) = obj.as_List().map(|a| a.payload()) {
            let mut dst = Vec::with_capacity(src.len());
            for (i, el) in src.values().iter().enumerate() {
                if i == idx as usize {
                    dst.push(ListElement::box_from_value(ctx, new_val)?);
                } else {
                    dst.push(ListElement::box_from_value(ctx, el.to_value(ctx)?)?);
                }
            }
            return List::from_values(ctx, dst).map(JSValue::from);
        }
    }

    // slow path: should not happen for well-formed List
    let mut dst = Vec::new();
    for i in 0..len {
        let v = obj.get_property_i64(ctx, i)?;
        if i == idx {
            dst.push(ListElement::box_from_value(ctx, new_val)?);
        } else {
            dst.push(ListElement::box_from_value(ctx, v)?);
        }
    }
    List::from_values(ctx, dst).map(JSValue::from)
}

fn js_list_concat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this: JSValue = cc.this().objectify(ctx)?.into();
    let len = this.get_length_prop(ctx)?;
    let mut arr = List::with_capacity(ctx, (len + cc.length() as i64) as usize)?;

    for i in 0..len {
        let v = this.get_property_i64(ctx, i)?;
        arr.payload_mut().push(ListElement::box_from_value(ctx, v)?)
            .map_err(|_| { ctx.throw_out_of_memory(); -1 })?;
    }

    for i in 0..cc.length() as i64 {
        let v = cc.arg(i as usize);
        arr.payload_mut().push(ListElement::box_from_value(ctx, v)?)
            .map_err(|_| { ctx.throw_out_of_memory(); -1 })?;
    }

    arr.set_length(ctx, (len + cc.length() as i64) as u32, true)?;
    Ok(arr.into())
}

pub(crate) fn js_list_every(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let special = cc.magic();

    let obj = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;

    let func = cc.arg(0);
    let this_arg = if cc.length() > 1 { cc.arg(1) } else { JSValue::UNDEFINED };
    func.ensure_function(ctx)?;

    let mut ret: JSValue = match special {
        s if s == special_every => JSValue::Bool(true),
        s if s == special_some => JSValue::Bool(false),
        special_map => List::with_capacity(ctx, len as usize)?.into(),
        special_filter => List::with_capacity(ctx, 0)?.into(),
        _ => JSValue::UNDEFINED,
    };

    let mut n = 0;

    for k in 0..len {
        let val = obj.get_property_i64(ctx, k)?;
        let index_val = JSValue::from(k);
        let res = func.call(ctx, this_arg, &[val, index_val, obj])?;

        match special {
            s if s == special_every => {
                if let Ok(false) = res.to_bool() {
                    ret = JSValue::Bool(false);
                    break;
                }
            }
            s if s == special_some => {
                if res.to_bool().unwrap_or(true) {
                    ret = JSValue::Bool(true);
                    break;
                }
            }
            special_map => {
                ret.set_property_i64(ctx, k, res)?;
            }
            special_filter => {
                if res.to_bool().unwrap_or(true) {
                    ret.set_property_i64(ctx, n, val)?;
                    n += 1;
                }
            }
            _ => {}
        }
    }

    if special == special_filter {
        ret.set_property(ctx, Atom::CONST_length, JSValue::from(n))?;
    }

    Ok(ret)
}

pub(crate) fn js_list_reduce(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let special = cc.magic();

    let obj = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;

    let func = cc.arg(0);
    func.ensure_function(ctx)?;

    let mut acc: JSValue;
    let mut k = 0;

    if cc.length() > 1 {
        acc = cc.arg(1);
    } else {
        loop {
            if k >= len {
                ctx.throw_type_error("empty list");
                return Err(-1);
            }
            let k1 = if special & special_reduceRight != 0 {
                len - k - 1
            } else {
                k
            };
            k += 1;
            if let Some(r) = obj.try_get_property_i64(ctx, k1)? {
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

        if let Some(val) = obj.try_get_property_i64(ctx, k1)? {
            let index_val = JSValue::from(k1);
            acc = func.call(ctx, JSValue::UNDEFINED, &[acc, val, index_val, obj])?;
        }
    }

    Ok(acc)
}

fn js_list_fill(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = cc.this().objectify(ctx)?;
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

    while start < end {
        obj.set_property_i64(ctx, start, cc.arg(0))?;
        start += 1;
    }

    Ok(obj.into())
}

fn js_list_find(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mode = cc.magic();

    let obj = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;

    let func = cc.arg(0);
    func.ensure_function(ctx)?;

    let this_val = cc.this();
    let this_arg = if cc.length() > 1 { cc.arg(1) } else { JSValue::UNDEFINED };

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

    while k != end {
        let index_val = JSValue::from(k);
        let val = obj.get_property_value(ctx, index_val)?;
        let res = func.call(ctx, this_arg, &[val, index_val, this_val])?;

        if res.to_bool()? {
            return if matches!(mode, special_findIndex | special_findLastIndex) {
                Ok(index_val)
            } else {
                Ok(val)
            };
        }

        k += dir;
    }

    Ok(if matches!(mode, special_findIndex | special_findLastIndex) {
        JSValue::Int(-1)
    } else {
        JSValue::UNDEFINED
    })
}

fn js_list_indexOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this().objectify(ctx)?;
    let len = this.get_length_prop(ctx)?;

    if len > 0 {
        let start = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len, len)
        } else {
            0
        };

        let target = cc.arg(0);

        for i in start..len {
            let val = this.get_property_i64(ctx, i)?;
            if target.strict_eq(val) {
                return Ok(JSValue::from(i as u32));
            }
        }
    }

    Ok(JSValue::Int(-1))
}

pub(crate) fn js_list_lastIndexOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?.into();
    let len = obj.get_length_prop(ctx)?;
    let target = cc.arg(0);

    let mut res: i64 = -1;

    if len > 0 {
        let cnt = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i64_clamp(-1, len - 1, len)
        } else {
            len - 1
        };

        for n in (0..=cnt).rev() {
            let val = obj.get_property_i64(ctx, n)?;
            if target.strict_eq(val) {
                res = n;
                break;
            }
        }
    }

    Ok(JSValue::from(res))
}

fn js_list_includes(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let length = obj.get_length_prop(ctx)?;
    let target = cc.arg(0);

    if length > 0 {
        let start = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i64_clamp(0, length, length)
        } else {
            0
        };

        for i in start..length {
            let val = obj.get_property_i64(ctx, i)?;
            if target.strict_eq(val) {
                return Ok(JSValue::Bool(true));
            }
        }
    }

    Ok(JSValue::Bool(false))
}

pub(crate) fn js_list_join(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let to_locale_string = cc.magic() == 1;

    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let mut c = b',' as i32;
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
            if c >= 0 {
                let _ = sbuf.push_u8(c as u8);
            } else {
                let _ = sbuf.push_content(p.as_ref().unwrap().content());
            }
        }

        let el = obj.get_property_i64(ctx, i)?;
        if !el.is_undefined_or_null() {
            let ss: StringRef = if to_locale_string {
                el.to_locale_string(ctx)?.to_string(ctx)?
            } else if let JSValue::String(s) = el {
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

    ctx.create_string_with_buf(sbuf).map(JSValue::from)
}

pub(crate) fn js_list_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    obj.invoke_callable(ctx, Atom::CONST_join, &[])?
        .unwrap_or_else(|| {
            js_object_toString(ctx, CallContext::new_simple(obj.into(), &[]))
        })
}

pub(crate) fn js_list_pop(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let shift = cc.magic() == 1;

    let mut this = cc.this().objectify(ctx)?;
    let length = this.get_length_prop(ctx)?;

    let mut res = JSValue::UNDEFINED;
    let mut new_len = 0;

    if length > 0 {
        new_len = length - 1;

        if this.class_id() == JS_CLASS_LIST && this.is_fast_array() {
            if let Some(arr_data) = this.as_List_mut().map(|a| a.payload_mut()) {
                if shift {
                    let el = arr_data.remove(0);
                    res = el.to_value(ctx)?;
                    // el was removed from ListPayload (traced by GcTrace). After
                    // to_value extracts the JSValue, protect its GC nodes.
                    ctx.gc_protect(&res);
                } else {
                    let el = arr_data.pop().unwrap();
                    let val = el.to_value(ctx)?;
                    ctx.gc_protect(&val);
                    res = val;
                }
                this.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::from(new_len as u32)));
                return Ok(res);
            }
        }

        // slow path
        let val = if shift {
            let v = this.get_property_i64(ctx, 0)?;
            for i in 0..new_len {
                let next = this.get_property_i64(ctx, i + 1)?;
                this.set_property_i64(ctx, i, next)?;
            }
            v
        } else {
            this.get_property_i64(ctx, new_len)?
        };
        this.delete_property_i64(ctx, new_len, JS_PROP_THROW)?;
        res = val;
    }

    this.set_property(ctx, Atom::CONST_length, JSValue::from(new_len))?;
    Ok(res)
}

pub(crate) fn js_list_push(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let unshift = cc.magic();

    let mut obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let new_len = len + cc.length() as i64;
    if new_len > MAX_SAFE_INTEGER {
        ctx.throw_type_error("List too long");
        return Err(-1);
    }

    if unshift != 0 && cc.length() > 0 {
        // shift existing elements
        for i in (0..len).rev() {
            let v = obj.get_property_i64(ctx, i)?;
            obj.set_property_i64(ctx, i + cc.length() as i64, v)?;
        }
        for i in 0..cc.length() {
            obj.set_property_i64(ctx, i as i64, cc.arg(i))?;
        }
    } else {
        for i in 0..cc.length() {
            obj.set_property_i64(ctx, len + i as i64, cc.arg(i))?;
        }
    }

    obj.set_property(ctx, Atom::CONST_length, JSValue::from(new_len))?;
    Ok(JSValue::from(new_len))
}

fn js_list_reverse(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this().objectify(ctx)?;
    let len = this.get_length_prop(ctx)?;

    if len <= 1 {
        return Ok(this.into());
    }

    if this.class_id() == JS_CLASS_LIST && this.is_fast_array() {
        if let Some(arr_data) = this.as_List_mut().map(|a| a.payload_mut()) {
            arr_data.reverse();
            return Ok(this.into());
        }
    }

    // slow path
    let mut l = 0;
    let mut h = len - 1;
    while l < h {
        let lval = this.get_property_i64(ctx, l)?;
        let hval = this.get_property_i64(ctx, h)?;
        this.set_property_i64(ctx, l, hval)?;
        this.set_property_i64(ctx, h, lval)?;
        l += 1;
        h -= 1;
    }

    Ok(this.into())
}

fn js_list_toReversed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this().objectify(ctx)?;
    let len = {
        let n = this.get_length_prop(ctx)?;
        if n > u32::MAX as i64 {
            ctx.throw_range_error("invalid list length");
            return Err(-1);
        } else {
            n as u32
        }
    };

    let mut new_lst = Vec::new();
    new_lst.try_reserve_exact(len as usize).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    for i in (0..len).rev() {
        let v = this.get_property_i64(ctx, i as i64)?;
        new_lst.push(ListElement::box_from_value(ctx, v)?);
    }

    List::from_values(ctx, new_lst).map(JSValue::from)
}

fn js_list_sort(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
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
    // Use Array::sort slow path via Object interface
    crate::Array::sort(ctx, &mut obj, cmpfn)?;
    Ok(obj.into())
}

fn js_list_toSorted(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
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
        ctx.throw_range_error("invalid list length");
        return Err(-1);
    }

    let mut new_lst = Vec::new();
    new_lst.try_reserve_exact(len as usize).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    for i in 0..len {
        let v = this.get_property_i64(ctx, i)?;
        new_lst.push(ListElement::box_from_value(ctx, v)?);
    }

    let mut list = List::from_values(ctx, new_lst)?;
    crate::Array::sort(ctx, &mut list.into(), cmpfn)?;
    Ok(list.into())
}

pub(crate) fn js_list_slice(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_splice = cc.magic() != 0;

    let mut obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
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
            ctx.throw_type_error("List too long");
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

    // make return List
    let mut res_arr = List::with_capacity(ctx, count as usize)?;
    for k in start..(start + count) {
        let v = obj.get_property_i64(ctx, k)?;
        res_arr.payload_mut().push(ListElement::box_from_value(ctx, v)?)
            .map_err(|_| { ctx.throw_out_of_memory(); -1 })?;
    }
    res_arr.set_length(ctx, count as u32, true)?;

    if is_splice {
        let new_len = len + item_count as i64 - del_count;
        // shift elements
        if item_count as i64 != del_count {
            if item_count as i64 < del_count {
                for i in (start + del_count)..len {
                    let v = obj.get_property_i64(ctx, i)?;
                    obj.set_property_i64(ctx, i - (del_count - item_count as i64), v)?;
                }
            } else {
                for i in ((start + del_count)..len).rev() {
                    let v = obj.get_property_i64(ctx, i)?;
                    obj.set_property_i64(ctx, i + (item_count as i64 - del_count), v)?;
                }
            }
        }

        for i in (new_len..len).rev() {
            obj.delete_property_i64(ctx, i, JS_PROP_THROW)?;
        }

        for i in 0..item_count {
            obj.set_property_i64(ctx, start + i as i64, cc.arg(i as usize + 2))?;
        }

        obj.set_property(ctx, Atom::CONST_length, JSValue::from(new_len))?;
    }

    Ok(res_arr.into())
}

fn js_list_toSpliced(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

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
        ctx.throw_type_error("invalid list length");
        return Err(-1);
    }

    let mut new_lst = Vec::new();
    new_lst.try_reserve_exact(new_len as usize).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    for i in 0..start {
        let v = obj.get_property_i64(ctx, i)?;
        new_lst.push(ListElement::box_from_value(ctx, v)?);
    }

    for i in 0..add {
        new_lst.push(ListElement::box_from_value(ctx, cc.arg((2 + i) as usize))?);
    }

    for i in (start + del)..len {
        let v = obj.get_property_i64(ctx, i)?;
        new_lst.push(ListElement::box_from_value(ctx, v)?);
    }

    List::from_values(ctx, new_lst).map(|a| a.into())
}

fn js_list_copyWithin(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let to: i64 = cc.arg(0).to_number(ctx)?.to_i64_clamp(0, len, len);
    let start: i64 = cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len, len);
    let end = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_number(ctx)?.to_i64_clamp(0, len, len)
    } else {
        len
    };

    let count = std::cmp::min(end - start, len - to);
    if count > 0 {
        let mut vals = Vec::new();
        vals.try_reserve_exact(count as usize).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        for i in 0..count {
            vals.push(obj.get_property_i64(ctx, start + i)?);
        }
        // Protect copied values before writing. vals is a local Vec, not traced
        // by GC — set_property_i64 may allocate and trigger arena GC.
        for v in &vals {
            ctx.gc_protect(&v);
        }
        for (i, v) in vals.drain(..).enumerate() {
            obj.set_property_i64(ctx, to + i as i64, v)?;
        }
    }

    Ok(obj.into())
}

pub(crate) fn js_list_flatten(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let map = cc.magic();

    let source = cc.this().objectify(ctx)?.into();
    let source_len = source.get_length_prop(ctx)?;

    let mut depth = 1;
    let mut mapper_func = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if map != 0 {
        mapper_func = cc.arg(0);
        if cc.length() > 1 {
            this_arg = cc.arg(1);
        }
        mapper_func.ensure_function(ctx)?;
    } else if cc.length() > 0 && !cc.arg(0).is_undefined() {
        depth = cc.arg(0).to_number(ctx)?.to_i32_saturating();
    }

    let arr = List::create(ctx)?;

    JS_FlattenIntoArray(
        ctx,
        arr.into(),
        source,
        source_len,
        0,
        depth as _,
        mapper_func,
        this_arg,
    )?;

    Ok(arr.into())
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
                    ctx.throw_type_error("List too long");
                    return Err(-1);
                }

                target.set_property_i64(ctx, target_index, el)?;

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

pub const js_list_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("at", 1, js_list_at, 0),
    FieldDef::FUNC("with", 2, js_list_with, 0),
    FieldDef::FUNC("concat", 1, js_list_concat, 0),
    FieldDef::FUNC("every", 1, js_list_every, special_every as i16),
    FieldDef::FUNC("some", 1, js_list_every, special_some as i16),
    FieldDef::FUNC("forEach", 1, js_list_every, special_forEach as i16),
    FieldDef::FUNC("map", 1, js_list_every, special_map as i16),
    FieldDef::FUNC("filter", 1, js_list_every, special_filter as i16),
    FieldDef::FUNC("reduce", 1, js_list_reduce, special_reduce as i16),
    FieldDef::FUNC(
        "reduceRight",
        1,
        js_list_reduce,
        special_reduceRight as i16,
    ),
    FieldDef::FUNC("fill", 1, js_list_fill, 0),
    FieldDef::FUNC("find", 1, js_list_find, special_find as i16),
    FieldDef::FUNC("findIndex", 1, js_list_find, special_findIndex as i16),
    FieldDef::FUNC("findLast", 1, js_list_find, special_findLast as i16),
    FieldDef::FUNC(
        "findLastIndex",
        1,
        js_list_find,
        special_findLastIndex as i16,
    ),
    FieldDef::FUNC("indexOf", 1, js_list_indexOf, 0),
    FieldDef::FUNC("lastIndexOf", 1, js_list_lastIndexOf, 0),
    FieldDef::FUNC("includes", 1, js_list_includes, 0),
    FieldDef::FUNC("join", 1, js_list_join, 0),
    FieldDef::FUNC("toString", 0, js_list_toString, 0),
    FieldDef::FUNC("toLocaleString", 0, js_list_join, 1),
    FieldDef::FUNC("pop", 0, js_list_pop, 0),
    FieldDef::FUNC("push", 1, js_list_push, 0),
    FieldDef::FUNC("shift", 0, js_list_pop, 1),
    FieldDef::FUNC("unshift", 1, js_list_push, 1),
    FieldDef::FUNC("reverse", 0, js_list_reverse, 0),
    FieldDef::FUNC("toReversed", 0, js_list_toReversed, 0),
    FieldDef::FUNC("sort", 1, js_list_sort, 0),
    FieldDef::FUNC("toSorted", 1, js_list_toSorted, 0),
    FieldDef::FUNC("slice", 2, js_list_slice, 0),
    FieldDef::FUNC("splice", 2, js_list_slice, 1),
    FieldDef::FUNC("toSpliced", 2, js_list_toSpliced, 0),
    FieldDef::FUNC("copyWithin", 2, js_list_copyWithin, 0),
    FieldDef::FUNC("flatMap", 1, js_list_flatten, 1),
    FieldDef::FUNC("flat", 0, js_list_flatten, 0),
    FieldDef::FUNC("values", 0, js_list_values, 0),
    FieldDef::ALIAS("[Symbol.iterator]", "values"),
    FieldDef::FUNC("keys", 0, js_list_keys, 0),
    FieldDef::FUNC("entries", 0, js_list_entries, 0),
];

fn js_list_values(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    let iter = ListIterator::create(ctx, obj.into(), len as u32, JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE)?;
    Ok(iter.into())
}

fn js_list_keys(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    let iter = ListIterator::create(ctx, obj.into(), len as u32, JSIteratorKindEnum::JS_ITERATOR_KIND_KEY)?;
    Ok(iter.into())
}

fn js_list_entries(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    let iter = ListIterator::create(ctx, obj.into(), len as u32, JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE)?;
    Ok(iter.into())
}

use crate::builtins::iterator::{JSIteratorKindEnum, ObjectIter};

#[derive(Debug)]
pub struct ListIterator {
    obj: JSValue,
    index: u32,
    len: u32,
    kind: JSIteratorKindEnum,
}

impl ListIterator {
    pub fn create(ctx: &mut ScopedContext, obj: JSValue, len: u32, kind: JSIteratorKindEnum) -> Result<Object, i32> {
        let payload = crate::builtins::iterator::ArrayIterPayload::new(obj, 0, len, kind);
        let iter = ctx.create_instance_with_payload(ObjectPayload::ARRAY_ITERATOR(payload)).map_err(|_| -1)?;
        Ok(iter)
    }
}

// Note: List iterator reuses Array iterator payload and next function.

fn js_list_from(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let items = cc.arg(0);
    let mut mapping = false;
    let mut mapfn = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if cc.length() > 1 {
        mapfn = cc.arg(1);
        if !mapfn.is_undefined() {
            mapfn.ensure_function(ctx)?;
            mapping = true;
            if cc.length() > 2 {
                this_arg = cc.arg(2);
            }
        }
    }

    let mut k: i64 = 0;
    let mut res = JSValue::UNDEFINED;

    let has_iterator = {
        #[cfg(feature = "builtin_symbol")]
        {
            let it = items.get_property(ctx, Atom::CONST_Symbol_iterator)?;
            let has = !it.is_undefined();

            if has {
                let mut arr = if cc.this().is_constructor() {
                    cc.this().call_ctor(ctx, &[])?
                } else {
                    List::create(ctx)?.into()
                };

                let iter = items.get_iterator_must(ctx, false)?;
                let it = ObjectIter::new(ctx, iter, Atom::CONST_next)?;

                for el in it {
                    let mut val = el?;
                    if mapping {
                        val = mapfn.call(ctx, this_arg, &[val, JSValue::Int(k as i32)])?;
                    }
                    arr.set_property_i64(ctx, k, val)?;
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
            cc.this().call_ctor(ctx, std::slice::from_ref(&len_val))?
        } else {
            List::with_capacity(ctx, len as usize)?.into()
        };

        for i in 0..len {
            let mut el = src.get_property_i64(ctx, i)?;
            if mapping {
                el = mapfn.call(ctx, this_arg, &[el, JSValue::Int(i as i32)])?;
            }
            arr.set_property_i64(ctx, i, el)?;
        }

        k = len;
        res = arr;
    }

    res.set_property(ctx, Atom::CONST_length, JSValue::from(k as u32))?;
    Ok(res)
}

fn js_list_of(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let mut arr = if this.is_constructor() {
        this.call_ctor(ctx, std::slice::from_ref(&JSValue::Int(cc.length() as i32)))?
    } else {
        List::create(ctx)?.into()
    };

    let cnt = cc.length();
    for i in 0..cnt {
        arr.set_property_i64(ctx, i as i64, cc.arg(i))?;
    }

    arr.set_property(ctx, Atom::CONST_length, JSValue::from(cnt as u32))?;
    Ok(arr)
}

fn js_list_isList(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    Ok(JSValue::Bool(
        cc.arg(0).as_object().is_some_and(|o| o.class_id() == JS_CLASS_LIST),
    ))
}

pub const js_list_funcs: &[FieldDef] = &[
    FieldDef::FUNC("isList", 1, js_list_isList, 0),
    FieldDef::FUNC("from", 1, js_list_from, 0),
    FieldDef::FUNC("of", 0, js_list_of, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

use crate::object::ObjectPayload;
