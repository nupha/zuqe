// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::UnsafeCell;

use gc_lite::{GcRef, GcTrace, GcTraceCtx, GcWeak};

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::Atom,
    builtins::array_buffer::ArrayBuffer,
    class::ClassID,
    context::ScopedContext,
    error::JS_ThrowTypeErrorDetachedArrayBuffer,
    function::{Callback, NativeFunction},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object, ObjectImpl, ObjectPayload},
    object_cast,
    runtime::RuntimeImpl,
    value::JSValue,
};

use super::{
    js_typed_array_base_constructor, js_typed_array_base_funcs, js_typed_array_base_proto_funcs,
    js_typed_array_constructor,
};

pub const JS_TYPED_ARRAY_COUNT: usize =
    JS_CLASS_FLOAT64_ARRAY as usize - JS_CLASS_UINT8C_ARRAY as usize + 1;

#[derive(Debug)]
pub struct TypedArrayPayload {
    /// underlaying array buffer object.
    pub(super) array_buffer: ArrayBuffer,
    /// start offset in array buffer
    pub(super) offset: u32,
    /// number of bytes from array buffer
    pub(super) byte_length: u32,
    /// number of elements in typed array
    count: u32,

    /// back reference to the TypedArray/DataView object.
    #[deprecated(note = "seems no use")]
    obj: UnsafeCell<GcWeak<ObjectImpl>>,
}

impl GcTrace for TypedArrayPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.array_buffer.trace(tr);
    }
}

impl TypedArrayPayload {
    /// create typed array payload with underlying array buffer `buffer`,
    /// offset in array buffer and type array element count.
    fn new(
        array_buffer: ArrayBuffer,
        class_id: ClassID,
        offset: usize,
        count: usize,
    ) -> Result<Self, i32> {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY
        ));

        let abuf = array_buffer.payload();
        let byte_length = abuf.byte_length();
        let size_log2 = TypedArray::SIZE_LOG2(class_id);

        debug_assert!(!abuf.is_detached());
        debug_assert!(
            byte_length & ((1 << size_log2) - 1) == 0,
            "byte_length must be aligned to element size"
        );
        debug_assert!(
            offset & ((1 << size_log2) - 1) == 0,
            "offset must be aligned to element size"
        );
        debug_assert!(offset + (count << size_log2) <= byte_length);

        let ta = Self {
            array_buffer,
            offset: offset as u32,
            byte_length: (count as u32) << size_log2,
            count: count as u32,
            obj: UnsafeCell::new(GcWeak::default()), // empty weakref
        };

        Ok(ta)
    }

    /// get underlaying array buffer object.
    #[inline(always)]
    pub const fn array_buffer(&self) -> ArrayBuffer {
        self.array_buffer
    }

    /// check if `self` TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        self.array_buffer().payload().is_detached()
    }

    #[inline(always)]
    pub const fn byte_length(&self) -> usize {
        self.byte_length as _
    }

    #[inline(always)]
    pub const fn count(&self) -> usize {
        self.count as _
    }

    #[inline(always)]
    pub const fn set_count(&mut self, n: usize) {
        self.count = n as _;
    }

    /// Get underlaying bytes raw pointer of typed array. Starting from base offset.
    #[inline(always)]
    fn as_ptr(&self) -> *const u8 {
        let ptr = self.array_buffer().payload().as_ptr();
        if !ptr.is_null() {
            let base = self.offset as usize;
            unsafe { ptr.add(base) }
        } else {
            ptr
        }
    }

    /// Get underlaying bytes
    pub(crate) fn bytes(&self) -> &[u8] {
        let ptr = self.as_ptr();
        if !ptr.is_null() {
            let len = self.byte_length as usize;
            unsafe { std::slice::from_raw_parts(ptr, len) }
        } else {
            &[]
        }
    }

    /// run closure with underlaying bytes of typed array.
    pub(crate) fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload()
            .with_bytes(|buf| f(&buf[base..base + len]))
    }

    /// run closure with underlaying bytes of typed array.
    pub(super) fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload_mut()
            .with_bytes_mut(|buf| f(&mut buf[base..base + len]))
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct TypedArray(GcRef<ObjectImpl>);

