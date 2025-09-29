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
    object::{JSObject, JSObjectWeakref, ObjectPayload},
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
pub enum ABufData {
    Empty,
    Owned(Box<[u8]>),
    Shared(Arc<RwLock<Box<[u8]>>>),
    Raw(NonNull<u8>, usize, Option<JSFreeArrayBufferDataFunc>),
}

impl ABufData {
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
struct ABufInner {
    pub content: ABufData,

    /// if is shared, the array buffer cannot be detached
    pub detached: bool,

    /// typed arrays that based on this array buffer.
    pub array_list: Vec<JSObjectWeakref>,

    pub opaque: *mut c_void,
}

impl ABufInner {
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

#[derive(Debug, Clone)]
pub struct ArrayBufPayload(Rc<RefCell<ABufInner>>);

impl ArrayBufPayload {
    #[inline(always)]
    pub fn is_detached(&self) -> bool {
        self.0.borrow().detached
    }

    /// get capacity of buffer, count in bytes
    #[inline(always)]
    pub fn byte_length(&self) -> usize {
        self.0.borrow().byte_length()
    }

    pub fn add_typed_array_weakref(&mut self, t: JSObjectWeakref) {
        self.0.borrow_mut().array_list.push(t);
    }

    /// `obj_id` is the id of typed array object
    pub fn remove_typed_array_weakref(&mut self, obj_id: usize) {
        self.0
            .borrow_mut()
            .array_list
            .retain(|a| match a.0.strong_count() {
                1 => {
                    if let Some(o) = a.upgrade() {
                        o.id() != obj_id
                    } else {
                        false
                    }
                }
                0 => false,
                _ => true,
            });
    }

    #[inline(always)]
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        self.0.borrow().with_bytes(f)
    }

    #[inline(always)]
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.0.borrow_mut().with_bytes_mut(f)
    }

    /// get buffer content
    #[inline(always)]
    pub fn content(&self) -> Ref<'_, ABufData> {
        Ref::map(self.0.borrow(), |b| &b.content)
    }

    fn set_content(&self, content: ABufData) -> ABufData {
        let mut b = self.0.borrow_mut();
        b.detached = false;
        std::mem::replace(&mut b.content, content)
    }

    #[inline(always)]
    pub fn opaque(&self) -> *mut c_void {
        self.0.borrow().opaque
    }
}

