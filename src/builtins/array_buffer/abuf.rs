use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, RwLock},
};

use gc_lite::{GcRef, GcTracable, GcWeak};

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_SHARED_ARRAY_BUFFER,
    class::ClassID,
    context::{Context, ContextImpl},
    error::JSResult,
    function::CallContext,
    object::{ObjectImpl, ObjectPayload, Object},
    object_cast,
    runtime::RuntimeImpl,
    utils::unlikely,
    value::JSValue,
};

use super::{
    js_array_buffer_funcs, js_array_buffer_proto_funcs, js_shared_array_buffer_funcs,
    js_shared_array_buffer_proto_funcs,
};

pub type JSFreeArrayBufferDataFunc =
    fn(rt: &RuntimeImpl, opaque: *mut c_void, data: *mut u8, data_length: usize);

pub struct JSSharedArrayBufferFunctions {
    pub sab_alloc: fn(*mut c_void, usize) -> Result<Box<[u8]>, i32>,
    pub sab_free: fn(*mut c_void, usize),
    // pub sab_dup: fn(*mut c_void, *mut c_void),
    pub sab_opaque: *mut c_void,
}

#[derive(Debug)]
pub(crate) enum ABufData {
    Empty,
    Owned(Box<[u8]>),
    Shared(Arc<RwLock<Box<[u8]>>>),
    Raw(NonNull<u8>, usize, Option<JSFreeArrayBufferDataFunc>),
}

impl ABufData {
    /// for SharedArrayBuffer
    #[inline(always)]
    pub const fn is_shared(&self) -> bool {
        matches!(self, Self::Shared(_))
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

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        match self {
            Self::Empty => f(&[]),
            Self::Owned(b) => f(b.as_ref()),
            Self::Shared(a) => f(a.read().unwrap().as_ref()),
            Self::Raw(p, len, _) => f(unsafe { std::slice::from_raw_parts(p.as_ptr(), *len) }),
        }
    }

    pub(crate) fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
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
pub struct ArrayBufferPayload {
    data: ABufData,
    /// if is shared, the array buffer cannot be detached
    detached: bool,
    /// list of TypedArray objects that adopting this ArrayBuffer.
    array_list: Vec<GcWeak<ObjectImpl>>,
    opaque: *mut c_void,
}

unsafe impl GcTracable for ArrayBufferPayload {
    #[inline(always)]
    fn trace(&self, _: &mut gc_lite::GcTracer) {}
}

impl ArrayBufferPayload {
    #[inline(always)]
    pub const fn is_detached(&self) -> bool {
        self.detached
    }

    /// get capacity of buffer, count in bytes
    #[inline]
    pub fn byte_length(&self) -> usize {
        if unlikely(self.is_detached()) {
            0
        } else {
            self.data.len()
        }
    }

    pub fn add_typed_array_weakref(&mut self, t: GcWeak<ObjectImpl>) {
        self.array_list.push(t);
    }

    /// `obj_id` is the id of typed array object
    pub fn remove_typed_array_weakref(&mut self, rt: &RuntimeImpl, obj_id: usize) {
        let gcm = rt.gc_heap();

        self.array_list.retain(|w| {
            w.upgrade(gcm)
                .is_some_and(|a| unsafe { a.as_ref().id() != obj_id })
        });
    }

    /// get buffer content
    #[inline(always)]
    pub(crate) const fn data(&self) -> &ABufData {
        &self.data
    }

    // /// get buffer content
    // #[inline(always)]
    // pub(crate) const fn data_mut(&mut self) -> &mut ABufData {
    //     &mut self.data
    // }

    /// replace buffer data, returns old data
    pub(crate) const fn replace(&mut self, data: ABufData) -> ABufData {
        self.detached = false;
        std::mem::replace(&mut self.data, data)
    }

    #[inline(always)]
    pub const fn opaque(&self) -> *mut c_void {
        self.opaque
    }

    #[inline(always)]
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        self.data.with_bytes(f)
    }

