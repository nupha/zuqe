use crate::{
    array::js_create_array,
    context::JSContext,
    error::JS_ThrowTypeErrorDetachedArrayBuffer,
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::js_malloc_typed,
    runtime::JSRuntime,
    typed_array::TypedArrayValue,
    value::{
        object::{js_get_length32, JS_ToObject},
        JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_ToStringCheckObject,
    },
    JS_CLASS_ARRAY_ITERATOR, JS_CLASS_C_FUNCTION, JS_CLASS_FOR_IN_ITERATOR,
    JS_CLASS_STRING_ITERATOR,
};

use super::JSIteratorKindEnum;

pub struct JSArrayIteratorData {
    pub obj: JSValue,
    pub kind: JSIteratorKindEnum,
    pub idx: u32,
}

impl JSArrayIteratorData {
    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        let it = val.object_unchecked().u.array_iterator_data;
        if !it.is_null() {
            JS_FreeValueRT(rt, (*it).obj);
            drop(Box::from_raw(it));
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        let it = val.object_unchecked().u.array_iterator_data;
        if !it.is_null() {
            JS_MarkValue(rt, (*it).obj, mark_func);
        }
    }
}

/// 用于创建一个数组迭代器对象，该对象可以用来遍历数组中的元素。
/// 通常被用于实现数组的 keys、values 和 entries 方法
pub unsafe fn js_create_array_iterator(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let kind = std::mem::transmute::<u8, JSIteratorKindEnum>(magic as u8 & 3);
    let (class_id, arr) = if magic & 4 != 0 {
        // string iterator case
        (
            JS_CLASS_STRING_ITERATOR,
            JS_ToStringCheckObject(ctx, this_val),
        )
    } else {
        (JS_CLASS_ARRAY_ITERATOR, JS_ToObject(ctx, this_val))
    };
    if arr.is_exception() {
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    }

    let it = if let Some(p) = js_malloc_typed::<JSArrayIteratorData>(ctx, 1, false) {
        p.write(JSArrayIteratorData {
            obj: arr,
            kind,
            idx: 0,
        });
        Box::from_raw(p.as_ptr())
    } else {
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    };

    let enum_obj = if let Ok(o) = (*ctx).create_object_with_classid(class_id) {
        o
    } else {
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    };

    enum_obj
        .object_unchecked()
        .set_opaque(Box::into_raw(it).cast());

    enum_obj
}

pub unsafe fn js_array_iterator_next(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    pdone: *mut i32,
    magic: i32,
) -> JSValue {
    macro_rules! fail {
        () => {
            *pdone = 0;
            return JSValue::EXCEPTION;
        };
    }
    macro_rules! done {
        () => {
            *pdone = 1;
            return JSValue::UNDEFINED;
        };
    }

    if let Ok(mut p) = this_val
        .object_unchecked()
        .opaque_must::<JSArrayIteratorData>(JS_CLASS_ARRAY_ITERATOR, ctx.as_mut().unwrap())
    {
        let it = p.as_mut();

        if it.obj.is_undefined() {
            done!();
        }

        // eval array length
        let len = if let Ok(tao) = TryInto::<TypedArrayValue>::try_into(it.obj) {
            // `it.obj` is Uint8ClampArray..Float64Array object
            if tao.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                fail!();
            }
            tao.length() as u32
        } else {
            // is ArrayIterator or StringIterator object
            if let Ok(n) = js_get_length32(ctx, it.obj) {
                n
            } else {
                fail!();
            }
        };

        let idx = it.idx;
        if idx >= len {
            JS_FreeValue(ctx, it.obj);
            it.obj = JSValue::UNDEFINED;
            done!();
        }

        it.idx = idx + 1;
        *pdone = 0;

        match it.kind {
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => JSValue::from(idx),
            JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                let val = it.obj.get_property_u32(ctx.as_mut().unwrap(), idx);
                if val.is_exception() {
                    JSValue::EXCEPTION
                } else {
                    val
                }
            }
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                let val = it.obj.get_property_u32(ctx.as_mut().unwrap(), idx);
                if val.is_exception() {
                    return JSValue::EXCEPTION;
                }
                let num = JSValue::from_u32(idx);
                let args = [num, val];
                let obj = js_create_array(ctx, 2, args.as_ptr());
                JS_FreeValue(ctx, val);
                JS_FreeValue(ctx, num);
                obj
            }
        }
    } else {
        fail!();
    }
}
