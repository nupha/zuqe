// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    ffi::c_void,
    ops::DerefMut,
    ptr::NonNull,
    sync::{Arc, RwLock},
};

use gc_lite::{GcTrace, GcTraceCtx, GcWeak};

use crate::{
    builtins::typed_array::TypedArray,
    context::ScopedContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::CallContext,
    object::{ArrayBuffer, Object, ObjectImpl, ObjectPayload},
    runtime::RuntimeImpl,
    utils::unlikely,
    value::JSValue,
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

    fn as_ptr(&self) -> *const u8 {
        match self {
            Self::Empty => std::ptr::null(),
            Self::Owned(b) => b.as_ptr(),
            Self::Raw(p, _, _) => p.as_ptr(),
            Self::Shared(_) => {
                unreachable!("SharedArrayBuffer cannot accessed underlaying bytes raw pointer");
                // a.read().unwrap().as_ptr()
            }
        }
    }

    /// get underlaying bytes of ArrayBuffer. Not for SharedArrayBuffer.
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Owned(b) => b,
            Self::Raw(p, n, _) => unsafe { std::slice::from_raw_parts(p.as_ptr(), *n) },
            Self::Shared(_) => {
                unreachable!(
                    "SharedArrayBuffer cannot accessed underlaying bytes, use with_bytes() instead"
                )
            }
        }
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
    /// list of TypedArray objects (weakref) that adopting this ArrayBuffer.
    typed_array_list: Vec<GcWeak<ObjectImpl>>,
    /// whether the buffer is resizable (created with a `maxByteLength` option).
    resizable: bool,
    /// maximum byte length for a resizable buffer; 0 if not resizable.
    max_byte_length: usize,
    opaque: *mut c_void,
}

impl GcTrace for ArrayBufferPayload {
    #[inline(always)]
    fn trace(&self, _: &mut GcTraceCtx) {}
}

impl ArrayBufferPayload {
    #[inline(always)]
    pub const fn is_detached(&self) -> bool {
        self.detached
    }

    /// whether the buffer is resizable (created with a `maxByteLength` option).
    #[inline(always)]
    pub const fn is_resizable(&self) -> bool {
        self.resizable
    }

    /// maximum byte length for a resizable buffer; 0 if not resizable.
    #[inline(always)]
    pub const fn max_byte_length(&self) -> usize {
        self.max_byte_length
    }

    /// Throw a TypeError if the buffer has been detached.
    #[inline]
    pub fn ensure_attached(&self, ctx: &mut ScopedContext) -> Result<(), i32> {
        if unlikely(self.is_detached()) {
            Err(JS_ThrowTypeErrorDetachedArrayBuffer(ctx))
        } else {
            Ok(())
        }
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

    pub fn add_typed_array_weakref(&mut self, w: GcWeak<ObjectImpl>) {
        self.typed_array_list.push(w);
    }

    /// get buffer content
    #[inline(always)]
    pub(crate) const fn data(&self) -> &ABufData {
        &self.data
    }

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
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }

