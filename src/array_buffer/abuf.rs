use std::ffi::c_void;

use crate::{
    class::{js_create_from_ctor, JSClassID},
    context::JSContext,
    error::JS_ThrowRangeError,
    list_entry,
    mem::{js_free_rt, js_malloc_typed},
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    types::{init_list_head, list_foreach_safe, list_head},
    value::{JSValue, JSValueConst, JS_ToIndex, JS_VALUE_GET_OBJ},
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW, JS_CLASS_SHARED_ARRAY_BUFFER,
};

pub type JSFreeArrayBufferDataFunc =
    unsafe fn(rt: *mut JSRuntime, opaque: *mut c_void, data: *mut u8, data_length: usize);

pub struct JSArrayBuffer {
    /// NULL if detached.
    pub data: *mut u8,
    /// data length in bytes
    pub byte_length: i32,

    /// detatched flag
    pub detached: bool,

    /// if shared, the array buffer cannot be detached
    pub shared: bool,

    /// typed arrays that based on this array buffer.
    pub array_list: list_head,

    pub opaque: *mut c_void,

    /// for default allocated buffer, this is used to free `data`.
    /// in case `data` is allocated by JSRuntime.sab_funcs.sab_alloc,
    /// JSRuntime.sab_funcs.sab_free will be used to free it,
    /// this callback is not used in the case.
    pub free_func: Option<JSFreeArrayBufferDataFunc>,
}

impl JSValue {
    /// Get Array buffer from JSValue.
    /// If value if not an ArrayBuffer or SharedArrayBuffer instance, behaviour is undefined.
    pub unsafe fn array_buffer_unchecked<'a>(self) -> &'a mut JSArrayBuffer {
        debug_assert!(self.is_object());

        let p = self.object_unchecked();
        debug_assert!(matches!(
            p.class_id(),
            JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
        ));

        p.u.array_buffer.as_mut().unwrap()
    }

    /// Get Array buffer from JSValue.
    /// If value if not an ArrayBuffer or SharedArrayBuffer instance, returns Err.
    pub fn array_buffer<'a>(self) -> Result<&'a mut JSArrayBuffer, i32> {
        self.object().ok_or(-1).and_then(|p| {
            if matches!(
                p.class_id(),
                JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
            ) {
                Ok(unsafe { p.u.array_buffer.as_mut().unwrap() })
            } else {
                Err(-1)
            }
        })
    }
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
    let rt = (*ctx).rt();

    // XXX: we are currently limited to 2 GB
    if len > i32::MAX as u64 {
        JS_ThrowRangeError(ctx, c"invalid array buffer length");
        return JSValue::EXCEPTION;
    }

    // make ArrayBuffer state
    let mut ab_state = if let Some(p) = js_malloc_typed::<JSArrayBuffer>(ctx, 1, false) {
        Box::from_raw(p.as_ptr())
    } else {
        return JSValue::EXCEPTION;
    };

    ab_state.byte_length = len as i32;

    if alloc_flag {
        // alloc data buffer.
        // the allocation must be done after the object creation
        if class_id == JS_CLASS_SHARED_ARRAY_BUFFER && rt.sab_funcs.sab_alloc.is_some() {
            // alloc sab
            ab_state.data =
                (rt.sab_funcs.sab_alloc.unwrap())(rt.sab_funcs.sab_opaque, len as usize).cast();

            if ab_state.data.is_null() {
                return JSValue::EXCEPTION;
            }

            std::ptr::write_bytes(ab_state.data, 0, len as usize);
        } else {
            // default alloc
            let mut chunk = Vec::<u8>::new();
            if chunk.try_reserve_exact(len as usize).is_err() {
                (*ctx).JS_ThrowOutOfMemory();
                return JSValue::EXCEPTION;
            }
            chunk.resize(len as usize, 0);

            // leak Box<[u8]> to *mut [u8], cast to *mut u8
            ab_state.data = Box::into_raw(chunk.into_boxed_slice()) as *mut u8;
        }
    } else {
        if class_id == JS_CLASS_SHARED_ARRAY_BUFFER && rt.sab_funcs.sab_dup.is_some() {
            (rt.sab_funcs.sab_dup.unwrap())(rt.sab_funcs.sab_opaque, buf.cast());
        }
        ab_state.data = buf;
    }