    #[inline(always)]
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.data.with_bytes_mut(f)
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct ArrayBuffer(pub(crate) GcRef<ObjectImpl>);

object_cast!(
    ArrayBuffer,
    JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
);

impl ArrayBuffer {
    pub fn class_initialize(mut ctx: Context, class_id: ClassID) -> Result<Option<Object>, i32> {
        match class_id {
            JS_CLASS_ARRAY_BUFFER => {
                // ArrayBuffer
                let mut proto = ctx.create_base_object()?;
                proto.install_field_defs(&ctx, &js_array_buffer_proto_funcs)?;

                let mut ab_func = ctx.create_global_constructor_only(
                    "ArrayBuffer",
                    js_array_buffer_constructor,
                    1,
                    proto.into(),
                    0,
                )?;
                ab_func.install_field_defs(&ctx, &js_array_buffer_funcs)?;

                Ok(Some(proto))
            }
            JS_CLASS_SHARED_ARRAY_BUFFER => {
                // SharedArrayBuffer
                let mut proto = ctx.create_base_object()?;
                proto.install_field_defs(&ctx, &js_shared_array_buffer_proto_funcs)?;

                let mut sab_func = ctx.create_global_constructor_only(
                    "SharedArrayBuffer",
                    js_shared_array_buffer_constructor,
                    1,
                    proto.into(),
                    0,
                )?;
                sab_func.install_field_defs(&ctx, &js_shared_array_buffer_funcs)?;

                Ok(Some(proto))
            }
            _ => {
                unreachable!()
            }
        }
    }

    /// also used for SharedArrayBuffer
    pub fn class_finalize(rt: &mut RuntimeImpl, isa: &ObjectPayload, _id: usize) {
        let abuf = isa.array_buffer().unwrap();

        // free content
        match abuf.data() {
            ABufData::Raw(p, len, Some(free_func)) => {
                free_func(rt, abuf.opaque(), p.as_ptr(), *len);
            }
            ABufData::Shared(a) => {
                if Arc::strong_count(a) == 1
                    && let Some(fns) = &rt.sab_funcs
                {
                    (fns.sab_free)(fns.sab_opaque, a.read().unwrap().len());
                }
            }
            _ => {}
        }
    }

    /// Create new instance of class ARRAY_BUFFER or ARRAY_BUFFER_SHARED.
    fn create_internal(
        ctx: &ContextImpl,
        new_target: JSValue,
        shared: bool,
        data: ABufData,
        opaque: *mut c_void,
    ) -> Result<Self, i32> {
        // make ArrayBuffer payload
        let ab = {
            let inner = ArrayBufferPayload {
                data,
                detached: false,
                array_list: Vec::new(),
                opaque,
            };
            Box::new(inner)
        };

        // create array buffer or shared array buffer instance
        ctx.create_object_from_ctor(
            new_target,
            if !shared {
                ObjectPayload::ARRAY_BUFFER(ab)
            } else {
                ObjectPayload::SHARED_ARRAY_BUFFER(ab)
            },
        )
        .map(|o| ArrayBuffer(o.0))
        .map_err(|_| -1)
    }