object_cast!(
    TypedArray,
    JS_CLASS_UINT8C_ARRAY
        | JS_CLASS_INT8_ARRAY
        | JS_CLASS_UINT8_ARRAY
        | JS_CLASS_INT16_ARRAY
        | JS_CLASS_UINT16_ARRAY
        | JS_CLASS_INT32_ARRAY
        | JS_CLASS_UINT32_ARRAY
        | JS_CLASS_BIG_INT64_ARRAY
        | JS_CLASS_BIG_UINT64_ARRAY
        | JS_CLASS_FLOAT32_ARRAY
        | JS_CLASS_FLOAT64_ARRAY
);

impl TypedArray {
    /// log2 size for each class_id
    const TYPED_ARRAY_SIZE_LOG2: [u8; JS_TYPED_ARRAY_COUNT] = [
        0, // JS_CLASS_UINT8C_ARRAY
        0, // JS_CLASS_INT8_ARRAY
        0, // JS_CLASS_UINT8_ARRAY
        1, // JS_CLASS_INT16_ARRAY
        1, // JS_CLASS_UINT16_ARRAY
        2, // JS_CLASS_INT32_ARRAY
        2, // JS_CLASS_UINT32_ARRAY
        3, // JS_CLASS_BIG_INT64_ARRAY
        3, // JS_CLASS_BIG_UINT64_ARRAY
        2, // JS_CLASS_FLOAT32_ARRAY
        3, // JS_CLASS_FLOAT64_ARRAY
    ];

    /// JS_CLASS_DATAVIEW is not allowed.
    #[inline(always)]
    pub(crate) fn SIZE_LOG2(class_id: ClassID) -> u8 {
        debug_assert!(
            (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&class_id),
            "class_id={class_id}"
        );
        Self::TYPED_ARRAY_SIZE_LOG2[(class_id - JS_CLASS_UINT8C_ARRAY) as usize]
    }