    init_list_head(&mut ab_state.array_list);

    ab_state.detached = false;
    ab_state.shared = class_id == JS_CLASS_SHARED_ARRAY_BUFFER;
    ab_state.opaque = opaque;
    ab_state.free_func = free_func;
    if alloc_flag && !buf.is_null() {
        std::ptr::copy_nonoverlapping(buf, ab_state.data, len as usize);
    }

    // create js instance
    let ab_obj = js_create_from_ctor(ctx, new_target, class_id);
    if ab_obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    // Note: set pointer to object opaque, can be accessed using u.array_buffer field.
    ab_obj
        .object_unchecked()
        .set_opaque(Box::into_raw(ab_state).cast());

    ab_obj
}

/// the callback to free JSArrayBuffer.
/// - opaque: JSArrayBuffer.opaque field
/// - data: JSArrayBuffer.data field
/// - data_length: JSArrayBuffer.byte_length field
pub unsafe fn js_array_buffer_free(
    rt: *mut JSRuntime,
    opaque: *mut c_void,
    data: *mut u8,
    data_length: usize,
) {
    // (data, data_length) is actually raw pointer to Box<[u8]>,
    // restore then drop it.
    let s = std::ptr::slice_from_raw_parts_mut(data, data_length);
    drop(Box::from_raw(s));
}

pub unsafe fn js_array_buffer_constructor2(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    len: u64,
    class_id: JSClassID,
) -> JSValue {
    return js_array_buffer_constructor3(
        ctx,
        new_target,
        len,
        class_id,
        std::ptr::null_mut(),
        Some(js_array_buffer_free),
        std::ptr::null_mut(),
        true,
    );
}

pub unsafe fn js_array_buffer_constructor1(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    len: u64,
) -> JSValue {
    return js_array_buffer_constructor2(ctx, new_target, len, JS_CLASS_ARRAY_BUFFER);
}

pub unsafe fn js_array_buffer_constructor(
    ctx: *mut JSContext,
    new_target: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let mut len: u64 = 0;
    if JS_ToIndex(ctx, &mut len as *mut u64, *argv.offset(0)) != 0 {
        return JSValue::EXCEPTION;
    }
    return js_array_buffer_constructor1(ctx, new_target, len);
}

pub unsafe fn js_shared_array_buffer_constructor(
    ctx: *mut JSContext,
    new_target: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let mut len: u64 = 0;
    if JS_ToIndex(ctx, &mut len as *mut u64, *argv.offset(0)) != 0 {
        return JSValue::EXCEPTION;
    }
    return js_array_buffer_constructor2(ctx, new_target, len, JS_CLASS_SHARED_ARRAY_BUFFER);
}

// also used for SharedArrayBuffer
pub unsafe fn js_array_buffer_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = JS_VALUE_GET_OBJ(val);
    let abuf = (*p).u.array_buffer;

    // Note: the ArrayBuffer finalizer may be called before the typed array
    // finalizers using it, so abuf->array_list is not necessarily empty.
    if !abuf.is_null() {
        list_foreach_safe(&mut (*abuf).array_list, |el| {
            let ta = list_entry!(el, JSTypedArray, link);
            (*ta).link.prev = std::ptr::null_mut();
            (*ta).link.next = std::ptr::null_mut();
            let p1 = (*ta).obj;
            // Note: the typed array length and offset fields are not modified
            if (*p1).class_id() != JS_CLASS_DATAVIEW {
                (*(*p1).u.array).typed_array_count = 0;
                (*(*p1).u.array).data.ptr = std::ptr::null_mut();
            }
        });

        if (*abuf).shared && ((*rt).sab_funcs.sab_free).is_some() {
            (((*rt).sab_funcs.sab_free).unwrap())((*rt).sab_funcs.sab_opaque, (*abuf).data.cast());
        } else {
            if let Some(cb) = (*abuf).free_func {
                cb(
                    rt,
                    (*abuf).opaque,
                    (*abuf).data,
                    (*abuf).byte_length as usize,
                );
            }
        }

        js_free_rt(rt, abuf.cast());
    }
}
