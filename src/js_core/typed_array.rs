use std::ffi::c_void;

use crate::{
    array_buffer::{js_array_buffer_constructor3, JSArrayBuffer},
    atom::consts::JS_ATOM_Symbol_iterator,
    class::{js_create_from_ctor, JSClassID},
    context::JSContext,
    error::{JS_ThrowRangeError, JS_ThrowTypeErrorDetachedArrayBuffer},
    function::JS_CallConstructor,
    iterator::js_array_from_iterator,
    mem::js_free_rt,
    runtime::JSRuntime,
    typed_array::{
        get_typed_array, typed_array_init, typed_array_is_detached, typed_array_size_log2,
    },
    value::{
        object::{js_get_length64, JS_GetOpaque, JS_GetProperty, JS_SpeciesConstructor},
        JSValue, JSValueConst, JS_FreeValue, JS_IsException, JS_IsNull, JS_IsUndefined, JS_ToIndex,
        JS_ToLengthFree, JS_TAG_OBJECT, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER,
    JS_CLASS_UINT8C_ARRAY,
};

pub unsafe fn js_typed_array___speciesCreate(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = *argv.offset(0);
    let p = get_typed_array(ctx, obj, false);
    if p.is_null() {
        return JSValue::EXCEPTION;
    }

    let ctor = JS_SpeciesConstructor(ctx, obj, JSValue::UNDEFINED);
    if JS_IsException(ctor) {
        return ctor;
    }

    let argc1 = std::cmp::max(argc - 1, 0);
    if JS_IsUndefined(ctor) {
        js_typed_array_constructor(
            ctx,
            JSValue::UNDEFINED,
            argc1,
            argv.offset(1),
            (*p).class_id() as _,
        )
    } else {
        let ret = js_typed_array_create(ctx, ctor, argc1, argv.offset(1));
        JS_FreeValue(ctx, ctor);
        ret
    }
}

pub unsafe fn js_typed_array_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    classid: i32,
) -> JSValue {
    let size_log2 = typed_array_size_log2(classid as _);

    let buffer: JSValue;
    let mut offset: u64 = 0;
    let mut len = 0_u64;

    if JS_VALUE_GET_TAG(*argv.offset(0)) != JS_TAG_OBJECT {
        if JS_ToIndex(ctx, &mut len, *argv.offset(0)) != 0 {
            return JSValue::EXCEPTION;
        }
        buffer = js_array_buffer_constructor1(ctx, JSValue::UNDEFINED, len << size_log2);
        if JS_IsException(buffer) {
            return JSValue::EXCEPTION;
        }
        offset = 0;
    } else {
        let p = JS_VALUE_GET_OBJ(*argv.offset(0));
        if matches!(
            (*p).class_id(),
            JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
        ) {
            let abuf = (*p).u.array_buffer;
            if JS_ToIndex(ctx, &mut offset, *argv.offset(1)) != 0 {
                return JSValue::EXCEPTION;
            }
            if (*abuf).detached {
                return JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            }
            if (offset & ((1 << size_log2) - 1) != 0) || offset > (*abuf).byte_length as u64 {
                return JS_ThrowRangeError(ctx, c"invalid offset");
            }

            if JS_IsUndefined(*argv.offset(2)) {
                if (*abuf).byte_length & ((1 << size_log2) - 1) != 0 {
                    return JS_ThrowRangeError(ctx, c"invalid length");
                }
                len = ((*abuf).byte_length as u64 - offset) >> size_log2;
            } else {
                if JS_ToIndex(ctx, &mut len, *argv.offset(2)) != 0 {
                    return JSValue::EXCEPTION;
                }
                if (*abuf).detached {
                    return JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                }
                if (offset + (len << size_log2)) > (*abuf).byte_length as u64 {
                    return JS_ThrowRangeError(ctx, c"invalid length");
                }
            }
            buffer = (*argv.offset(0)).dup();
        } else {
            return if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&(*p).class_id()) {
                js_typed_array_constructor_ta(ctx, new_target, *argv.offset(0), classid)
            } else {
                js_typed_array_constructor_obj(ctx, new_target, *argv.offset(0), classid)
            };
        }
    }

    let obj = js_create_from_ctor(ctx, new_target, classid as _);
    if JS_IsException(obj) {
        JS_FreeValue(ctx, buffer);
        return JSValue::EXCEPTION;
    }
    if typed_array_init(ctx, obj, buffer, offset, len) != 0 {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    return obj;
}

