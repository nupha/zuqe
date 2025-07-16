use std::{
    cell::{Ref, RefCell},
    ffi::c_void,
    ops::Deref,
    ptr::NonNull,
    rc::Rc,
    sync::{Arc, RwLock},
};

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW, JS_CLASS_SHARED_ARRAY_BUFFER,
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::CallContext,
    object::{ObjectDetail, JSObjectWeakref},
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

pub type JSFreeArrayBufferDataFunc =
    fn(rt: &mut JSRuntime, opaque: *mut c_void, data: *mut u8, data_length: usize);

pub struct JSSharedArrayBufferFunctions {
    pub sab_alloc: fn(*mut c_void, usize) -> Result<Box<[u8]>, i32>,
    pub sab_free: fn(*mut c_void, usize),
    // pub sab_dup: fn(*mut c_void, *mut c_void),
    pub sab_opaque: *mut c_void,
}

#[derive(Debug)]
pub enum ArrayBufferContent {
    Empty,
    Owned(Box<[u8]>),
    Shared(Arc<RwLock<Box<[u8]>>>),
    Raw(NonNull<u8>, usize, Option<JSFreeArrayBufferDataFunc>),
}

impl ArrayBufferContent {
    /// for SharedArrayBuffer
    pub fn is_shared(&self) -> bool {
        if let Self::Shared(_) = self {
            true
        } else {
            false
        }
    }

    /// byte length
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Owned(b) => b.len(),
            Self::Shared(a) => a.read().unwrap().len(),
            Self::Raw(_, len, _) => *len,
        }
    }

    #[deprecated(
        note = "it's unsafe for SharedArrayBuffer with Arc+Mutex, don't use this. will be removed"
    )]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            Self::Empty => std::ptr::null_mut(),
            Self::Owned(b) => b.as_mut_ptr(),
            Self::Shared(a) => a.write().unwrap().as_mut_ptr(),
            Self::Raw(p, _, _) => p.as_ptr(),
        }
    }

    fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        match self {
            Self::Empty => f(&[]),
            Self::Owned(b) => f(b.as_ref()),
            Self::Shared(a) => f(a.read().unwrap().as_ref()),
            Self::Raw(p, len, _) => f(unsafe { std::slice::from_raw_parts(p.as_ptr(), *len) }),
        }
    }

    fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        match self {
            Self::Empty => f(&mut []),
            Self::Owned(b) => f(b.as_mut()),
            Self::Shared(a) => f(a.write().unwrap().as_mut()),
            Self::Raw(p, len, _) => f(unsafe { std::slice::from_raw_parts_mut(p.as_ptr(), *len) }),
        }
    }

    unsafe fn with_bytes_ptr<R>(&self, f: impl FnOnce(*const u8) -> R) -> R {
        match self {
            Self::Empty => f(std::ptr::null()),
            Self::Owned(b) => f(b.as_ptr()),
            Self::Shared(a) => f(a.read().unwrap().as_ptr()),
            Self::Raw(p, _, _) => f(p.as_ptr()),
        }
    }

    unsafe fn with_bytes_mut_ptr<R>(&mut self, f: impl FnOnce(*mut u8) -> R) -> R {
        match self {
            Self::Empty => f(std::ptr::null_mut()),
            Self::Owned(b) => f(b.as_mut_ptr()),
            Self::Shared(a) => f(a.write().unwrap().as_mut_ptr()),
            Self::Raw(p, _, _) => f(p.as_ptr()),
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

#[derive(Debug)]
pub(crate) struct JSArrayBufferInner {
    pub content: ArrayBufferContent,

    pub detached: bool,

    /// if shared, the array buffer cannot be detached
    #[deprecated(note = "use content is_shared() instead")]
    pub shared: bool,

    /// typed arrays that based on this array buffer.
    pub array_list: Vec<JSObjectWeakref>,

    pub opaque: *mut c_void,
}

impl JSArrayBufferInner {
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

    // #[inline(always)]
    // pub unsafe fn with_bytes_ptr<R>(&self, f: impl FnOnce(*const u8) -> R) -> R {
    //     self.content.with_bytes_ptr(f)
    // }

    // #[inline(always)]
    // pub unsafe fn with_bytes_mut_ptr<R>(&mut self, f: impl FnOnce(*mut u8) -> R) -> R {
    //     self.content.with_bytes_mut_ptr(f)
    // }
}

#[derive(Debug, Clone)]
pub struct ObjArrayBuffer(pub(crate) Rc<RefCell<JSArrayBufferInner>>);

impl ObjArrayBuffer {
    #[inline(always)]
    pub fn is_detached(&self) -> bool {
        self.0.borrow().detached
    }

    /// get capacity of buffer, count in bytes
    #[inline(always)]
    pub fn byte_length(&self) -> usize {
        self.0.borrow().byte_length()
    }

    #[inline(always)]
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        self.0.borrow().with_bytes(f)
    }

    #[inline(always)]
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.0.borrow_mut().with_bytes_mut(f)
    }

    #[inline(always)]
    pub fn content(&self) -> Ref<'_, ArrayBufferContent> {
        Ref::map(self.0.borrow(), |b| &b.content)
    }

    #[inline(always)]
    pub fn opaque(&self) -> *mut c_void {
        self.0.borrow().opaque
    }
}

