// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::UnsafeCell;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_ARRAY_ITERATOR, JS_CLASS_STRING_ITERATOR, ScopedContext,
    builtins::typed_array::TypedArray,
    error::JSResult,
    function::{CallContext, IteratorState},
    object::PayloadPtr,
    value::JSValue,
};

use super::JSIteratorKindEnum;

/// iterate state for Array, String or TypedArray
#[derive(Debug)]
pub struct ArrayIterPayload {
    /// target value
    val: UnsafeCell<JSValue>,
    /// current index
    idx: UnsafeCell<u32>,
    kind: JSIteratorKindEnum,
}

impl GcTrace for ArrayIterPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        unsafe {
            (*self.val.get()).trace(tr);
        }
    }
}

impl ArrayIterPayload {
    /// get iterator current index
    #[inline(always)]
    pub(crate) fn index(&self) -> u32 {
        unsafe { *self.idx.get() }
    }

    /// set iterator index
    #[inline(always)]
    pub(crate) fn set_index(&self, i: u32) {
        unsafe {
            *self.idx.get() = i;
        }
    }

    /// increase iterator index by 1
    #[inline(always)]
    pub(crate) fn inc_index(&self) {
        unsafe {
            *self.idx.get() += 1;
        }
    }

    /// get iterator target value
    #[inline(always)]
    pub(crate) const fn value(&self) -> JSValue {
        unsafe { *self.val.get().as_ref().unwrap_unchecked() }
    }

    /// set iterator target value
    pub(crate) fn set_value(&self, obj: JSValue) {
        unsafe {
            *self.val.get() = obj;
        }
    }
}

/// 用于创建一个数组迭代器对象，该对象可以用来遍历数组中的元素。
/// 通常被用于实现数组的 keys、values 和 entries 方法
pub fn js_create_array_iterator(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 & 3) };

    let this = cc.this();
    let (class_id, obj) = if cc.magic() & 4 != 0 {
        (
            JS_CLASS_STRING_ITERATOR,
            JSValue::make_string(this.to_string_non_undefined_or_null(ctx)?),
        )
    } else {
        (JS_CLASS_ARRAY_ITERATOR, JSValue::from(this.objectify(ctx)?))
    };

    ctx.create_instance(PayloadPtr::new(
        class_id,
        ArrayIterPayload {
            val: UnsafeCell::new(obj),
            idx: UnsafeCell::new(0),
            kind,
        },
    ))
    .map(JSValue::from)
}

pub(crate) fn js_array_iterator_next(
    ctx: &mut ScopedContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let this = cc.this();
    let this_obj = this
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_ITERATOR))?;
    let it: &ArrayIterPayload = this_obj
        .payload_array_iterator()
        .ok_or_else(|| ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_ITERATOR))?;

    let obj = it.value();
    if obj.is_undefined() {
        *state = IteratorState::Done;
        return Ok(JSValue::UNDEFINED);
    }

    // eval array length
    let len = if let Ok(tao) = TypedArray::try_from(obj) {
        // is typed array
        tao.ensure_attached(ctx)?;
        // An out-of-bounds view (its resizable buffer was shrunk past its end)
        // throws, matching the spec's typed array iterator step.
        if tao.payload().is_out_of_bounds() {
            return Err(ctx.throw_type_error("out of bounds"));
        }
        tao.length() as u32
    } else {
        // is Array or String
        it.value().get_length_prop(ctx)? as u32
    };

    let idx = it.index();
    if idx >= len {
        it.set_value(JSValue::UNDEFINED);
        *state = IteratorState::Done;
        return Ok(JSValue::UNDEFINED);
    }

    *state = IteratorState::NotDone;
    it.inc_index();

    match it.kind {
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => Ok(JSValue::from(idx)),
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => it.value().get_property_u32(ctx, idx),
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
            let val = it.value().get_property_u32(ctx, idx)?;
            let num = JSValue::from(idx);
            crate::array::Array::from_values(ctx, vec![num, val]).map(|a| a.into())
        }
    }
}
