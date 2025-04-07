use std::ffi::CStr;

use crate::{
    class::JSClassID,
    context::JSContext,
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{list_add_tail, list_del, list_head},
    value::{
        object::JSObject, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT,
        JS_TAG_OBJECT, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_DATAVIEW, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

pub struct JSTypedArray {
    pub link: list_head,    /* link to arraybuffer */
    pub obj: *mut JSObject, /* back pointer to the TypedArray/DataView object */

    /// based array buffer  
    pub buffer: *mut JSObject,
    /// offset in the array buffer  
    pub offset: u32,
    /// length in the array buffer   
    pub length: u32,
}

pub const JS_TYPED_ARRAY_COUNT: usize =
    JS_CLASS_FLOAT64_ARRAY as usize - JS_CLASS_UINT8C_ARRAY as usize + 1;

pub const TYPED_ARRAY_SIZE_LOG2: [u8; JS_TYPED_ARRAY_COUNT] = [
    0, 0, 0, 1, 1, 2, 2, 3, 3, /* BigInt64Array, BigUint64Array */
    2, 3,
];

#[inline(always)]
pub fn typed_array_size_log2(classid: JSClassID) -> u8 {
    debug_assert!((JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&classid));

    TYPED_ARRAY_SIZE_LOG2[(classid - JS_CLASS_UINT8C_ARRAY) as usize]
}

// 'obj' must be an allocated typed array object
pub unsafe fn typed_array_init(
    ctx: *mut JSContext,
    obj: JSValueConst,
    buffer: JSValue,
    offset: u64,
    len: u64,
) -> i32 {
    let p = JS_VALUE_GET_OBJ(obj);
    let size_log2 = typed_array_size_log2((*p).class_id());

    let mut ta = if let Some(p) = js_malloc_typed::<JSTypedArray>(ctx, 1, false) {
        Box::from_raw(p.as_ptr())
    } else {
        JS_FreeValue(ctx, buffer);
        return -1;
    };

    let pbuffer = JS_VALUE_GET_OBJ(buffer);
    let abuf = (*pbuffer).u.array_buffer;

    ta.obj = p;
    ta.buffer = pbuffer;
    ta.offset = offset as u32;
    ta.length = (len as u32) << size_log2;

    list_add_tail(&mut ta.link, &mut (*abuf).array_list);

    (*p).u.typed_array = Box::into_raw(ta);
    (*(*p).u.array).typed_array_count = len as u32;
    (*(*p).u.array).data.ptr = (*abuf).data.offset(offset as isize).cast();

    0
}

pub unsafe fn get_typed_array(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    is_dataview: bool,
) -> *mut JSObject {
    if JS_VALUE_GET_TAG(this_val) != JS_TAG_OBJECT {
        (*ctx).JS_ThrowTypeError(CStr::from_ptr(
            format!(
                "not a {}\0",
                if is_dataview {
                    "DataView"
                } else {
                    "TypedArray"
                }
            )
            .as_ptr()
            .cast(),
        ));
        return std::ptr::null_mut();
    }

    let p = JS_VALUE_GET_OBJ(this_val);
    if is_dataview {
        if (*p).class_id() != JS_CLASS_DATAVIEW {
            (*ctx).JS_ThrowTypeError(CStr::from_ptr(
                format!(
                    "not a {}\0",
                    if is_dataview {
                        "DataView"
                    } else {
                        "TypedArray"
                    }
                )
                .as_ptr()
                .cast(),
            ));
            return std::ptr::null_mut();
        }
    } else {
        if !((JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&(*p).class_id())) {
            (*ctx).JS_ThrowTypeError(CStr::from_ptr(
                format!(
                    "not a {}\0",
                    if is_dataview {
                        "DataView"
                    } else {
                        "TypedArray"
                    }
                )
                .as_ptr()
                .cast(),
            ));
            return std::ptr::null_mut();
        }
    }

    p
}

pub unsafe fn js_typed_array_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = JS_VALUE_GET_OBJ(val);
    let ta = (*p).u.typed_array;
    if !ta.is_null() {
        // during the GC the finalizers are called in an arbitrary
        // order so the ArrayBuffer finalizer may have been called
        if !((*ta).link.next).is_null() {
            list_del(&mut (*ta).link);
        }
        JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*ta).buffer.cast()));

        drop(Box::from_raw(ta));
    }
}

pub unsafe fn js_typed_array_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let p = JS_VALUE_GET_OBJ(val);
    let ta = (*p).u.typed_array;
    if !ta.is_null() {
        JS_MarkValue(rt, JS_MKPTR!(JSTag::Object, (*ta).buffer.cast()), mark_func);
    }
}

/* WARNING: 'p' must be a typed array */
pub unsafe fn typed_array_is_detached(ctx: *const JSContext, p: *const JSObject) -> bool {
    let ta = (*p).u.typed_array;
    let abuf = (*(*ta).buffer).u.array_buffer;

    /* XXX: could simplify test by ensuring that
    p->u.array.u.ptr is NULL iff it is detached */
    (*abuf).detached
}
