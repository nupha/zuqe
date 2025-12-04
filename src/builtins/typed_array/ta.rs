use std::cell::UnsafeCell;

use gc_lite::{Collect, GcRef, GcWeak};

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::Atom,
    builtins::{array_buffer::ArrayBuffer, iterator::js_array_from_iterator},
    class::ClassID,
    context::{Context, ContextImpl},
    error::JS_ThrowTypeErrorDetachedArrayBuffer,
    function::Callback,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, ObjectImpl, ObjectPayload, ObjectRef},
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
    pub(super) buffer: ArrayBuffer,

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

unsafe impl Collect for TypedArrayPayload {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.buffer.trace(tracer);
    }
}

impl TypedArrayPayload {
    /// create typed array payload with underlying array buffer `buffer`,
    /// offset in array buffer and type array element count.
    fn new(
        buffer: ArrayBuffer,
        class_id: ClassID,
        offset: usize,
        count: usize,
    ) -> Result<Self, i32> {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY
        ));

        let abuf = buffer.payload();
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
            buffer,
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
        self.buffer
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

    pub fn class_initialize(mut ctx: Context, class_id: ClassID) -> Result<Option<ObjectRef>, i32> {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY
        ));

        let mut proto = ctx.create_base_object().unwrap();
        proto.install_field_defs(&ctx, &js_typed_array_base_proto_funcs)?;

        // TypedArray.prototype.toString must be the same object as Array.prototype.toString
        let to_string = ctx
            .class_proto(JS_CLASS_ARRAY)
            .unwrap()
            .get_property(&ctx, Atom::CONST_toString)?;

        // XXX: should use alias method in JSCFunctionListEntry
        proto.define_property_value(
            &ctx,
            Atom::CONST_toString,
            to_string,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;

        let mut typed_array_base_func = ctx.create_native_function(
            Callback::Func(js_typed_array_base_constructor),
            "TypedArray",
            0,
            0,
        )?;
        typed_array_base_func
            .install_field_defs(&ctx, &js_typed_array_base_funcs)
            .unwrap();

        ctx.bind_ctor_proto(typed_array_base_func.into(), proto.into())
            .unwrap();

        // make proto for variant typed array classes
        for cid in JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY {
            let nby = 1 << TypedArray::SIZE_LOG2(cid);
            let name = Atom::from_id(
                Atom::CONST_Uint8ClampedArray.id() + (cid - JS_CLASS_UINT8C_ARRAY) as u32,
            )
            .to_utf8();

            let mut ta_obj = ctx.create_base_object_with_proto(Some(proto))?;
            ta_obj.define_property_value_str(&ctx, "BYTES_PER_ELEMENT", JSValue::Int(nby), 0)?;

            let mut func_obj: JSValue = ctx
                .create_native_function_with_proto(
                    Callback::Constructor(js_typed_array_constructor),
                    &name,
                    3,
                    cid as i16, // magic
                    Some(typed_array_base_func),
                )?
                .into();

            ctx.add_global_constructor(name.as_ref(), func_obj, ta_obj.into());
            func_obj.define_property_value_str(&ctx, "BYTES_PER_ELEMENT", JSValue::Int(nby), 0)?;

            ctx.class_proto[cid as usize] = ta_obj.into();
        }

        Ok(None)
    }

    pub fn class_finalize(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize) {
        if let Some(ta) = isa.typed_array() {
            // during the GC the finalizers are called in an arbitrary
            // order so the ArrayBuffer finalize may have been called
            ta.array_buffer()
                .payload_mut()
                .remove_typed_array_weakref(rt, id);
        }
    }

    #[inline(always)]
    pub fn payload(&self) -> &TypedArrayPayload {
        unsafe { self.0.as_ref().payload().typed_array().unwrap() }
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut TypedArrayPayload {
        unsafe { self.0.as_mut().payload_mut().typed_array_mut().unwrap() }
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
    pub(crate) fn with_elements_as<T, R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        let len = self.length();
        self.with_bytes(|b| f(unsafe { std::slice::from_raw_parts(b.as_ptr().cast::<T>(), len) }))
    }

    /// create a new TypedArray with content copied from `self` typed array.
    pub fn create_to_new(
        &self,
        ctx: &ContextImpl,
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

    /// create an TypedArray with array buffer, offset and count.
    /// `count`: number of elements.
    fn create(
        ctx: &ContextImpl,
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
            .create_object_from_ctor(new_target, isa)
            .map_err(|_| -1)?;

        // establish mutual references
        {
            let p = ta.payload().typed_array().unwrap();
            let w = ta.downgrade();

            p.array_buffer().payload_mut().add_typed_array_weakref(w);
            unsafe {
                *p.obj.get() = w;
            }
        }

        Ok(TypedArray(ta.0))
    }

    /// create an TypedArray with empty array buffer.
    /// `count`: number of elements
    pub fn with_capacity(
        ctx: &ContextImpl,
        new_target: JSValue,
        count: usize,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        TypedArray::create(
            ctx,
            new_target,
            ArrayBuffer::create(ctx, false, count << TypedArray::SIZE_LOG2(class_id))?,
            0,
            count,
            class_id,
        )
    }

    /// create a TypedArray from an existing `source`. The source value could be:
    /// - TypedArray: create a TypedArray with content copied from source;
    /// - ArrayBuffer, SharedArrayBuffer: create a TypedArray with array buffer;
    /// - An enumerable object: create a TypedArray with content from enumerated index property vals.
    pub(crate) fn create_from(
        ctx: &ContextImpl,
        new_target: JSValue,
        source: JSValue,
        offset: usize,
        length: usize,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        let size_log2 = TypedArray::SIZE_LOG2(class_id);

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
                        // source is an enumetable object.
                        // get property from source object and set to new array.
                        let iter_method = o.get_property(ctx, Atom::CONST_Symbol_iterator)?;
                        let (enum_obj, count) = if !iter_method.is_undefined_or_null() {
                            js_array_from_iterator(ctx, o.into(), iter_method)?
                        } else {
                            let len = o.get_length_prop(ctx)? as usize;
                            (o.into(), len)
                        };

                        let mut new_ta = Self::create(
                            ctx,
                            new_target,
                            ArrayBuffer::create(ctx, false, count << size_log2)?,
                            0,
                            count,
                            class_id,
                        )?;

                        for i in 0..count {
                            let v = enum_obj.get_property_u32(ctx, i as u32)?;
                            new_ta.set_property_u32(ctx, i as u32, v)?;
                        }

                        Ok(new_ta)
                    }
                }
            }
            _ => Err(-1),
        }
    }
}

#[deprecated]
pub fn js_typed_array_mark(val: JSValue) {
    let p = unsafe { val.as_object_unchecked() };
    if p.payload().typed_array().is_some() {
        // mark underlaying array buffer
        // println!("[O.o] TODO typed array mark");
        // (*rt).gc_mark_value(JSValue::Object(ta.array_buffer().clone()), mark_func);
    }
}
