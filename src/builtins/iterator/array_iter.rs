// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::UnsafeCell;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_ARRAY_ITERATOR, JS_CLASS_STRING_ITERATOR, ScopedContext,
    builtins::typed_array::TypedArray,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, IteratorState},
    object::ObjectPayload,
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
            JSValue::from(this.to_string_non_undefined_or_null(ctx)?),
        )
    } else {
        (JS_CLASS_ARRAY_ITERATOR, JSValue::from(this.objectify(ctx)?))
    };

    let isa = Box::new(ArrayIterPayload {
        val: UnsafeCell::new(obj),
        idx: UnsafeCell::new(0),
        kind,
    });
    ctx.create_instance_with_payload(if class_id == JS_CLASS_STRING_ITERATOR {
        ObjectPayload::STRING_ITERATOR(isa)
    } else {
        ObjectPayload::ARRAY_ITERATOR(isa)
    })
    .map(JSValue::Object)
    .map_err(|_| -1)
}

pub(crate) fn js_array_iterator_next(
    ctx: &mut ScopedContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    if let ObjectPayload::ARRAY_ITERATOR(it) = cc
        .this()
        .retrive_object_payload(ctx, JS_CLASS_ARRAY_ITERATOR)?
    {
        let obj = it.value();
        if obj.is_undefined() {
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        // eval array length
        let len = if let Ok(tao) = TypedArray::try_from(obj) {
            // is typed array
            if tao.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                return Err(-1);
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
                ctx.create_array_with_values(vec![num, val])
                    .map(|a| a.into())
            }
        }
    } else {
        unreachable!("")
    }
}