pub unsafe fn js_typed_array_constructor_obj(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    obj: JSValueConst,
    classid: i32,
) -> JSValue {
    let size_log2 = typed_array_size_log2(classid as _);

    let ret = js_create_from_ctor(ctx, new_target, classid as _);
    if JS_IsException(ret) {
        return JSValue::EXCEPTION;
    }

    let iter = JS_GetProperty(ctx, obj, JS_ATOM_Symbol_iterator);
    if JS_IsException(iter) {
        // C2RS: goto fail replaced with conditional logic
        JS_FreeValue(ctx, ret);
        return JSValue::EXCEPTION;
    }

    let arr: JSValue;
    let len: i64;

    if !JS_IsUndefined(iter) && !JS_IsNull(iter) {
        let mut len1: u32 = 0;
        arr = js_array_from_iterator(ctx, &mut len1 as *mut u32, obj, iter);
        JS_FreeValue(ctx, iter);
        if JS_IsException(arr) {
            JS_FreeValue(ctx, ret);
            return JSValue::EXCEPTION;
        }
        len = len1 as i64;
    } else {
        len = if let Ok(l) = js_get_length64(ctx, obj) {
            l
        } else {
            JS_FreeValue(ctx, ret);
            return JSValue::EXCEPTION;
        };
        arr = obj.dup();
    }

    let buffer = js_array_buffer_constructor1(ctx, JSValue::UNDEFINED, (len as u64) << size_log2);
    if JS_IsException(buffer) {
        // C2RS: goto fail replaced with conditional logic
        JS_FreeValue(ctx, arr);
        JS_FreeValue(ctx, ret);
        return JSValue::EXCEPTION;
    }

    if typed_array_init(ctx, ret, buffer, 0, len as _) != 0 {
        JS_FreeValue(ctx, arr);
        JS_FreeValue(ctx, ret);
        JS_FreeValue(ctx, buffer);
        return JSValue::EXCEPTION;
    }

    for i in 0..len {
        let val = arr.get_property_u32(ctx.as_mut().unwrap(), i as u32);
        if val.is_exception() {
            JS_FreeValue(ctx, arr);
            JS_FreeValue(ctx, ret);
            JS_FreeValue(ctx, buffer);
            return JSValue::EXCEPTION;
        }

        if ret
            .set_property_u32(ctx.as_mut().unwrap(), i as u32, val)
            .is_err()
        {
            JS_FreeValue(ctx, arr);
            JS_FreeValue(ctx, ret);
            JS_FreeValue(ctx, buffer);
            return JSValue::EXCEPTION;
        }
    }

    JS_FreeValue(ctx, arr);
    return ret;
}

pub unsafe fn js_typed_array_constructor_ta(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    src_obj: JSValueConst,
    classid: i32,
) -> JSValue {
    let obj = js_create_from_ctor(ctx, new_target, classid as JSClassID);
    if JS_IsException(obj) {
        return obj;
    }

    let p = JS_VALUE_GET_OBJ(src_obj);
    if typed_array_is_detached(ctx, p) {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let ta = (*p).u.typed_array;
    let len = (*p).array_length_unchecked();
    let src_buffer = (*ta).buffer;
    let src_abuf = (*src_buffer).u.array_buffer;
    let size_log2 = typed_array_size_log2(classid as _);

    let buffer = js_array_buffer_constructor1(ctx, JSValue::UNDEFINED, (len as u64) << size_log2);
    if JS_IsException(buffer) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    // necessary because it could have been detached
    if typed_array_is_detached(ctx, p) {
        JS_FreeValue(ctx, buffer);
        JS_FreeValue(ctx, obj);
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return JSValue::EXCEPTION;
    }

    let abuf = JS_GetOpaque(buffer, JS_CLASS_ARRAY_BUFFER) as *mut JSArrayBuffer;
    if typed_array_init(ctx, obj, buffer, 0, len as _) != 0 {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    if (*p).class_id() == classid as JSClassID {
        // same type: copy the content
        std::ptr::copy_nonoverlapping(
            (*src_abuf).data.offset((*ta).offset as isize),
            (*abuf).data,
            (*abuf).byte_length as usize,
        );
    } else {
        for i in 0..len {
            let val = src_obj.get_property_u32(ctx.as_mut().unwrap(), i as u32);
            if val.is_exception() {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            if obj
                .set_property_u32(ctx.as_mut().unwrap(), i as u32, val)
                .is_err()
            {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
    }

    obj
}

unsafe fn js_array_buffer_free(rt: *mut JSRuntime, opaque: *mut c_void, ptr: *mut c_void) {
    js_free_rt(rt, ptr);
}

pub unsafe fn js_array_buffer_constructor2(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    len: u64,
    class_id: JSClassID,
) -> JSValue {
    js_array_buffer_constructor3(
        ctx,
        new_target,
        len,
        class_id,
        std::ptr::null_mut(),
        Some(js_array_buffer_free),
        std::ptr::null_mut(),
        true,
    )
}
pub unsafe fn js_array_buffer_constructor1(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    len: u64,
) -> JSValue {
    js_array_buffer_constructor2(ctx, new_target, len, JS_CLASS_ARRAY_BUFFER)
}

pub unsafe fn js_typed_array_create(
    ctx: *mut JSContext,
    ctor: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let ret = JS_CallConstructor(ctx, ctor, argc, argv);
    if JS_IsException(ret) {
        return ret;
    }

    // validate the typed array
    let new_len = if let Ok(n) = js_typed_array_get_length_internal(ctx, ret) {
        n
    } else {
        JS_FreeValue(ctx, ret);
        return JSValue::EXCEPTION;
    };

    if argc == 1 {
        // ensure that it is large enough
        let len = if let Ok(n) = JS_ToLengthFree(ctx, (*argv.offset(0)).dup()) {
            n
        } else {
            JS_FreeValue(ctx, ret);
            return JSValue::EXCEPTION;
        };

        if new_len < len as u32 {
            JS_FreeValue(ctx, ret);
            (*ctx).JS_ThrowTypeError(c"TypedArray length is too small");
            return JSValue::EXCEPTION;
        }
    }

    ret
}

pub unsafe fn js_typed_array_get_length_internal(
    ctx: *mut JSContext,
    obj: JSValueConst,
) -> Result<u32, i32> {
    let p = get_typed_array(ctx, obj, false);
    if p.is_null() {
        return Err(-1);
    }
    if typed_array_is_detached(ctx, p) {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    Ok((*p).array_length_unchecked() as u32)
}
