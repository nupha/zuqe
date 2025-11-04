use std::cell::UnsafeCell;

use crate::{
    JS_CLASS_ARRAY_ITERATOR, JS_CLASS_STRING_ITERATOR,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, IteratorState},
    object::ObjectPayload,
    runtime::JSRuntime,
    typed_array::TypedArray,
    value::JSValue,
};

use super::JSIteratorKindEnum;

/// iterate state for Array, String or TypedArray
#[derive(Debug)]
pub struct ArrayIterPayload<'a> {
    /// target value
    val: UnsafeCell<JSValue<'a>>,
    /// current index
    idx: UnsafeCell<u32>,
    kind: JSIteratorKindEnum,
}

impl<'a> ArrayIterPayload<'a> {
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
    pub(crate) const fn value(&self) -> &JSValue<'a> {
        unsafe { self.val.get().as_ref().unwrap_unchecked() }
    }

    /// set iterator target value
    pub(crate) fn set_value(&self, obj: JSValue<'a>) {
        unsafe {
            *self.val.get() = obj;
        }
    }

    pub const fn class_finalizer(_rt: &mut JSRuntime, _isa: &ObjectPayload, _id: usize) {}

    pub fn class_mark(val: JSValue) {
        let p = unsafe { val.as_object_unchecked() };
        if let ObjectPayload::ARRAY_ITERATOR(it) | ObjectPayload::STRING_ITERATOR(it) = p.payload()
        {
            it.value().gc_mark();
        }
    }
}

/// 用于创建一个数组迭代器对象，该对象可以用来遍历数组中的元素。
/// 通常被用于实现数组的 keys、values 和 entries 方法
pub fn js_create_array_iterator<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 & 3) };
    let (class_id, obj) = if cc.magic() & 4 != 0 {
        (
            JS_CLASS_STRING_ITERATOR,
            JSValue::from(cc.this().to_string_non_undefined_or_null(ctx)?),
        )
    } else {
        (
            JS_CLASS_ARRAY_ITERATOR,
            JSValue::from(cc.this().clone().objectify(ctx)?),
        )
    };

    let isa = Box::new(ArrayIterPayload {
        val: UnsafeCell::new(obj),
        idx: UnsafeCell::new(0),
        kind,
    });
    ctx.create_object(if class_id == JS_CLASS_STRING_ITERATOR {
        ObjectPayload::STRING_ITERATOR(isa)
    } else {
        ObjectPayload::ARRAY_ITERATOR(isa)
    })
    .map(JSValue::Object)
    .map_err(|_| -1)
}

pub fn js_array_iterator_next<'a>(
    ctx: &JSContext<'a>,
    cc: CallContext<'_, 'a>,
    state: &mut IteratorState,
) -> JSResult<'a> {
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
        let len = if let Ok(tao) = TypedArray::try_from(obj.clone()) {
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
