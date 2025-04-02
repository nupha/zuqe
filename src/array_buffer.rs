use std::ffi::c_void;

use crate::{
    class::{js_create_from_ctor, JSClassID},
    context::JSContext,
    error::JS_ThrowRangeError,
    mem::{js_malloc_typed, js_mallocz},
    runtime::JSRuntime,
    types::{init_list_head, list_head},
    value::{object::JS_SetOpaque, JSValue, JSValueConst, JS_FreeValue, JS_IsException},
    JS_CLASS_SHARED_ARRAY_BUFFER,
};

pub type JSFreeArrayBufferDataFunc =
    unsafe fn(rt: *mut JSRuntime, opaque: *mut c_void, ptr: *mut c_void);

pub struct JSArrayBuffer {
    pub byte_length: i32, /* 0 if detached */
    pub detached: bool,
    pub shared: bool,  /* if shared, the array buffer cannot be detached */
    pub data: *mut u8, /* NULL if detached */

    /// typed arrays that based on this array buffer.
    pub array_list: list_head,

    pub opaque: *mut c_void,
    pub free_func: Option<JSFreeArrayBufferDataFunc>,
}

pub unsafe fn js_array_buffer_constructor3(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    len: u64,
    class_id: JSClassID,
    buf: *mut u8,
    free_func: Option<JSFreeArrayBufferDataFunc>,
    opaque: *mut c_void,
    alloc_flag: bool,
) -> JSValue {
    let obj = js_create_from_ctor(ctx, new_target, class_id);
    if JS_IsException(obj) {
        return obj;
    }

    let rt = (*ctx).rt();

    // XXX: we are currently limited to 2 GB
    if len > i32::MAX as u64 {
        JS_ThrowRangeError(ctx, c"invalid array buffer length");
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut abuf = if let Some(p) = js_malloc_typed::<JSArrayBuffer>(ctx, 1, false) {
        Box::from_raw(p.as_ptr())
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    abuf.byte_length = len as i32;
    if alloc_flag {
        if class_id == JS_CLASS_SHARED_ARRAY_BUFFER && rt.sab_funcs.sab_alloc.is_some() {
            (*abuf).data = (rt.sab_funcs.sab_alloc.unwrap())(
                rt.sab_funcs.sab_opaque,
                std::cmp::max(len as usize, 1),
            )
            .cast();

            if (*abuf).data.is_null() {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            std::ptr::write_bytes(abuf.data, 0, len as usize);
        } else {
            // the allocation must be done after the object creation
            (*abuf).data = js_mallocz(ctx, std::cmp::max(len as usize, 1)).cast();
            if (*abuf).data.is_null() {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
    } else {
        if class_id == JS_CLASS_SHARED_ARRAY_BUFFER && rt.sab_funcs.sab_dup.is_some() {
            (rt.sab_funcs.sab_dup.unwrap())(rt.sab_funcs.sab_opaque, buf.cast());
        }
        abuf.data = buf;
    }

    init_list_head(&mut abuf.array_list);

    abuf.detached = false;
    abuf.shared = class_id == JS_CLASS_SHARED_ARRAY_BUFFER;
    abuf.opaque = opaque;
    abuf.free_func = free_func;
    if alloc_flag && !buf.is_null() {
        std::ptr::copy_nonoverlapping(buf, abuf.data, len as usize);
    }

    JS_SetOpaque(obj, Box::into_raw(abuf).cast());

    obj
}

pub unsafe fn js_array_buffer_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}