    /// create empty array buffer instance with required capacity.
    /// `capacity` is counted in bytes.
    pub(crate) fn with_capacity_ex(
        ctx: &ContextImpl,
        new_target: JSValue,
        shared: bool,
        capacity: usize,
        opaque: *mut c_void,
    ) -> Result<Self, i32> {
        // Note: according to spec -
        // 'The new ArrayBuffer instance is created prior to allocating the Data Block.'
        // we create empty array buffer instance first,
        // then allocate buffer, and finally set buffer to array buffer instance created earlier.

        // 1. create ArrayBuffer instance
        let isa = Box::new(ArrayBufferPayload {
            data: ABufData::Empty,
            detached: false,
            array_list: Vec::new(),
            opaque,
        });

        let mut obj = ctx
            .create_object_from_ctor(
                new_target,
                if !shared {
                    ObjectPayload::ARRAY_BUFFER(isa)
                } else {
                    ObjectPayload::SHARED_ARRAY_BUFFER(isa)
                },
            )
            .map(|o| Self(o.0))
            .map_err(|_| -1)?;

        // 2. allocate buffer and set buffer to instance
        let data = if capacity == 0 {
            ABufData::Empty
        } else {
            // we are currently limited to 2 GB
            if capacity > i32::MAX as usize {
                ctx.throw_range_error("invalid array buffer length");
                return Err(-1);
            }

            match (shared, &ctx.rt_ref().sab_funcs) {
                (true, Some(fns)) => {
                    let mut data = (fns.sab_alloc)(fns.sab_opaque, capacity)?;
                    unsafe {
                        std::ptr::write_bytes(data.as_mut_ptr(), 0, capacity);
                    }
                    ABufData::Shared(Arc::new(RwLock::new(data)))
                }
                _ => {
                    let mut chunk = Vec::<u8>::new();
                    if chunk.try_reserve_exact(capacity).is_err() {
                        ctx.throw_out_of_memory();
                        return Err(-1);
                    }
                    chunk.resize(capacity, 0);
                    ABufData::Owned(chunk.into_boxed_slice())
                }
            }
        };

        let _d = obj.payload_mut().replace(data);
        debug_assert!(matches!(_d, ABufData::Empty));

        Ok(obj)
    }

    #[inline(always)]
    pub fn from_data(
        ctx: &ContextImpl,
        new_target: JSValue,
        shared: bool,
        data: Box<[u8]>,
        opaque: *mut c_void,
    ) -> Result<Self, i32> {
        Self::create_internal(ctx, new_target, shared, ABufData::Owned(data), opaque)
    }

    /// create empty array buffer instance with required capacity.
    /// `capacity` is counted in bytes.
    #[inline(always)]
    pub fn create(ctx: &ContextImpl, shared: bool, capacity: usize) -> Result<Self, i32> {
        Self::with_capacity_ex(
            ctx,
            JSValue::UNDEFINED,
            shared,
            capacity,
            std::ptr::null_mut(),
        )
    }

    /// get array buffer payload
    #[inline(always)]
    pub fn payload(&self) -> &ArrayBufferPayload {
        unsafe { self.0.as_ref().payload().array_buffer().unwrap() }
    }

    /// get array buffer payload
    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut ArrayBufferPayload {
        unsafe { self.0.as_mut().payload_mut().array_buffer_mut().unwrap() }
    }

    pub fn is_detached(&self) -> bool {
        self.payload().is_detached()
    }

    /// detatch array buffer
    pub fn detatch(&mut self, ctx: &ContextImpl) {
        let abuf = self.payload_mut();

        if !abuf.is_detached() {
            let data0 = abuf.replace(ABufData::Empty);

            if let ABufData::Raw(ptr, len, fin) = data0
                && let Some(f) = fin
            {
                f(ctx.rt_ref(), abuf.opaque(), ptr.as_ptr(), len);
            }

            for w in abuf.array_list.drain(..) {
                if let Some(o) = w.upgrade(ctx.rt_ptr().gc_heap()) {
                    // typed array or dataview
                    // Note: the typed array length and offset fields are not modified. set count to 0
                    if let Some(ta) = Object::from(o).as_TypedArray_mut() {
                        ta.payload_mut().set_count(0);
                    }
                }
            }

            abuf.detached = true;
        }
    }
}

pub fn js_array_buffer_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let size = cc.arg(0).to_index(ctx)? as usize;
    ArrayBuffer::with_capacity_ex(ctx, *cc.this(), false, size, std::ptr::null_mut())
        .map(|x| x.into())
}

pub fn js_shared_array_buffer_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let size = cc.arg(0).to_index(ctx)? as usize;
    ArrayBuffer::with_capacity_ex(ctx, *cc.this(), true, size, std::ptr::null_mut())
        .map(|x| x.into())
}

// also used for SharedArrayBuffer
#[deprecated(note = "use ArrayBuffer::class_finalize() instead")]
pub fn js_array_buffer_finalizer(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize) {
    ArrayBuffer::class_finalize(rt, isa, id);
}
