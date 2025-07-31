use std::ops::{Deref, DerefMut};

use crate::{
    JS_CLASS_ARRAY_ITERATOR, JS_CLASS_STRING_ITERATOR,
    array::js_create_array,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, IteratorState},
    object::{ObjectDetail, js_get_length32},
    runtime::JSRuntime,
    string::JS_ToStringCheckObject,
    typed_array::TypedArray,
    value::JSValue,
};

use super::JSIteratorKindEnum;

#[derive(Debug)]
pub struct JSArrayIteratorData {
    pub obj: JSValue,
    pub kind: JSIteratorKindEnum,
    pub idx: u32,
}

impl JSArrayIteratorData {
    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {}

    pub fn hook_mark(val: JSValue) {
        let p = unsafe { val.object_unchecked() };
        if let ObjectDetail::ARRAY_ITERATOR(it) | ObjectDetail::STRING_ITERATOR(it) =
            p.0.payload.borrow().deref()
        {
            it.obj.gc_mark();
        }
    }
}

/// 用于创建一个数组迭代器对象，该对象可以用来遍历数组中的元素。
/// 通常被用于实现数组的 keys、values 和 entries 方法
pub fn js_create_array_iterator(ctx: &JSContext, cc: CallContext) -> JSResult {
    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 & 3) };

    let (class_id, arr) = if cc.magic() & 4 != 0 {
        // string iterator case
        (
            JS_CLASS_STRING_ITERATOR,
            JS_ToStringCheckObject(ctx, cc.this().clone())?,
        )
    } else {
        (
            JS_CLASS_ARRAY_ITERATOR,
            cc.this().coerce_to_object(ctx)?.into(),
        )
    };

    let it = Box::new(JSArrayIteratorData {
        obj: arr,
        kind,
        idx: 0,
    });
    ctx.create_object(if class_id == JS_CLASS_STRING_ITERATOR {
        ObjectDetail::STRING_ITERATOR(it)
    } else {
        ObjectDetail::ARRAY_ITERATOR(it)
    })
    .map(JSValue::Object)
    .map_err(|_| -1)
}

pub fn js_array_iterator_next(
    ctx: &JSContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let mut isa = unsafe { cc.this().object_unchecked().0.payload.borrow_mut() };

    if let ObjectDetail::ARRAY_ITERATOR(it) = isa.deref_mut() {
        if it.obj.is_undefined() {
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        // eval array length
        let len = if let Ok(tao) = TryInto::<TypedArray>::try_into(it.obj.clone()) {
            // `it.obj` is Uint8ClampArray..Float64Array object
            if tao.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                return Err(-1);
            }
            tao.length() as u32
        } else {
            // is ArrayIterator or StringIterator object
            js_get_length32(ctx, &it.obj)?
        };

        let idx = it.idx;
        if idx >= len {
            it.obj = JSValue::UNDEFINED;
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        *state = IteratorState::HasMore;
        it.idx = idx + 1;

        match it.kind {
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => Ok(JSValue::from(idx)),
            JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => it.obj.get_property_u32(ctx, idx),
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                let val = it.obj.get_property_u32(ctx, idx)?;
                let num = JSValue::from(idx);
                unsafe { js_create_array(ctx, 2, [num, val].as_ptr()).check_exception() }
            }
        }
    } else {
        Err(-1)
    }
}
