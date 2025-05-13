use crate::{
    array::{js_create_array, JS_NewArray},
    atom::{consts::*, JSAtom, JS_FreeAtomRT},
    context::JSContext,
    error::JS_ThrowTypeErrorDetachedArrayBuffer,
    function::{js_poll_interrupts, JSCFunctionEnum, JS_Call, JS_CallFree},
    gc::{JS_MarkFunc, JS_MarkValue},
    map::JSMapRecord,
    mem::{js_malloc, js_malloc_typed},
    runtime::JSRuntime,
    typed_array::TypedArrayValue,
    value::{
        object::{
            js_get_length32, JS_GetPrototypeFree, JS_NewObject, JS_ToObject, JS_ToObjectFree,
            PropertyEnum, JS_GPN_ENUM_ONLY, JS_GPN_SET_ENUM, JS_GPN_STRING_MASK, JS_PROP_C_W_E,
            JS_PROP_ENUMERABLE, JS_PROP_THROW,
        },
        JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_ToBoolFree, JS_ToStringCheckObject,
        JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_UNDEFINED, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARRAY_ITERATOR, JS_CLASS_C_FUNCTION, JS_CLASS_FOR_IN_ITERATOR,
    JS_CLASS_STRING_ITERATOR,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSIteratorKindEnum {
    JS_ITERATOR_KIND_KEY,
    JS_ITERATOR_KIND_VALUE,
    JS_ITERATOR_KIND_KEY_AND_VALUE,
}

pub struct JSArrayIteratorData {
    pub obj: JSValue,
    pub kind: JSIteratorKindEnum,
    pub idx: u32,
}

pub struct JSMapIteratorData {
    pub obj: JSValue,
    pub kind: JSIteratorKindEnum,
    pub cur_record: *mut JSMapRecord,
}

pub struct JSForInIterator {
    pub obj: JSValue,
    pub idx: u32,
    pub atom_count: u32,
    pub in_prototype_chain: bool,
    pub is_array: bool,
    pub tab_atom: Box<[PropertyEnum]>,
}

/// create an Object with two properties: `value` and `done`.
/// Note: `val` is consumed.
pub unsafe fn js_create_iterator_result(ctx: &mut JSContext, val: JSValue, done: bool) -> JSValue {
    let obj = JS_NewObject(ctx);
    if obj.is_exception() {
        JS_FreeValue(ctx, val);
        return obj;
    }

    if obj
        .define_property_value(ctx, JS_ATOM_value, val, JS_PROP_C_W_E)
        .is_err()
    {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    if obj
        .define_property_value(
            ctx,
            JS_ATOM_done,
            if done { JSValue::TRUE } else { JSValue::FALSE },
            JS_PROP_C_W_E,
        )
        .is_err()
    {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    obj
}

pub unsafe fn JS_GetIterator2(
    ctx: *mut JSContext,
    obj: JSValueConst,
    method: JSValueConst,
) -> JSValue {
    let enum_obj = JS_Call(ctx.as_mut().unwrap(), method, obj, 0, std::ptr::null_mut());
    if enum_obj.is_exception() {
        return enum_obj;
    }

    if !enum_obj.is_object() {
        JS_FreeValue(ctx, enum_obj);
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    return enum_obj;
}

pub unsafe fn JS_GetIterator(ctx: *mut JSContext, obj: JSValueConst, is_async: bool) -> JSValue {
    let method = if is_async {
        let mut val = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_asyncIterator);
        if val.is_exception() {
            return JSValue::EXCEPTION;
        }

        if val.is_undefined_or_null() {
            val = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_iterator);
            if val.is_exception() {
                return val;
            }

            let sync_iter = JS_GetIterator2(ctx, obj, val);
            JS_FreeValue(ctx, val);
            if sync_iter.is_exception() {
                return JSValue::EXCEPTION;
            }

            let ret = (*ctx)
                .create_async_from_sync_iterator(sync_iter)
                .unwrap_or(JSValue::EXCEPTION);

            JS_FreeValue(ctx, sync_iter);
            return ret;
        }

        val
    } else {
        let v = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_iterator);
        if v.is_exception() {
            return JSValue::EXCEPTION;
        }
        v
    };

    if !method.is_callable(ctx.as_ref().unwrap()) {
        JS_FreeValue(ctx, method);
        return (*ctx).JS_ThrowTypeError(c"value is not iterable");
    }

    let ret = JS_GetIterator2(ctx, obj, method);
    JS_FreeValue(ctx, method);

    ret
}

/// return *pdone: 0-not done, 1-done, 2-the iterator object is not parsed
pub unsafe fn JS_IteratorNext2(
    ctx: *mut JSContext,
    enum_obj: JSValueConst,
    method: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    pdone: *mut i32,
) -> JSValue {
    // fast path for the built-in iterators (avoid creating the
    // intermediate result object)
    if method.is_object() {
        let p = method.object_unchecked();
        if p.class_id() == JS_CLASS_C_FUNCTION && p.u.cfunc.cproto == JSCFunctionEnum::IteratorNext
        {
            let ret = (p.u.cfunc.c_function.iterator_next)(
                ctx,
                enum_obj,
                argc,
                if argc == 0 {
                    [JSValue::UNDEFINED].as_mut_ptr()
                } else {
                    argv
                },
                pdone,
                (*p).u.cfunc.magic as _,
            );
            return ret;
        }
    }

    let obj = JS_Call(ctx.as_mut().unwrap(), method, enum_obj, argc, argv);
    if obj.is_exception() {
        *pdone = 0;
        return JSValue::EXCEPTION;
    }
    if !obj.is_object() {
        JS_FreeValue(ctx, obj);
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"iterator must return an object");

        *pdone = 0;
        return JSValue::EXCEPTION;
    }

    *pdone = 2;
    obj
}

pub unsafe fn JS_IteratorNext(
    ctx: *mut JSContext,
    enum_obj: JSValueConst,
    method: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    pdone: *mut bool,
) -> JSValue {
    let mut done: i32 = 0;
    let obj = JS_IteratorNext2(ctx, enum_obj, method, argc, argv, &mut done);
    if obj.is_exception() {
        *pdone = false;
        return JSValue::EXCEPTION;
    }

    if done != 2 {
        *pdone = done != 0;
        return obj;
    } else {
        let done_val = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_done);
        if done_val.is_exception() {
            JS_FreeValue(ctx, obj);
            *pdone = false;
            return JSValue::EXCEPTION;
        }
        *pdone = JS_ToBoolFree(ctx, done_val).unwrap_or(true);

        let value = if !*pdone {
            obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_value)
        } else {
            JSValue::UNDEFINED
        };
        JS_FreeValue(ctx, obj);
        return value;
    }
}