    /// Get underlaying bytes of ArrayBuffer. Not for SharedArrayBuffer.
    #[inline(always)]
    pub fn bytes(&self) -> &[u8] {
        self.data.bytes()
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

impl ArrayBuffer {
    /// also used for SharedArrayBuffer
    pub fn class_finalize(rt: &mut RuntimeImpl, isa: &ObjectPayload, _id: usize) {
        // free data content
        let abuf = isa.array_buffer().unwrap();
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

    /// Create ARRAY_BUFFER or ARRAY_BUFFER_SHARED instance.
    fn create_internal(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        data: ABufData,
        opaque: *mut c_void,
    ) -> Result<Self, i32> {
        let shared = matches!(data, ABufData::Shared(_));

        // make ArrayBuffer payload
        let ab = Box::new(ArrayBufferPayload {
            data,
            detached: false,
            typed_array_list: Vec::new(),
            resizable: false,
            max_byte_length: 0,
            opaque,
        });

        // create array buffer or shared array buffer instance
        ctx.create_instance_with_ctor(
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
        ctx: &mut ScopedContext,
        new_target: JSValue,
        shared: bool,
        capacity: usize,
        opaque: *mut c_void,
    ) -> Result<Self, i32> {
        Self::with_capacity_max(ctx, new_target, shared, capacity, 0, opaque)
    }

    /// Like [`with_capacity_ex`] but accepts `max_byte_length` for a
    /// *resizable* (growable) buffer. When `max_byte_length == 0` the buffer is
    /// fixed-size (non-resizable). For a `SharedArrayBuffer`, a non-zero
    /// `max_byte_length` marks it growable per the resizable SharedArrayBuffer
    /// semantics.
    pub(crate) fn with_capacity_max(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        shared: bool,
        capacity: usize,
        max_byte_length: usize,
        opaque: *mut c_void,
    ) -> Result<Self, i32> {
        // Note: according to spec -
        // 'The new ArrayBuffer instance is created prior to allocating the Data Block.'
        // we create empty array buffer instance first,
        // then allocate buffer, and finally set buffer to array buffer instance created earlier.

        // 1. create ArrayBuffer instance
        let resizable = max_byte_length > 0;
        let isa = Box::new(ArrayBufferPayload {
            data: ABufData::Empty,
            detached: false,
            typed_array_list: Vec::new(),
            resizable,
            max_byte_length,
            opaque,
        });
        let mut obj = ctx
            .create_instance_with_ctor(
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
                return Err(ctx.throw_range_error("invalid array buffer length"));
            }

            match (shared, &ctx.rt().sab_funcs) {
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
                        return Err(ctx.throw_out_of_memory());
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

    /// create a resizable ArrayBuffer instance with initial byte length
    /// `byte_length` and max byte length `max_byte_length`.
    pub(crate) fn with_capacity_resizable(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        byte_length: usize,
        max_byte_length: usize,
    ) -> Result<Self, i32> {
        let mut obj =
            Self::with_capacity_ex(ctx, new_target, false, byte_length, std::ptr::null_mut())?;
        let p = obj.payload_mut();
        p.resizable = true;
        p.max_byte_length = max_byte_length;
        Ok(obj)
    }

    /// Resize a resizable ArrayBuffer to `new_length` bytes, updating all
    /// dependent TypedArray views (their lengths shrink/grow, or they go
    /// out-of-bounds). A no-op when the length is unchanged.
    pub fn resize(&mut self, ctx: &mut ScopedContext, new_length: usize) -> Result<(), i32> {
        let rt = ctx.rt_ptr();
        let heap = rt.gc_heap();

        let abuf = self.payload_mut();
        abuf.ensure_attached(ctx)?;
        if unlikely(!abuf.is_resizable()) {
            return Err(ctx.throw_type_error("ArrayBuffer is not resizable"));
        }
        if new_length > abuf.max_byte_length() {
            return Err(ctx.throw_range_error("invalid array buffer length"));
        }
        let old_length = abuf.byte_length();
        if new_length == old_length {
            return Ok(());
        }

        // Reallocate the backing block to the exact new size, preserving the
        // existing contents and zero-filling the newly grown region.
        let data = std::mem::replace(&mut abuf.data, ABufData::Empty);
        abuf.data = match data {
            ABufData::Empty => {
                if new_length == 0 {
                    ABufData::Empty
                } else {
                    ABufData::Owned(vec![0u8; new_length].into_boxed_slice())
                }
            }
            ABufData::Owned(b) => {
                let mut v = b.into_vec();
                v.resize(new_length, 0);
                ABufData::Owned(v.into_boxed_slice())
            }
            ABufData::Shared(_) => {
                return Err(ctx.throw_type_error("cannot resize a SharedArrayBuffer"));
            }
            ABufData::Raw(_, _, _) => {
                return Err(ctx.throw_type_error("cannot resize a raw ArrayBuffer"));
            }
        };

        // Update every live TypedArray view.
        for w in abuf.typed_array_list.iter() {
            let Some(o) = w.upgrade(heap) else {
                continue;
            };
            let mut o = Object::from(o.into_raw());
            let element_size = 1usize << TypedArray::SIZE_LOG2(o.class_id());
            let Some(ta) = o.payload_mut().typed_array_mut() else {
                continue;
            };
            let offset = ta.offset();
            if ta.is_length_tracked() {
                // Length-tracked: follows the buffer size; out-of-bounds only
                // when the view's start lies past the new end of the buffer.
                let oob = offset > new_length;
                let count = if oob {
                    0
                } else {
                    (new_length - offset) / element_size
                };
                ta.set_count(count);
                ta.set_byte_length(count * element_size);
                ta.set_out_of_bounds(oob);
            } else {
                // Fixed-length: nominal length is preserved; the view only goes
                // out-of-bounds when the buffer shrinks past its end.
                let oob = offset + ta.raw_byte_length() > new_length;
                ta.set_out_of_bounds(oob);
            }
        }

        Ok(())
    }

    /// Grow a *growable* SharedArrayBuffer to `new_length` bytes, preserving the
    /// existing contents and zero-filling the newly grown region. Equivalent to
    /// `SharedArrayBuffer.prototype.grow`. Throws `TypeError` when the buffer is
    /// not growable, or `RangeError` when `new_length > maxByteLength`.
    pub fn grow(&mut self, ctx: &mut ScopedContext, new_length: usize) -> Result<(), i32> {
        let rt = ctx.rt_ptr();
        let heap = rt.gc_heap();

        let abuf = self.payload_mut();
        abuf.ensure_attached(ctx)?;

        if unlikely(!abuf.is_resizable()) {
            return Err(ctx.throw_type_error("SharedArrayBuffer is not growable"));
        }
        if new_length > abuf.max_byte_length() {
            return Err(ctx.throw_range_error("invalid array buffer length"));
        }
        let old_length = abuf.byte_length();
        if new_length == old_length {
            return Ok(());
        }

        // Grow the shared backing block under the write lock.
        let data = std::mem::replace(&mut abuf.data, ABufData::Empty);
        abuf.data = match data {
            ABufData::Shared(shared) => {
                let mut guard = shared.write().unwrap();
                let mut v = std::mem::take(&mut *guard).into_vec();
                v.resize(new_length, 0);
                ABufData::Shared(Arc::new(RwLock::new(v.into_boxed_slice())))
            }
            _ => {
                // Non-shared buffers use `resize`, not `grow`.
                return Err(ctx.throw_type_error("grow is only valid on SharedArrayBuffer"));
            }
        };

        // Update every live TypedArray view (mirrors `resize`).
        for w in abuf.typed_array_list.iter() {
            let Some(o) = w.upgrade(heap) else {
                continue;
            };
            let mut o = Object::from(o.into_raw());
            let element_size = 1usize << TypedArray::SIZE_LOG2(o.class_id());
            let Some(ta) = o.payload_mut().typed_array_mut() else {
                continue;
            };
            let offset = ta.offset();
            if ta.is_length_tracked() {
                let oob = offset > new_length;
                let count = if oob {
                    0
                } else {
                    (new_length - offset) / element_size
                };
                ta.set_count(count);
                ta.set_byte_length(count * element_size);
                ta.set_out_of_bounds(oob);
            } else {
                let oob = offset + ta.raw_byte_length() > new_length;
                ta.set_out_of_bounds(oob);
            }
        }

        Ok(())
    }

    /// create array buffer with owned bytes.
    #[inline(always)]
    pub fn from_bytes(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        bytes: Box<[u8]>,
        opaque: *mut c_void,
    ) -> Result<Self, i32> {
        Self::create_internal(ctx, new_target, ABufData::Owned(bytes), opaque)
    }

    /// create empty array buffer instance with required capacity.
    /// `capacity` is counted in bytes.
    #[inline(always)]
    pub fn create(ctx: &mut ScopedContext, shared: bool, capacity: usize) -> Result<Self, i32> {
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
        unsafe { self.0.as_ref().payload() }.array_buffer().unwrap()
    }

    /// get array buffer payload
    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut ArrayBufferPayload {
        self.deref_mut().payload_mut().array_buffer_mut().unwrap()
    }

    pub fn is_detached(&self) -> bool {
        self.payload().is_detached()
    }

    /// Throw a TypeError if the buffer has been detached.
    #[inline]
    pub fn ensure_attached(&self, ctx: &mut ScopedContext) -> Result<(), i32> {
        self.payload().ensure_attached(ctx)
    }

    /// detatch array buffer
    pub fn detatch(&mut self, ctx: &mut ScopedContext) {
        let abuf = self.payload_mut();

        if !abuf.is_detached() {
            let data0 = abuf.replace(ABufData::Empty);

            if let ABufData::Raw(ptr, len, fin) = data0
                && let Some(f) = fin
            {
                f(ctx.rt(), abuf.opaque(), ptr.as_ptr(), len);
            }

            let rt = ctx.rt_ptr();
            let heap = rt.gc_heap();

            for o in abuf
                .typed_array_list
                .drain(..)
                .filter_map(|w| w.upgrade(heap))
            {
                let mut o = Object::from(o.into_raw());
                // the typed array length and offset fields are not modified, only set count to 0
                debug_assert!(
                    o.payload_mut().typed_array_mut().is_some(),
                    "expect TypedArray object"
                );
                if let Some(ta) = o.payload_mut().typed_array_mut() {
                    ta.set_count(0);
                }
            }

            abuf.detached = true;
        }
    }
}

pub fn js_array_buffer_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let byte_length = cc.arg(0).to_index(ctx)? as usize;

    let mut resizable = false;
    let mut max_byte_length = 0usize;
    if cc.length() >= 2
        && let Some(options) = cc.arg(1).as_object()
    {
        let mbl_val = options.get_property_str(ctx, "maxByteLength")?;
        if !mbl_val.is_undefined() {
            let mbl = mbl_val.to_index(ctx)? as usize;
            if byte_length > mbl {
                return Err(ctx.throw_range_error("invalid array buffer length"));
            }
            if mbl > i32::MAX as usize {
                return Err(ctx.throw_range_error("invalid array buffer length"));
            }
            resizable = true;
            max_byte_length = mbl;
        }
    }

    if resizable {
        ArrayBuffer::with_capacity_resizable(ctx, cc.this(), byte_length, max_byte_length)
            .map(JSValue::from)
    } else {
        ArrayBuffer::with_capacity_ex(ctx, cc.this(), false, byte_length, std::ptr::null_mut())
            .map(JSValue::from)
    }
}

pub fn js_shared_array_buffer_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let size = cc.arg(0).to_index(ctx)? as usize;

    let mut max_byte_length = 0usize;
    if cc.length() >= 2
        && let Some(options) = cc.arg(1).as_object()
    {
        let mbl_val = options.get_property_str(ctx, "maxByteLength")?;
        if !mbl_val.is_undefined() {
            let mbl = mbl_val.to_index(ctx)? as usize;
            if size > mbl {
                return Err(ctx.throw_range_error("invalid array buffer length"));
            }
            if mbl > i32::MAX as usize {
                return Err(ctx.throw_range_error("invalid array buffer length"));
            }
            max_byte_length = mbl;
        }
    }

    ArrayBuffer::with_capacity_max(
        ctx,
        cc.this(),
        true,
        size,
        max_byte_length,
        std::ptr::null_mut(),
    )
    .map(JSValue::from)
}

// // also used for SharedArrayBuffer
// #[deprecated(note = "use ArrayBuffer::class_finalize() instead")]
// pub fn js_array_buffer_finalizer(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize) {
//     ArrayBuffer::class_finalize(rt, isa, id);
// }
