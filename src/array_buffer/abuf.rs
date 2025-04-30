use std::{
    ffi::c_void,
    ops::Deref,
    ptr::NonNull,
    sync::{Arc, RwLock},
};

use crate::{
    class::{js_create_from_ctor, JSClassID},
    context::JSContext,
    error::JSResult,
    list_entry,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    types::{init_list_head, list_foreach_safe, list_head},
    value::{JSValue, JSValueConst, JS_ToIndex},
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW, JS_CLASS_SHARED_ARRAY_BUFFER,
};

pub type JSFreeArrayBufferDataFunc =
    unsafe fn(rt: *mut JSRuntime, opaque: *mut c_void, data: *mut u8, data_length: usize);

pub struct JSSharedArrayBufferFunctions {
    pub sab_alloc: fn(*mut c_void, usize) -> Result<Box<[u8]>, i32>,
    pub sab_free: fn(*mut c_void, usize),
    // pub sab_dup: fn(*mut c_void, *mut c_void),
    pub sab_opaque: *mut c_void,
}

pub enum ArrayBufferContent {
    Empty,
    Owned(Box<[u8]>),
    Shared(Arc<RwLock<Box<[u8]>>>),
    Raw(NonNull<u8>, usize, Option<JSFreeArrayBufferDataFunc>),
}

impl ArrayBufferContent {
    pub fn is_shared(&self) -> bool {
        if let Self::Shared(_) = self {
            true
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Owned(b) => b.len(),
            Self::Shared(a) => a.read().unwrap().len(),
            Self::Raw(_, len, _) => *len,
        }
    }

    #[deprecated(note = "this is unsafe")]
    pub fn as_ptr(&self) -> *const u8 {
        match self {
            Self::Empty => std::ptr::null(),
            Self::Owned(b) => b.as_ptr(),
            Self::Shared(a) => a.read().unwrap().as_ptr(),
            Self::Raw(p, _, _) => p.as_ptr(),
        }
    }

    #[deprecated(note = "this is unsafe")]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            Self::Empty => std::ptr::null_mut(),
            Self::Owned(b) => b.as_mut_ptr(),
            Self::Shared(a) => a.write().unwrap().as_mut_ptr(),
            Self::Raw(p, _, _) => p.as_ptr(),
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Owned(b) => b.deref(),
            Self::Shared(a) => todo!(),
            Self::Raw(p, len, _) => unsafe { std::slice::from_raw_parts(p.as_ptr(), *len) },
        }
    }

    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        match self {
            Self::Empty => f(&[]),
            Self::Owned(b) => f(b.as_ref()),
            Self::Shared(a) => f(a.read().unwrap().as_ref()),
            Self::Raw(p, len, _) => f(unsafe { std::slice::from_raw_parts(p.as_ptr(), *len) }),
        }
    }

    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        match self {
            Self::Empty => f(&mut []),
            Self::Owned(b) => f(b.as_mut()),
            Self::Shared(a) => f(a.write().unwrap().as_mut()),
            Self::Raw(p, len, _) => f(unsafe { std::slice::from_raw_parts_mut(p.as_ptr(), *len) }),
        }
    }

    /// get buffer unique id (use addr). this is used to check if two buffer are same.
    pub fn id(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Owned(b) => b.as_ptr() as usize,
            Self::Shared(a) => a.read().unwrap().as_ptr() as usize,
            Self::Raw(p, _, _) => p.as_ptr() as usize,
        }
    }
}

pub struct JSArrayBuffer {
    pub content: ArrayBufferContent,

    pub detached: bool,

    /// if shared, the array buffer cannot be detached
    pub shared: bool,

    /// typed arrays that based on this array buffer.
    pub array_list: list_head,

    pub opaque: *mut c_void,
    // /// for default allocated buffer, this is used to free `data`.
    // /// in case `data` is allocated by JSRuntime.sab_funcs.sab_alloc,
    // /// JSRuntime.sab_funcs.sab_free will be used to free it,
    // /// this callback is not used in the case.
    // pub free_func: Option<JSFreeArrayBufferDataFunc>,
}

impl JSArrayBuffer {
    /// get capacity of buffer, count in bytes
    #[inline(always)]
    pub fn byte_length(&self) -> usize {
        self.content.len()
    }

    #[inline(always)]
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        self.content.with_bytes(f)
    }

    #[inline(always)]
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.content.with_bytes_mut(f)
    }
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