    pub(crate) fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY
        ));
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        let mut proto = ctx.create_base_object().unwrap();
        proto.install_fields(ctx, js_typed_array_base_proto_funcs)?;

        // TypedArray.prototype.toString must be the same object as Array.prototype.toString
        let to_string = ctx.array_proto().get_property(ctx, Atom::CONST_toString)?;

        // XXX: should use alias method in JSCFunctionListEntry
        proto.define_property_value(
            ctx,
            Atom::CONST_toString,
            to_string,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;

        let mut ta_func_proto = ctx.create_native_function(
            Callback::Func(js_typed_array_base_constructor),
            "TypedArray",
            0,
            0,
        )?;
        ta_func_proto.install_fields(ctx, js_typed_array_base_funcs)?;
        ctx.bind_ctor_proto(ta_func_proto.into(), proto)?;

        let mut global = ctx.global_obj();

        // make proto for variant typed array classes
        for cid in JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY {
            let nby = 1 << Self::SIZE_LOG2(cid);
            let name = Atom::from_id(
                Atom::CONST_Uint8ClampedArray.id() + (cid - JS_CLASS_UINT8C_ARRAY) as u32,
            );

            let mut ta_proto = ctx.create_base_object_with_proto(Some(proto))?;
            ta_proto.define_property_value_str(ctx, "BYTES_PER_ELEMENT", JSValue::Int(nby), 0)?;
            ctx.class_slots[cid as usize].proto = Some(ta_proto);

            let mut ta_ctor: Object = NativeFunction::create(
                ctx,
                Callback::Constructor(js_typed_array_constructor),
                name,
                3,
                cid as i16, // magic
                ta_func_proto.into(),
                None,
            )?
            .into();
            ctx.bind_ctor_proto(ta_ctor, ta_proto)?;
            ta_ctor.define_property_value_str(ctx, "BYTES_PER_ELEMENT", JSValue::Int(nby), 0)?;
            ctx.class_slots[cid as usize].ctor = Some(ta_ctor);

            global.define_property_value(
                ctx,
                name,
                ta_ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
        }

        Ok(())
    }

    pub(crate) const fn class_finalize(_rt: &mut RuntimeImpl, _isa: &ObjectPayload, _id: usize) {
        // Hint: TypedArray underlaying ArrayBuffer object could have been disposed,
        // calling ta.array_buffer() like following may cause undefined behaviour.
        // if let Some(ta) = isa.typed_array() {
        //     ta.array_buffer()
        //         .payload_mut()
        //         .remove_typed_array_weakref(rt, id);
        // }
    }

    /// create TypedArray instance with given ArrayBuffer, offset and count.
    ///
    /// Note: `count` is number of elements, not bytes.
    fn create(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        array_buffer: ArrayBuffer,
        offset: usize,
        count: usize,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        let ta = Box::new(TypedArrayPayload::new(
            array_buffer,
            class_id,
            offset,
            count,
        )?);

        let isa = match class_id {
            JS_CLASS_UINT8C_ARRAY => ObjectPayload::UINT8C_ARRAY(ta),
            JS_CLASS_UINT8_ARRAY => ObjectPayload::UINT8_ARRAY(ta),
            JS_CLASS_INT8_ARRAY => ObjectPayload::INT8_ARRAY(ta),
            JS_CLASS_UINT16_ARRAY => ObjectPayload::UINT16_ARRAY(ta),
            JS_CLASS_INT16_ARRAY => ObjectPayload::INT16_ARRAY(ta),
            JS_CLASS_INT32_ARRAY => ObjectPayload::INT32_ARRAY(ta),
            JS_CLASS_UINT32_ARRAY => ObjectPayload::UINT32_ARRAY(ta),
            JS_CLASS_FLOAT32_ARRAY => ObjectPayload::FLOAT32_ARRAY(ta),
            JS_CLASS_FLOAT64_ARRAY => ObjectPayload::FLOAT64_ARRAY(ta),
            c @ (JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY) => {
                ctx.throw_type_error(&format!(
                    "typed array for {} is unsupported",
                    if c == JS_CLASS_BIG_INT64_ARRAY {
                        "BigInt"
                    } else {
                        "BigUint"
                    }
                ));
                return Err(-1);
            }
            c => {
                unreachable!("{c}")
            }
        };

        let ta = ctx
            .create_instance_with_ctor(new_target, isa)
            .map_err(|_| -1)?;

        // establish mutual references between TypedArray <==> ArrayBuffer
        {
            let p = ta.payload().typed_array().unwrap();
            let w = ta.downgrade();
            p.array_buffer().payload_mut().add_typed_array_weakref(w);
            unsafe {
                *p.obj.get() = w;
            }
        }

        Ok(Self(ta.0))
    }

    /// create TypedArray instance with empty ArrayBuffer.
    ///
    /// Note: `count` is number of elements, not bytes.
    pub(crate) fn create_empty(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        count: usize,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        let abuf = ArrayBuffer::create(ctx, false, count << TypedArray::SIZE_LOG2(class_id))?;
        Self::create(ctx, new_target, abuf, 0, count, class_id)
    }

    /// Create new TypedArray instance from given `source` value. The source could be:
    /// - a TypedArray object: create TypedArray with content copied from source;
    /// - an ArrayBuffer or SharedArrayBuffer object: create TypedArray with array buffer;
    /// - an enumerable object: create TypedArray with content from enumerated index property vals.
    pub(crate) fn create_from(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        source: JSValue,
        offset: usize,
        length: usize,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        let size_log2 = Self::SIZE_LOG2(class_id);

        match source {
            JSValue::Object(o) => {
                match o.class_id() {
                    JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER => {
                        Self::create(ctx, new_target, ArrayBuffer(o.0), offset, length, class_id)
                    }
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        Self(o.0).create_to_new(ctx, new_target, class_id)
                    }
                    _ => {
                        // if source is iterable, retrieve values and create new typed array.
                        #[cfg(feature = "builtin_symbol")]
                        if let Some(it) = source.iter_maybe(ctx)? {
                            // Note: according to ECMAScript specification,
                            // must collect values from iterator first, then do number convert.
                            let mut vals = Vec::<JSValue>::with_capacity(64);
                            for v in it {
                                vals.push(v?);
                            }

                            let count = vals.len();
                            let mut bytes = Vec::<u8>::with_capacity(count << size_log2);

                            for val in vals {
                                let n = val.to_number(ctx)?;
                                match class_id {
                                    JS_CLASS_UINT8C_ARRAY => {
                                        bytes.push(n.to_u8_clamped());
                                    }
                                    JS_CLASS_INT8_ARRAY
                                    | JS_CLASS_UINT8_ARRAY
                                    | JS_CLASS_INT16_ARRAY
                                    | JS_CLASS_UINT16_ARRAY
                                    | JS_CLASS_INT32_ARRAY
                                    | JS_CLASS_UINT32_ARRAY => {
                                        let n = n.to_i32();
                                        match class_id {
                                            JS_CLASS_INT8_ARRAY => {
                                                bytes.push(n as u8);
                                            }
                                            JS_CLASS_UINT8_ARRAY => {
                                                bytes.push(n as u8);
                                            }
                                            JS_CLASS_INT16_ARRAY => {
                                                bytes.extend((n as i16).to_ne_bytes());
                                            }
                                            JS_CLASS_UINT16_ARRAY => {
                                                bytes.extend((n as u16).to_ne_bytes());
                                            }
                                            JS_CLASS_INT32_ARRAY => {
                                                bytes.extend(n.to_ne_bytes());
                                            }
                                            JS_CLASS_UINT32_ARRAY => {
                                                bytes.extend((n as u32).to_ne_bytes());
                                            }
                                            _ => {
                                                unreachable!();
                                            }
                                        }
                                    }
                                    JS_CLASS_FLOAT32_ARRAY | JS_CLASS_FLOAT64_ARRAY => {
                                        let n = n.to_f64();
                                        match class_id {
                                            JS_CLASS_FLOAT32_ARRAY => {
                                                bytes.extend((n as f32).to_ne_bytes());
                                            }
                                            JS_CLASS_FLOAT64_ARRAY => {
                                                bytes.extend(n.to_ne_bytes());
                                            }
                                            _ => {
                                                unreachable!();
                                            }
                                        }
                                    }
                                    JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                                        unimplemented!("BigInt64 and BigUint64 is not supported");
                                    }
                                    c => {
                                        unreachable!("class_id: {c}");
                                    }
                                }
                            }

                            let abuf = ArrayBuffer::from_bytes(
                                ctx,
                                JSValue::UNDEFINED,
                                bytes.into_boxed_slice(),
                                std::ptr::null_mut(),
                            )?;
                            return Self::create(ctx, JSValue::UNDEFINED, abuf, 0, count, class_id);
                        }

                        // Slow path: get indexed property from source then set to new typed array.
                        let len = o.get_length_prop(ctx)? as usize;
                        let abuf = ArrayBuffer::create(ctx, false, len << size_log2)?;
                        let mut new_ta = Self::create(ctx, new_target, abuf, 0, len, class_id)?;

                        for i in 0..len {
                            let v = o.get_property_u32(ctx, i as u32)?;
                            new_ta.set_property_u32(ctx, i as u32, v)?;
                        }

                        Ok(new_ta)
                    }
                }
            }
            _ => Err(-1),
        }
    }

    fn create_with_vec<D>(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        mut data: Vec<D>,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        let count = data.len();

        let data = unsafe {
            data.shrink_to_fit();
            let len = data.len();
            let cap = data.capacity();
            let ptr = data.as_mut_ptr() as *mut u8;
            let elem_size = std::mem::size_of::<D>();
            std::mem::forget(data);
            Vec::from_raw_parts(ptr, len * elem_size, cap * elem_size).into_boxed_slice()
        };

        let abuf = ArrayBuffer::from_bytes(ctx, JSValue::UNDEFINED, data, std::ptr::null_mut())?;
        Self::create(ctx, new_target, abuf, 0, count, class_id)
    }

    /// create Uint8Array instance with vec data.
    pub fn create_from_vec_u8(ctx: &mut ScopedContext, data: Vec<u8>) -> Result<Self, i32> {
        Self::create_with_vec(ctx, JSValue::UNDEFINED, data, JS_CLASS_UINT8_ARRAY)
    }

    /// create Int16Array instance with vec data.
    pub fn create_from_vec_i16(ctx: &mut ScopedContext, data: Vec<i16>) -> Result<Self, i32> {
        Self::create_with_vec(ctx, JSValue::UNDEFINED, data, JS_CLASS_INT16_ARRAY)
    }

    /// create Int32Array instance with vec data.
    pub fn create_from_vec_i32(ctx: &mut ScopedContext, data: Vec<i32>) -> Result<Self, i32> {
        Self::create_with_vec(ctx, JSValue::UNDEFINED, data, JS_CLASS_INT32_ARRAY)
    }

    /// create Uint32Array instance with vec data.
    pub fn create_from_vec_u32(ctx: &mut ScopedContext, data: Vec<u32>) -> Result<Self, i32> {
        Self::create_with_vec(ctx, JSValue::UNDEFINED, data, JS_CLASS_UINT32_ARRAY)
    }

    /// create Float32Array instance with vec data.
    pub fn create_from_vec_f32(ctx: &mut ScopedContext, data: Vec<f32>) -> Result<Self, i32> {
        Self::create_with_vec(ctx, JSValue::UNDEFINED, data, JS_CLASS_FLOAT32_ARRAY)
    }

    #[inline(always)]
    pub fn payload(&self) -> &TypedArrayPayload {
        self.0.payload().typed_array().unwrap()
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut TypedArrayPayload {
        self.0.payload_mut().typed_array_mut().unwrap()
    }

    /// check if the TypedArray is detached.
    #[inline(always)]
    pub fn is_detached(&self) -> bool {
        self.payload().is_detached()
    }

    /// Get elements count of typed array.
    /// If underlaying array buffer is detached, returns 0.
    #[inline]
    pub fn length(&self) -> usize {
        #[cfg(debug_assertions)]
        if self.is_detached() {
            debug_assert_eq!(self.payload().count(), 0);
            0
        } else {
            self.payload().count()
        }

        #[cfg(not(debug_assertions))]
        self.payload().count()
    }

    pub fn byte_length(&self) -> usize {
        if self.is_detached() {
            0
        } else {
            self.payload().byte_length()
        }
    }

    /// Get elements count of typed array. (calculated by underlying array buffer space).
    /// Usually this equals to that of .length(), only in case when array buffer is detached,
    /// this will still return rated count, but .length() will return 0.
    pub fn capacity(&self) -> usize {
        let size_log2 = Self::SIZE_LOG2(self.class_id());
        self.byte_length() >> size_log2
    }

    /// get underlying array buffer object.
    #[inline(always)]
    pub fn array_buffer(&self) -> ArrayBuffer {
        self.payload().array_buffer()
    }

    /// Get underlying array buffer byte offset.
    /// When underlaying array buffer is detached, returns 0.
    pub(super) fn array_buffer_byte_offset(&self) -> usize {
        if self.is_detached() {
            0
        } else {
            self.payload().offset as usize
        }
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        self.payload().with_bytes(f)
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.payload_mut().with_bytes_mut(f)
    }

    /// if nth is out of range, return Err(-1)
    pub(crate) fn set_element_int(&mut self, nth: usize, val: i32) -> Result<(), i32> {
        if nth < self.length() {
            let cid = self.class_id();
            self.with_bytes_mut(|buf| match cid {
                JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => unsafe {
                    *buf.as_mut_ptr().add(nth) = val as u8;
                },
                JS_CLASS_INT8_ARRAY => unsafe {
                    *buf.as_mut_ptr().cast::<i8>().add(nth) = val as i8;
                },
                JS_CLASS_INT16_ARRAY => unsafe {
                    *buf.as_mut_ptr().cast::<i16>().add(nth) = val as i16;
                },
                JS_CLASS_UINT16_ARRAY => unsafe {
                    *buf.as_mut_ptr().cast::<u16>().add(nth) = val as u16;
                },
                JS_CLASS_INT32_ARRAY => unsafe {
                    *buf.as_mut_ptr().cast::<i32>().add(nth) = val;
                },
                JS_CLASS_UINT32_ARRAY => unsafe {
                    *buf.as_mut_ptr().cast::<u32>().add(nth) = val as u32;
                },
                _ => unreachable!(),
            });

            Ok(())
        } else {
            Err(-1)
        }
    }

    /// if nth is out of range, returns Err(-1)
    pub(crate) fn set_element_float(&mut self, nth: usize, val: f64) -> Result<(), i32> {
        if nth < self.length() {
            let cid = self.class_id();
            self.with_bytes_mut(|buf| match cid {
                JS_CLASS_FLOAT32_ARRAY => unsafe {
                    *buf.as_mut_ptr().cast::<f32>().add(nth) = val as f32;
                },
                JS_CLASS_FLOAT64_ARRAY => unsafe {
                    *buf.as_mut_ptr().cast::<f64>().add(nth) = val;
                },
                _ => unreachable!(),
            });
            Ok(())
        } else {
            Err(-1)
        }
    }

    /// run closure with elements slice.
    /// will panic if class_id does not match eleement type T is matched.
    pub fn with_elements_as<T, R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        let len = self.length();
        self.with_bytes(|b| f(unsafe { std::slice::from_raw_parts(b.as_ptr().cast::<T>(), len) }))
    }

    /// create a new TypedArray with content copied from `self` typed array.
    pub fn create_to_new(
        &self,
        ctx: &mut ScopedContext,
        new_target: JSValue,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let count = self.length();
        let size_log2 = Self::SIZE_LOG2(class_id as _);
        let new_buf = ArrayBuffer::create(ctx, false, count << size_log2)?;

        // Note: necessary to check detacth again, because it could have been detached by side effect.
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        // create an empty typed array with count capacity.
        let mut new_ta = Self::create_from(ctx, new_target, new_buf.into(), 0, count, class_id)?;

        // copy data from src_ta to new_ta
        if Self::SIZE_LOG2(self.class_id()) == size_log2 {
            // fast path for same element size: copy bytes
            let src_ta = self.payload();
            src_ta.with_bytes(|src| {
                new_ta.with_bytes_mut(|dst| unsafe {
                    std::ptr::copy_nonoverlapping(
                        src.as_ptr(),
                        dst.as_mut_ptr(),
                        count << size_log2,
                    );
                });
            });
        } else {
            // slow path: get then set as props
            for i in 0..count {
                let val = self.get_property_u32(ctx, i as u32)?;
                new_ta.set_property_u32(ctx, i as u32, val)?;
            }
        }

        Ok(new_ta)
    }

    pub fn as_i32_array(&self) -> Option<&[i32]> {
        if self.class_id() == JS_CLASS_INT32_ARRAY {
            let ptr = self.payload().as_ptr();
            if !ptr.is_null() {
                Some(unsafe { std::slice::from_raw_parts(ptr.cast::<i32>(), self.length()) })
            } else {
                Some(&[])
            }
        } else {
            None
        }
    }

    pub fn as_u32_array(&self) -> Option<&[u32]> {
        if self.class_id() == JS_CLASS_UINT32_ARRAY {
            let ptr = self.payload().as_ptr();
            if !ptr.is_null() {
                Some(unsafe { std::slice::from_raw_parts(ptr.cast::<u32>(), self.length()) })
            } else {
                Some(&[])
            }
        } else {
            None
        }
    }
}