pub unsafe fn JS_IteratorClose(
    ctx: *mut JSContext,
    enum_obj: JSValueConst,
    is_exception_pending: bool,
) -> Result<(), i32> {
    let (exc_obj, mut res) = if is_exception_pending {
        let exc = (*ctx).rt_mut().current_exception.replace(JSValue::NULL);
        (exc, -1)
    } else {
        (JSValue::UNDEFINED, 0)
    };

    let method = enum_obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_return);
    if method.is_exception() {
        res = -1;
    } else {
        if !method.is_undefined_or_null() {
            let ret = JS_CallFree(
                ctx.as_mut().unwrap(),
                method,
                enum_obj,
                0,
                std::ptr::null_mut(),
            );
            if !is_exception_pending {
                if ret.is_exception() {
                    res = -1;
                } else if !ret.is_object() {
                    ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
                    res = -1;
                }
            }
            JS_FreeValue(ctx, ret);
        }
    }

    if is_exception_pending {
        ctx.as_mut().unwrap().JS_Throw(exc_obj);
    }

    if res == -1 {
        Err(-1)
    } else {
        Ok(())
    }
}

pub unsafe fn JS_IteratorGetCompleteValue(
    ctx: *mut JSContext,
    obj: JSValueConst,
    pdone: *mut bool,
) -> JSValue {
    let done_val = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_done);
    if done_val.is_exception() {
        *pdone = false;
        return JSValue::EXCEPTION;
    }
    let done = JS_ToBoolFree(ctx, done_val).unwrap();

    let value = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_value);
    if value.is_exception() {
        *pdone = false;
        return JSValue::EXCEPTION;
    }
    *pdone = done;
    value
}