impl JSContext {
    /// Create new instance of class ARRAY_BUFFER.
    fn create_array_buffer_inner(
        &mut self,
        new_target: JSValueConst,
        class_id: JSClassID,
        content: ArrayBufferContent,
        opaque: *mut c_void,
    ) -> JSResult {
        // make ArrayBuffer data
        let mut ab = unsafe {
            Box::from_raw(
                js_malloc_typed::<JSArrayBuffer>(self, 1, false)
                    .ok_or(-1)?
                    .as_ptr(),
            )
        };

        ab.content = content;

        unsafe {
            init_list_head(&mut ab.array_list);
        }

        ab.detached = false;
        ab.shared = class_id == JS_CLASS_SHARED_ARRAY_BUFFER;
        ab.opaque = opaque;

        // create js instance
        let ab_obj = unsafe { js_create_from_ctor(self, new_target, class_id).check_exception()? };

        // Note: set pointer to object opaque, can be accessed using u.array_buffer field.
        unsafe {
            ab_obj
                .object_unchecked()
                .set_opaque(Box::into_raw(ab).cast());
        }

        Ok(ab_obj)
    }

    pub fn create_array_buffer(
        &mut self,
        new_target: JSValueConst,
        class_id: JSClassID,
        byte_length: usize,
        opaque: *mut c_void,
    ) -> JSResult {
        let content = if byte_length == 0 {
            ArrayBufferContent::Empty
        } else {
            match (class_id, &self.rt().sab_funcs) {
                (JS_CLASS_SHARED_ARRAY_BUFFER, Some(fns)) => {
                    let mut data = (fns.sab_alloc)(fns.sab_opaque, byte_length)?;
                    unsafe {
                        std::ptr::write_bytes(data.as_mut_ptr(), 0, byte_length);
                    }
                    ArrayBufferContent::Shared(Arc::new(RwLock::new(data)))
                }
                _ => {
                    let mut chunk = Vec::<u8>::new();
                    if chunk.try_reserve_exact(byte_length).is_err() {
                        self.JS_ThrowOutOfMemory();
                        return Err(-1);
                    }
                    chunk.resize(byte_length, 0);
                    ArrayBufferContent::Owned(chunk.into_boxed_slice())
                }
            }
        };

        self.create_array_buffer_inner(new_target, class_id, content, opaque)
    }

    #[inline(always)]
    pub fn create_array_buffer_with_data(
        &mut self,
        new_target: JSValueConst,
        class_id: JSClassID,
        data: Box<[u8]>,
        opaque: *mut c_void,
    ) -> JSResult {
        self.create_array_buffer_inner(
            new_target,
            class_id,
            ArrayBufferContent::Owned(data),
            opaque,
        )
    }

    #[inline(always)]
    pub fn create_array_buffer_from_raw_parts(
        &mut self,
        new_target: JSValueConst,
        class_id: JSClassID,
        data: NonNull<u8>,
        data_len: usize,
        free_func: Option<JSFreeArrayBufferDataFunc>,
        opaque: *mut c_void,
    ) -> JSResult {
        debug_assert!(data_len > 0);

        self.create_array_buffer_inner(
            new_target,
            class_id,
            ArrayBufferContent::Raw(data, data_len, free_func),
            opaque,
        )
    }

    // / Create new instance of class ARRAY_BUFFER.
    // / `buf_length` is byte count of buf.
    // / parameter `alloc_flag`:
    // / - true, allocate chunk to copy content from buf,
    // / - false, use provided `buf`, must ensure that `buf` is valid within instance's lifetime.
    // /
    // / When instance is freed, `free_func` will be called to free buffer.
    // fn create_array_buffer_inner0(
    //     &mut self,
    //     new_target: JSValueConst,
    //     class_id: JSClassID,
    //     buf: *mut u8,
    //     buf_length: usize,
    //     free_func: Option<JSFreeArrayBufferDataFunc>,
    //     opaque: *mut c_void,
    //     alloc_flag: bool,
    // ) -> JSResult {
    //     // XXX: we are currently limited to 2 GB
    //     if buf_length > i32::MAX as usize {
    //         JS_ThrowRangeError(self, c"invalid array buffer length");
    //         return Err(-1);
    //     }

    //     // make ArrayBuffer data
    //     let mut ab = unsafe {
    //         Box::from_raw(
    //             js_malloc_typed::<JSArrayBuffer>(self, 1, false)
    //                 .ok_or(-1)?
    //                 .as_ptr(),
    //         )
    //     };

    //     ab.byte_length = buf_length as i32;

    //     match (class_id, &self.rt().sab_funcs) {
    //         (JS_CLASS_SHARED_ARRAY_BUFFER, Some(fns)) => {
    //             if alloc_flag {
    //                 // alloc sab
    //                 ab.data = (fns.sab_alloc)(fns.sab_opaque, buf_length).cast();
    //                 if ab.data.is_null() {
    //                     return Err(-1);
    //                 }
    //                 unsafe {
    //                     std::ptr::write_bytes(ab.data, 0, buf_length as usize);
    //                 }
    //             } else {
    //                 (fns.sab_dup)(fns.sab_opaque, buf.cast());
    //                 ab.data = buf;
    //             }
    //         }
    //         _ => {
    //             if alloc_flag {
    //                 // use default alloc
    //                 let mut chunk = Vec::<u8>::new();
    //                 if chunk.try_reserve_exact(buf_length as usize).is_err() {
    //                     self.JS_ThrowOutOfMemory();
    //                     return Err(-1);
    //                 }
    //                 chunk.resize(buf_length as usize, 0);
    //                 // leak Box<[u8]> to *mut [u8], cast to *mut u8
    //                 ab.data = Box::into_raw(chunk.into_boxed_slice()) as *mut u8;
    //             } else {
    //                 ab.data = buf;
    //             }
    //         }
    //     }