impl JSContext {
    /// Create new instance of class ARRAY_BUFFER.
    fn do_create_array_buffer(
        &self,
        new_target: &JSValueConst,
        class_id: JSClassID,
        content: ArrayBufferContent,
        opaque: *mut c_void,
    ) -> JSResult {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
        ));

        // make ArrayBuffer payload
        let ab = {
            let inner = JSArrayBufferInner {
                content,
                detached: false,
                shared: class_id == JS_CLASS_SHARED_ARRAY_BUFFER,
                array_list: Vec::new(),
                opaque,
            };
            ObjArrayBuffer(Rc::new(RefCell::new(inner)))
        };

        // create array buffer or shared array buffer instance
        self.create_instance_from_ctor(
            new_target,
            if class_id == JS_CLASS_ARRAY_BUFFER {
                ObjectDetail::ARRAY_BUFFER(ab)
            } else {
                ObjectDetail::SHARED_ARRAY_BUFFER(ab)
            },
        )
        .map_err(|_| -1)
    }

    /// create empty array buffer instance with required capacity.
    pub fn create_array_buffer(
        &self,
        new_target: &JSValue,
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

        self.do_create_array_buffer(new_target, class_id, content, opaque)
    }

    #[inline(always)]
    pub fn create_array_buffer_with_data(
        &self,
        new_target: &JSValue,
        class_id: JSClassID,
        data: Box<[u8]>,
        opaque: *mut c_void,
    ) -> JSResult {
        self.do_create_array_buffer(
            new_target,
            class_id,
            ArrayBufferContent::Owned(data),
            opaque,
        )
    }

    #[inline(always)]
    pub fn create_array_buffer_from_raw_parts(
        &self,
        new_target: &JSValueConst,
        class_id: JSClassID,
        data: NonNull<u8>,
        data_len: usize,
        free_func: Option<JSFreeArrayBufferDataFunc>,
        opaque: *mut c_void,
    ) -> JSResult {
        debug_assert!(data_len > 0);

        self.do_create_array_buffer(
            new_target,
            class_id,
            ArrayBufferContent::Raw(data, data_len, free_func),
            opaque,
        )
    }
}

#[deprecated(note = "use JSContext::create_array_buffer_inner0() instead")]
pub unsafe fn js_array_buffer_constructor3(
    ctx: &JSContext,
    new_target: JSValueConst,
    byte_length: usize,
    class_id: JSClassID,
    buf: *mut u8,
    free_func: Option<JSFreeArrayBufferDataFunc>,
    opaque: *mut c_void,
    alloc_flag: bool,
) -> JSValue {
    // ctx
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

/// the callback to free ObjArrayBuffer.
/// - opaque: ObjArrayBuffer.opaque field
/// - data: ObjArrayBuffer.data field
/// - data_length: ObjArrayBuffer.byte_length field
#[deprecated(note = "not used")]
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
    ctx: &JSContext,
    new_target: &JSValueConst,
    byte_length: usize,
    class_id: JSClassID,
) -> JSValue {
    ctx.create_array_buffer(new_target, class_id, byte_length, std::ptr::null_mut())
        .unwrap_or(JSValue::EXCEPTION)
}

#[inline(always)]
pub unsafe fn js_array_buffer_constructor1(
    ctx: &JSContext,
    new_target: &JSValueConst,
    byte_length: usize,
) -> JSValue {
    js_array_buffer_constructor2(ctx, new_target, byte_length, JS_CLASS_ARRAY_BUFFER)
}

pub fn js_array_buffer_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let size = cc.arg(0).coerce_to_index(ctx)?;
    unsafe { js_array_buffer_constructor1(ctx, cc.this(), size).check_exception() }
}

pub fn js_shared_array_buffer_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let len = cc.arg(0).coerce_to_index(ctx)?;
    ctx.create_array_buffer(
        cc.this(),
        JS_CLASS_SHARED_ARRAY_BUFFER,
        len,
        std::ptr::null_mut(),
    )
}

pub unsafe fn JS_DetachArrayBuffer(ctx: &JSContext, obj: &JSValueConst) {
    if obj.object_unchecked().class_id() != JS_CLASS_ARRAY_BUFFER {
        return;
    }

    if let Some(abuf) = obj.object_unchecked().0.payload.borrow_mut().array_buffer_mut() {
        if !abuf.0.borrow().detached {
            match abuf.content().deref() {
                ArrayBufferContent::Raw(ptr, len, fin) => {
                    if let Some(f) = fin {
                        f(ctx.rt_mut(), abuf.opaque(), ptr.as_ptr(), *len);
                    }
                }
                _ => {}
            }

            abuf.0.borrow_mut().detached = true;

            for w in abuf.0.borrow_mut().array_list.iter_mut() {
                if let Some(o) = w.upgrade() {
                    // typed array or dataview
                    // Note: the typed array length and offset fields are not modified
                    if o.class_id() != JS_CLASS_DATAVIEW {
                        if let Some(ta) = o.0.payload.borrow_mut().typed_array_mut() {
                            ta.count = 0;
                        } else {
                            unreachable!()
                        }
                    }
                }
            }
        }
    }
}

// also used for SharedArrayBuffer
pub fn js_array_buffer_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {
    let abuf = isa.array_buffer().unwrap();

    // free content
    match abuf.content().deref() {
        ArrayBufferContent::Raw(p, len, Some(free_func)) => {
            free_func(rt, abuf.opaque(), p.as_ptr(), *len);
        }
        ArrayBufferContent::Shared(a) => {
            if Arc::strong_count(&a) == 1 {
                if let Some(fns) = &rt.sab_funcs {
                    (fns.sab_free)(fns.sab_opaque, a.read().unwrap().len());
                }
            }
        }
        _ => {}
    }
}