impl JSContext {
    /// Create new instance of class ARRAY_BUFFER.
    /// TODO: should return ArrayBuffer type
    fn do_create_array_buffer(
        &self,
        new_target: &JSValueConst,
        class_id: JSClassID,
        content: ABufData,
        opaque: *mut c_void,
    ) -> JSResult {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
        ));

        // make ArrayBuffer payload
        let ab = {
            let inner = ABufInner {
                content,
                detached: false,
                array_list: Vec::new(),
                opaque,
            };
            ArrayBufPayload(Rc::new(RefCell::new(inner)))
        };

        // create array buffer or shared array buffer instance
        self.create_object_apply_ctor(
            new_target,
            if class_id == JS_CLASS_ARRAY_BUFFER {
                ObjectPayload::ARRAY_BUFFER(ab)
            } else {
                ObjectPayload::SHARED_ARRAY_BUFFER(ab)
            },
        )
        .map(JSValue::Object)
        .map_err(|_| -1)
    }

    /// create empty array buffer instance with required capacity.
    pub fn create_array_buffer_ex(
        &self,
        new_target: &JSValue,
        class_id: JSClassID,
        byte_length: usize,
        opaque: *mut c_void,
    ) -> Result<JSObject, i32> {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
        ));

        // Note: according to spec -
        // 'The new ArrayBuffer instance is created prior to allocating the Data Block.'
        // we create empty array buffer instance first,
        // then allocate buffer, and finally set buffer to array buffer instance created earlier.

        // 1. create ArrayBuffer instance
        let isa = ArrayBufPayload(Rc::new(RefCell::new(ABufInner {
            content: ABufData::Empty,
            detached: false,
            array_list: Vec::new(),
            opaque,
        })));
        let obj = self
            .create_object_apply_ctor(
                new_target,
                if class_id == JS_CLASS_ARRAY_BUFFER {
                    ObjectPayload::ARRAY_BUFFER(isa)
                } else {
                    ObjectPayload::SHARED_ARRAY_BUFFER(isa)
                },
            )
            .map_err(|_| -1)?;

        // 2. allocate buffer and set buffer to instance
        let content = if byte_length == 0 {
            ABufData::Empty
        } else {
            // we are currently limited to 2 GB
            if byte_length > i32::MAX as usize {
                self.throw_range_error("invalid array buffer length");
                return Err(-1);
            }

            match (class_id, &self.rt().sab_funcs) {
                (JS_CLASS_SHARED_ARRAY_BUFFER, Some(fns)) => {
                    let mut data = (fns.sab_alloc)(fns.sab_opaque, byte_length)?;
                    unsafe {
                        std::ptr::write_bytes(data.as_mut_ptr(), 0, byte_length);
                    }
                    ABufData::Shared(Arc::new(RwLock::new(data)))
                }
                _ => {
                    let mut chunk = Vec::<u8>::new();
                    if chunk.try_reserve_exact(byte_length).is_err() {
                        self.throw_out_of_memory();
                        return Err(-1);
                    }
                    chunk.resize(byte_length, 0);
                    ABufData::Owned(chunk.into_boxed_slice())
                }
            }
        };

        if let Some(ab) = obj.0.payload.borrow().array_buffer() {
            let _d = ab.set_content(content);
            debug_assert!(matches!(_d, ABufData::Empty))
        }

        Ok(obj)
    }

    #[inline(always)]
    pub fn create_array_buffer(&self, byte_length: usize) -> Result<JSObject, i32> {
        self.create_array_buffer_ex(
            &JSValue::UNDEFINED,
            JS_CLASS_ARRAY_BUFFER,
            byte_length,
            std::ptr::null_mut(),
        )
    }

    #[inline(always)]
    pub fn create_array_buffer_with_data(
        &self,
        new_target: &JSValue,
        class_id: JSClassID,
        data: Box<[u8]>,
        opaque: *mut c_void,
    ) -> JSResult {
        self.do_create_array_buffer(new_target, class_id, ABufData::Owned(data), opaque)
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
            ABufData::Raw(data, data_len, free_func),
            opaque,
        )
    }
}

pub fn js_array_buffer_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let size = cc.arg(0).to_array_index(ctx)? as usize;
    ctx.create_array_buffer_ex(cc.this(), JS_CLASS_ARRAY_BUFFER, size, std::ptr::null_mut())
        .map(JSValue::Object)
}

pub fn js_shared_array_buffer_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let len = cc.arg(0).to_array_index(ctx)? as usize;
    ctx.create_array_buffer_ex(
        cc.this(),
        JS_CLASS_SHARED_ARRAY_BUFFER,
        len,
        std::ptr::null_mut(),
    )
    .map(JSValue::Object)
}

pub unsafe fn JS_DetachArrayBuffer(ctx: &JSContext, obj: &JSValue) {
    if obj.as_object_unchecked().class_id() != JS_CLASS_ARRAY_BUFFER {
        return;
    }

    if let Some(abuf) = obj
        .as_object_unchecked()
        .0
        .payload
        .borrow_mut()
        .array_buffer_mut()
    {
        if !abuf.0.borrow().detached {
            match abuf.content().deref() {
                ABufData::Raw(ptr, len, fin) => {
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
pub fn js_array_buffer_finalizer(rt: &mut JSRuntime, isa: ObjectPayload, id: usize) {
    let abuf = isa.array_buffer().unwrap();

    // free content
    match abuf.content().deref() {
        ABufData::Raw(p, len, Some(free_func)) => {
            free_func(rt, abuf.opaque(), p.as_ptr(), *len);
        }
        ABufData::Shared(a) => {
            if Arc::strong_count(&a) == 1 {
                if let Some(fns) = &rt.sab_funcs {
                    (fns.sab_free)(fns.sab_opaque, a.read().unwrap().len());
                }
            }
        }
        _ => {}
    }
}