pub unsafe fn js_for_in_iterator_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let it = val.object_unchecked().u.for_in_iterator;

    JS_FreeValueRT(rt, (*it).obj);
    if !(*it).is_array {
        for x in (*it).tab_atom.iter() {
            JS_FreeAtomRT(rt, x.atom);
        }
    }

    drop(Box::from_raw(it));
}

pub unsafe fn js_for_in_iterator_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    let it = val.object_unchecked().u.for_in_iterator;
    JS_MarkValue(rt, (*it).obj, mark_func);
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

    let it =
        js_malloc(ctx, std::mem::size_of::<JSArrayIteratorData>()).cast::<JSArrayIteratorData>();
    if it.is_null() {
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    }
    (*it).obj = arr;
    (*it).kind = kind;
    (*it).idx = 0;

    let enum_obj = if let Ok(o) = (*ctx).create_object_with_classid(class_id) {
        o
    } else {
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    };

    enum_obj.object_unchecked().set_opaque(it.cast());

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

        let len = if let Ok(tao) = TryInto::<TypedArrayValue>::try_into(it.obj) {
            if tao.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                fail!();
            }
            tao.length() as u32
        } else {
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

/// `obj` is consumed.
pub unsafe fn build_for_in_iterator(ctx: *mut JSContext, obj: JSValue) -> JSValue {
    let tag = JS_VALUE_GET_TAG(obj);
    let obj = if !matches!(tag, JS_TAG_OBJECT | JS_TAG_NULL | JS_TAG_UNDEFINED) {
        JS_ToObjectFree(ctx, obj)
    } else {
        obj
    };

    let mut it = if let Some(p) = js_malloc_typed::<JSForInIterator>(ctx, 1, false) {
        Box::from_raw(p.as_ptr())
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let enum_obj = if let Ok(o) =
        (*ctx).create_object_with_proto_and_classid(JSValue::NULL, JS_CLASS_FOR_IN_ITERATOR)
    {
        o
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    it.is_array = false;
    it.obj = obj;
    it.idx = 0;
    it.atom_count = 0;
    it.tab_atom = Box::new([]);
    it.in_prototype_chain = false;

    if tag == JS_TAG_OBJECT {
        let ob = obj.object_unchecked();
        if ob.detailed.fast_array() {
            // check that there are no enumerable normal fields
            if !ob
                .shape()
                .unwrap()
                .props
                .iter()
                .any(|(_, prs)| prs.flags() & JS_PROP_ENUMERABLE != 0)
            {
                // no enumerable property, is fast array.
                // we only store the number of elements
                it.is_array = true;
                it.atom_count = ob.array_length_unchecked() as u32;
            }
        }

        if !it.is_array {
            // not an array - get own property names list
            if let Ok(names) = ob.get_own_property_names_internal(
                ctx.as_mut().unwrap(),
                JS_GPN_STRING_MASK | JS_GPN_SET_ENUM,
            ) {
                it.atom_count = names.len() as _;
                it.tab_atom = names;
            } else {
                JS_FreeValue(ctx, enum_obj);
                return JSValue::EXCEPTION;
            }
        }
    }

    // attach JSForInIterator to enum object
    enum_obj.object_unchecked().u.for_in_iterator = Box::into_raw(it);

    enum_obj
}

// return -1 if exception, 0 if slow case, 1 if the enumeration is finished
pub unsafe fn js_for_in_prepare_prototype_chain_enum(
    ctx: *mut JSContext,
    enum_obj: JSValueConst,
) -> i32 {
    let it = enum_obj.object_unchecked().u.for_in_iterator;

    // check if there are enumerable properties in the prototype chain (fast path)
    let mut obj1 = (*it).obj.dup();

    loop {
        obj1 = JS_GetPrototypeFree(ctx, obj1);

        if obj1.is_null() {
            break;
        } else if obj1.is_exception() {
            JS_FreeValue(ctx, obj1);
            return -1;
        }

        // count own property names
        let mut props_count: u32 = 0;
        let r = obj1.object_unchecked().own_property_names_foreach(
            ctx.as_mut().unwrap(),
            JS_GPN_STRING_MASK | JS_GPN_ENUM_ONLY,
            |_, _| {
                props_count += 1;
                Ok(())
            },
        );
        if r.is_err() {
            JS_FreeValue(ctx, obj1);
            return -1;
        }

        if props_count != 0 {
            JS_FreeValue(ctx, obj1);
            break;
        }

        // must check for timeout to avoid infinite loop
        if js_poll_interrupts(ctx.as_mut().unwrap()) != 0 {
            JS_FreeValue(ctx, obj1);
            return -1;
        }
    } // loop

    JS_FreeValue(ctx, obj1);

    // slow_path
    if (*it).is_array {
        if let Ok(names) = (*it)
            .obj
            .object_unchecked()
            .get_own_property_names_internal(
                ctx.as_mut().unwrap(),
                JS_GPN_STRING_MASK | JS_GPN_SET_ENUM,
            )
        {
            (*it).is_array = false;
            (*it).atom_count = names.len() as _;
            (*it).tab_atom = names;
        } else {
            return -1;
        }
    }

    for name in (*it).tab_atom.iter() {
        if enum_obj
            .define_property_value(
                ctx.as_mut().unwrap(),
                name.atom,
                JSValue::NULL,
                JS_PROP_ENUMERABLE,
            )
            .is_err()
        {
            return -1;
        }
    }

    0
}

pub unsafe fn iterator_to_array(ctx: *mut JSContext, items: JSValueConst) -> JSValue {
    let iter = JS_GetIterator(ctx, items, false);
    if iter.is_exception() {
        return JSValue::EXCEPTION;
    }

    let next_method = iter.get_property(ctx.as_mut().unwrap(), JS_ATOM_next);
    if next_method.is_exception() {
        JS_FreeValue(ctx, iter);
        return JSValue::EXCEPTION;
    }

    let r = JS_NewArray(ctx);
    if r.is_exception() {
        JS_FreeValue(ctx, next_method);
        JS_FreeValue(ctx, iter);
        return JSValue::EXCEPTION;
    }

    let mut k = 0;
    loop {
        let mut done = false;
        let v = JS_IteratorNext(ctx, iter, next_method, 0, std::ptr::null_mut(), &mut done);
        if v.is_exception() {
            JS_IteratorClose(ctx, iter, true);
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, r);
            return JSValue::EXCEPTION;
        }

        if done {
            break;
        }

        if r.define_property_value(
            ctx.as_mut().unwrap(),
            JSAtom(k),
            v,
            JS_PROP_C_W_E | JS_PROP_THROW,
        )
        .is_err()
        {
            JS_IteratorClose(ctx, iter, true);
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, r);
            return JSValue::EXCEPTION;
        }

        k += 1;
    }

    JS_FreeValue(ctx, next_method);
    JS_FreeValue(ctx, iter);
    r
}

pub unsafe fn js_array_from_iterator(
    ctx: *mut JSContext,
    plen: *mut u32,
    obj: JSValueConst,
    method: JSValueConst,
) -> JSValue {
    *plen = 0;

    let arr = JS_NewArray(ctx);
    if arr.is_exception() {
        return arr;
    }

    let iter = JS_GetIterator2(ctx, obj, method);
    if iter.is_exception() {
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    }

    let next_method = iter.get_property(ctx.as_mut().unwrap(), JS_ATOM_next);
    if next_method.is_exception() {
        JS_FreeValue(ctx, iter);
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    }

    let mut k = 0;

    loop {
        let mut done = false;
        let val = JS_IteratorNext(ctx, iter, next_method, 0, std::ptr::null_mut(), &mut done);
        if val.is_exception() {
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, arr);
            return JSValue::EXCEPTION;
        }

        if done {
            JS_FreeValue(ctx, val);
            break;
        }

        if arr
            .create_data_property_u32(ctx.as_mut().unwrap(), k, val, JS_PROP_THROW)
            .is_err()
        {
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, arr);
            return JSValue::EXCEPTION;
        }

        k += 1;
    }

    JS_FreeValue(ctx, next_method);
    JS_FreeValue(ctx, iter);

    *plen = k as u32;
    arr
}
