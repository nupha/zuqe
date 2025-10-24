use std::{cell::RefCell, ops::Deref};

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_DATAVIEW,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    array_buffer::ArrayBuffer,
    atom::consts::JS_ATOM_Symbol_iterator,
    class::ClassID,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    iterator::js_array_from_iterator,
    object::{JSObject, JSObjectWeakref, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    value::JSValue,
};

pub const JS_TYPED_ARRAY_COUNT: usize =
    JS_CLASS_FLOAT64_ARRAY as usize - JS_CLASS_UINT8C_ARRAY as usize + 1;

#[derive(Debug)]
pub struct TypedArrayPayload {
    /// back reference to the TypedArray/DataView object.
    pub obj: JSObjectWeakref,

    /// underlaying array buffer object.
    pub buffer: ArrayBuffer,

    /// start offset in array buffer  
    pub offset: u32,
    /// number of bytes from array buffer
    pub byte_length: u32,
    /// number of elements in typed array
    pub count: u32,
}

impl TypedArrayPayload {
    /// create typed array payload with underlying array buffer `buffer`,
    /// offset in array buffer and type array element count.
    /// Note: `buffer` is consumed.
    pub fn new(
        ctx: &JSContext,
        buffer: ArrayBuffer,
        class_id: ClassID,
        offset: usize,
        mut count: usize,
    ) -> Result<Self, i32> {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY
        ));

        let size_log2 = TypedArray::SIZE_LOG2(class_id);
        let abuf = buffer.payload();

        if abuf.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let byte_length = abuf.byte_length();
        if (offset & ((1 << size_log2) - 1) != 0) || offset > byte_length {
            ctx.throw_range_error("invalid offset");
            return Err(-1);
        }

        if count == 0 {
            if byte_length & ((1 << size_log2) - 1) != 0 {
                ctx.throw_range_error("invalid length");
                return Err(-1);
            }
            count = (byte_length - offset) >> size_log2;
        } else if (offset + (count << size_log2)) > byte_length {
            ctx.throw_range_error("invalid length");
            return Err(-1);
        }

        let ta = Self {
            obj: JSObjectWeakref::default(), // empty weakref
            buffer,
            offset: offset as u32,
            byte_length: (count as u32) << size_log2,
            count: count as u32,
        };
        Ok(ta)
    }

    /// check if `self` TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        self.buffer.payload().is_detached()
    }

    #[inline(always)]
    pub const fn byte_length(&self) -> usize {
        self.byte_length as _
    }

    /// get underlaying array buffer object.
    #[inline(always)]
    pub const fn array_buffer(&self) -> &ArrayBuffer {
        &self.buffer
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload()
            .with_bytes(|buf| f(&buf[base..base + len]))
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload()
            .with_bytes_mut(|buf| f(&mut buf[base..base + len]))
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct TypedArray(pub(crate) JSObject);

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
        | JS_CLASS_DATAVIEW
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

    /// run closure with typed array data.
    pub fn with<R>(&self, f: impl FnOnce(&TypedArrayPayload) -> R) -> R {
        self.0
            .payload()
            .typed_array()
            .map(|x| f(x.borrow().deref()))
            .unwrap()
    }

    /// check if the TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        self.with(|ta| ta.is_detached())
    }

    /// Get elements count of typed array.
    /// If underlaying array buffer is detached, returns 0.
    #[inline]
    pub fn length(&self) -> usize {
        self.with(|ta| ta.count as usize)
    }

    /// Get elements count of typed array. (calculated by underlying array buffer space).
    /// Usually this equals to that of .length(), only in case when array buffer is detached,
    /// this will still return rated count, but .length() will return 0.
    pub fn capacity(&self) -> usize {
        let size_log2 = Self::SIZE_LOG2(self.class_id());
        self.with(|ta| (ta.byte_length >> size_log2) as usize)
    }

    /// get underlying array buffer object.
    pub fn array_buffer(&self) -> ArrayBuffer {
        self.with(|ta| ta.array_buffer().clone())
    }

    /// Get underlying array buffer byte offset.
    /// When underlaying array buffer is detached:
    /// - is_dataview=true, returns Err.
    /// - is_dataview=false, returns 0.
    pub fn array_buffer_byte_offset(
        &self,
        ctx: &JSContext,
        is_dataview: bool,
    ) -> Result<usize, i32> {
        if self.is_detached() {
            if is_dataview {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                Err(-1)
            } else {
                Ok(0)
            }
        } else {
            Ok(self.with(|ta| ta.offset as usize))
        }
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        self.with(|p| p.with_bytes(f))
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub fn with_bytes_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.with(|ta| ta.with_bytes_mut(f))
    }

    /// if nth is out of range, return Err(-1)
    pub(crate) fn set_element_int(&self, nth: usize, val: i32) -> Result<(), i32> {
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
                    *buf.as_mut_ptr().cast::<i32>().add(nth) = val as i32;
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
    pub(crate) fn set_element_float(&self, nth: usize, val: f64) -> Result<(), i32> {
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
        ctx: &JSContext,
        new_target: &JSValue,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let count = self.length();
        let size_log2 = Self::SIZE_LOG2(class_id as _);

        let new_buf = ArrayBuffer::create(ctx, false, count << size_log2)?;

        // Note: necessary to check detacth again, because it could have been detached
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        // create an empty typed array with count capacity.
        let new_ta = Self::create_from(ctx, new_target, &new_buf.into(), 0, count, class_id)?;

        // copy data from src_ta to new_ta
        if Self::SIZE_LOG2(self.class_id()) == size_log2 {
            // fast path for same element size: copy bytes
            self.with(|src_ta| {
                src_ta.with_bytes(|src| {
                    new_ta.with_bytes_mut(|dst| unsafe {
                        std::ptr::copy_nonoverlapping(
                            src.as_ptr(),
                            dst.as_mut_ptr(),
                            count << size_log2,
                        );
                    });
                });
            });
        } else {
            // slow path: get then set as props
            for i in 0..count {
                let val = self.0.get_property_u32(ctx, i as u32)?;
                new_ta.0.set_property_u32(ctx, i as u32, val)?;
            }
        }

        Ok(new_ta)
    }

    /// create an TypedArray with array buffer, offset and count.
    /// `count`: number of elements.
    fn create(
        ctx: &JSContext,
        new_target: &JSValue,
        buffer: ArrayBuffer,
        offset: usize,
        count: usize,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        let arr = RefCell::new(Box::new(TypedArrayPayload::new(
            ctx, buffer, class_id, offset, count,
        )?));

        let isa = match class_id {
            JS_CLASS_UINT8C_ARRAY => ObjectPayload::UINT8C_ARRAY(arr),
            JS_CLASS_UINT8_ARRAY => ObjectPayload::UINT8_ARRAY(arr),
            JS_CLASS_INT8_ARRAY => ObjectPayload::INT8_ARRAY(arr),
            JS_CLASS_UINT16_ARRAY => ObjectPayload::UINT16_ARRAY(arr),
            JS_CLASS_INT16_ARRAY => ObjectPayload::INT16_ARRAY(arr),
            JS_CLASS_UINT32_ARRAY => ObjectPayload::UINT32_ARRAY(arr),
            JS_CLASS_INT32_ARRAY => ObjectPayload::UINT32_ARRAY(arr),
            JS_CLASS_FLOAT32_ARRAY => ObjectPayload::FLOAT32_ARRAY(arr),
            JS_CLASS_FLOAT64_ARRAY => ObjectPayload::FLOAT64_ARRAY(arr),
            JS_CLASS_DATAVIEW => ObjectPayload::DATAVIEW(arr),
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

            p.borrow()
                .array_buffer()
                .payload()
                .add_typed_array_weakref(w.clone());

            p.borrow_mut().obj = w;
        }

        Ok(TypedArray(ta))
    }

    /// create an TypedArray with empty array buffer.
    /// `count`: number of elements
    pub fn with_capacity(
        ctx: &JSContext,
        new_target: &JSValue,
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
    /// - undefined: create an empty TypedArray;
    /// - TypedArray: create a TypedArray with content copied from source;
    /// - ArrayBuffer, SharedArrayBuffer: create a TypedArray with array buffer;
    /// - An enumerable object: create a TypedArray with content from enumerated index property vals.
    /// Note: `source` is not consumed.
    pub fn create_from(
        ctx: &JSContext,
        new_target: &JSValue,
        source: &JSValue,
        offset: usize,
        length: usize,
        class_id: ClassID,
    ) -> Result<Self, i32> {
        let size_log2 = TypedArray::SIZE_LOG2(class_id);

        match source {
            JSValue::UNDEFINED => Self::with_capacity(ctx, new_target, length, class_id),
            JSValue::Object(p) => {
                match p.class_id() {
                    JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER => Self::create(
                        ctx,
                        new_target,
                        ArrayBuffer::try_from(source.clone()).unwrap(),
                        offset,
                        length,
                        class_id,
                    ),
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        TypedArray(p.clone()).create_to_new(ctx, new_target, class_id)
                    }
                    _ => {
                        // source is an enumetable object.
                        // get property from source object and set to new array.
                        let iter_method = source.get_property(ctx, &JS_ATOM_Symbol_iterator)?;
                        let (enum_obj, count) = if !iter_method.is_undefined_or_null() {
                            js_array_from_iterator(ctx, &source, &iter_method)?
                        } else {
                            (source.clone(), source.get_length_prop(ctx)? as usize)
                        };

                        let new_ta = Self::create(
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

/// Get the length of the TypedArray, equivalent to TypedArrayObj::length() but
/// will return an error if the TypedArray is detached.
#[deprecated(note = "use TypedArray::length() instead")]
pub fn js_typed_array_get_length_internal(ctx: &JSContext, obj: &JSValue) -> Result<u32, i32> {
    let p = TypedArray::try_from(obj.clone()).map_err(|_| {
        ctx.throw_type_error("not a TypedArray");
        -1
    })?;
    if p.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        Err(-1)
    } else {
        Ok(p.length() as u32)
    }
}

pub fn js_typed_array_set_internal(
    ctx: &JSContext,
    dst: &JSValue,
    src: &JSValue,
    off: &JSValue,
) -> JSResult {
    let p = TypedArray::try_from(dst.clone()).map_err(|_| {
        ctx.throw_type_error("not a TypedArray");
        -1
    })?;
    if p.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let offset: i64 = off.to_i64_saturating(ctx)?;
    if offset < 0 {
        ctx.throw_range_error("invalid array length");
        return Err(-1);
    }

    let src_p = src.clone().objectify(ctx)?;

    let count: i64;

    match src_p.class_id() {
        JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
            let src = TypedArray(src_p.clone());

            let (src_abuf, shift, cnt) = src.with(|ta| {
                (
                    ta.array_buffer().payload(),
                    TypedArray::SIZE_LOG2(p.class_id()),
                    ta.count,
                )
            });
            count = cnt as i64;

            if src_abuf.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                return Err(-1);
            }
            if offset > p.length() as i64 - count {
                ctx.throw_range_error("invalid array length");
                return Err(-1);
            }

            // copying between typed objects
            if src.class_id() == p.class_id() {
                // same type, use mem copy
                src.with_bytes(|src| {
                    p.with_bytes_mut(|dst| unsafe {
                        std::ptr::copy(
                            src.as_ptr(),
                            dst.as_mut_ptr(),
                            (count << shift) as usize, // count to byte_num
                        );
                    })
                });

                return Ok(JSValue::UNDEFINED);
            }

            let dest_abuf = p.with(|ta| ta.array_buffer().payload());

            if dest_abuf.content().id() == src_abuf.content().id() {
                // copying between the same buffer using different types of mappings
                // would require a temporary buffer
                todo!();
            } else {
                // copy
                todo!()
            }

            // Ok(JSValue::UNDEFINED)
        }

        _ => {
            count = src_p.get_length_prop(ctx)?;
            if offset > p.length() as i64 - count {
                ctx.throw_range_error("invalid array length");
                return Err(-1);
            }

            for i in 0..count as u32 {
                let val = src_p.get_property_u32(ctx, i)?;
                dst.set_property_u32(ctx, offset as u32 + i, val)?;
            }
        }
    }

    Ok(JSValue::UNDEFINED)
}

pub fn js_typed_array_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {
    if let Some(ta) = isa.typed_array() {
        // during the GC the finalizers are called in an arbitrary
        // order so the ArrayBuffer finalize may have been called
        ta.borrow()
            .array_buffer()
            .payload()
            .remove_typed_array_weakref(id);
    }
}

pub fn js_typed_array_mark(val: JSValue) {
    let p = unsafe { val.as_object_unchecked() };
    if let Some(_) = p.payload().typed_array() {
        // mark underlaying array buffer
        // println!("[O.o] TODO typed array mark");
        // (*rt).gc_mark_value(JSValue::Object(ta.array_buffer().clone()), mark_func);
    }
}

/* WARNING: 'p' must be a typed array */
#[deprecated(note = "use TypedArray::is_detached() instead")]
pub unsafe fn typed_array_is_detached(p: *mut JSValue) -> bool {
    unreachable!("use TypedArray::is_detached() instead")
}