    //     unsafe {
    //         init_list_head(&mut ab.array_list);
    //     }

    //     ab.detached = false;
    //     ab.shared = class_id == JS_CLASS_SHARED_ARRAY_BUFFER;
    //     ab.opaque = opaque;
    //     ab.free_func = free_func;
    //     if alloc_flag && !buf.is_null() {
    //         unsafe {
    //             std::ptr::copy_nonoverlapping(buf, ab.data, buf_length as usize);
    //         }
    //     }

    //     // create js instance
    //     let ab_obj = unsafe { js_create_from_ctor(self, new_target, class_id).check_exception()? };

    //     // Note: set pointer to object opaque, can be accessed using u.array_buffer field.
    //     unsafe {
    //         ab_obj
    //             .object_unchecked()
    //             .set_opaque(Box::into_raw(ab).cast());
    //     }

    //     Ok(ab_obj)
    // }
}

#[deprecated(note = "use JSContext::create_array_buffer_inner0() instead")]
pub unsafe fn js_array_buffer_constructor3(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    byte_length: usize,
    class_id: JSClassID,
    buf: *mut u8,
    free_func: Option<JSFreeArrayBufferDataFunc>,
    opaque: *mut c_void,
    alloc_flag: bool,
) -> JSValue {
    // (*ctx)
    //     .create_array_buffer_inner0(
    //         new_target,
    //         class_id,
    //         buf,
    //         byte_length,
    //         free_func,
    //         opaque,
    //         alloc_flag,
    //     )
    //     .unwrap_or(JSValue::EXCEPTION)
    unreachable!()
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

#[deprecated(note = "use JSContext::create_array_buffer() instead")]
pub unsafe fn js_array_buffer_constructor2(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    byte_length: usize,
    class_id: JSClassID,
) -> JSValue {
    (*ctx)
        .create_array_buffer(new_target, class_id, byte_length, std::ptr::null_mut())
        .unwrap_or(JSValue::EXCEPTION)

    // js_array_buffer_constructor3(
    //     ctx,
    //     new_target,
    //     byte_length,
    //     class_id,
    //     std::ptr::null_mut(),
    //     Some(js_array_buffer_free),
    //     std::ptr::null_mut(),
    //     true,
    // )
}

#[inline(always)]
pub unsafe fn js_array_buffer_constructor1(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    byte_length: usize,
) -> JSValue {
    js_array_buffer_constructor2(ctx, new_target, byte_length, JS_CLASS_ARRAY_BUFFER)
}

pub unsafe fn js_array_buffer_constructor(
    ctx: *mut JSContext,
    new_target: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let mut len: u64 = 0;
    if JS_ToIndex(ctx, &mut len as *mut u64, *argv.offset(0)) != 0 {
        JSValue::EXCEPTION
    } else {
        js_array_buffer_constructor1(ctx, new_target, len as _)
    }
}

pub unsafe fn js_shared_array_buffer_constructor(
    ctx: *mut JSContext,
    new_target: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let mut len: u64 = 0;
    if JS_ToIndex(ctx, &mut len, *argv.offset(0)) != 0 {
        JSValue::EXCEPTION
    } else {
        js_array_buffer_constructor2(ctx, new_target, len as _, JS_CLASS_SHARED_ARRAY_BUFFER)
    }
}

// also used for SharedArrayBuffer
pub unsafe fn js_array_buffer_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = val.object_unchecked();
    let abuf = p.u.array_buffer;

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

        let ab = Box::from_raw(abuf);

        // free content
        match ab.content {
            ArrayBufferContent::Raw(p, len, Some(free)) => {
                free(rt, ab.opaque, p.as_ptr(), len);
            }
            ArrayBufferContent::Shared(a) => {
                if Arc::strong_count(&a) == 1 {
                    if let Some(fns) = &(*rt).sab_funcs {
                        (fns.sab_free)(fns.sab_opaque, a.read().unwrap().len());
                    }
                }
            }
            _ => {}
        }

        // match ((*abuf).shared, &(*rt).sab_funcs) {
        //     (true, Some(fns)) => {
        //         (fns.sab_free)(fns.sab_opaque, (*abuf).data.cast());
        //     }
        //     _ => {
        //         if let Some(cb) = (*abuf).free_func {
        //             cb(
        //                 rt,
        //                 (*abuf).opaque,
        //                 (*abuf).data,
        //                 (*abuf).byte_length as usize,
        //             );
        //         }
        //     }
        // }
    }
}
