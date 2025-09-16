use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_DATAVIEW,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    array_buffer::ArrayBufObjPayload,
    atom::consts::JS_ATOM_Symbol_iterator,
    class::JSClassID,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    iterator::js_array_from_iterator,
    object::{JSObject, JSObjectWeakref, ObjectPayload, js_get_length64},
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

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

#[derive(Debug)]
pub struct TypedArrayObjPayload {
    /// back reference to the TypedArray/DataView object.
    pub obj: JSObjectWeakref,

    /// underlaying array buffer object
    pub buffer: JSObject,

    /// start offset in array buffer  
    pub offset: u32,
    /// number of bytes from array buffer
    pub byte_length: u32,
    /// number of elements in typed array
    pub count: u32,
}

impl TypedArrayObjPayload {
    /// create typed array payload with underlying array buffer `buffer`,
    /// offset in array buffer and type array element count.
    /// Note: `buffer` is consumed.
    pub fn new(
        ctx: &JSContext,
        buffer: JSValue,
        class_id: JSClassID,
        offset: usize,
        mut count: usize,
    ) -> Result<Self, i32> {
        debug_assert!(matches!(
            class_id,
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY
        ));

        let size_log2 = typed_array_size_log2(class_id);

        if let JSValue::Object(abo) = buffer {
            let abuf = abo.0.payload.borrow().array_buffer().cloned().unwrap();

            if abuf.0.borrow().detached {
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
                buffer: abo,
                offset: offset as u32,
                byte_length: (count as u32) << size_log2,
                count: count as u32,
            };
            Ok(ta)
        } else {
            unreachable!()
        }
    }

    /// check if `self` TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        // XXX: could simplify test by ensuring typed_array.is_null
        // (*self.0.u.array).data.ptr.is_null()
        self.buffer
            .0
            .payload
            .borrow()
            .array_buffer()
            .unwrap()
            .0
            .borrow()
            .detached
    }

    /// get underlaying array buffer.
    pub fn array_buffer(&self) -> ArrayBufObjPayload {
        self.buffer
            .0
            .payload
            .borrow()
            .array_buffer()
            .cloned()
            .unwrap()
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .with_bytes(|buf| f(&buf[base..base + len]))
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .with_bytes_mut(|buf| f(&mut buf[base..base + len]))
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct TypedArray(pub(crate) JSObject);

impl TryFrom<JSObject> for TypedArray {
    type Error = i32;

    fn try_from(obj: JSObject) -> Result<Self, Self::Error> {
        if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&obj.class_id()) {
            Ok(Self(obj))
        } else {
            Err(-1)
        }
    }
}

impl TryFrom<JSValue> for TypedArray {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if let JSValue::Object(ob) = value {
            ob.try_into()
        } else {
            Err(-1)
        }
    }
}

impl From<TypedArray> for JSObject {
    #[inline(always)]
    fn from(value: TypedArray) -> Self {
        value.0
    }
}

impl From<TypedArray> for JSValue {
    #[inline(always)]
    fn from(value: TypedArray) -> Self {
        Self::Object(value.0)
    }
}

impl TypedArray {
    #[inline(always)]
    pub fn class_id(&self) -> JSClassID {
        self.0.class_id()
    }

    /// run closure with typed array data.
    pub fn with<R>(&self, f: impl FnOnce(&TypedArrayObjPayload) -> R) -> R {
        self.0
            .inner()
            .payload
            .borrow()
            .typed_array()
            .map(f)
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
        let size_log2 = typed_array_size_log2(self.class_id());
        self.with(|ta| (ta.byte_length >> size_log2) as usize)
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
        self.with(|ta| ta.with_bytes(f))
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub fn with_bytes_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.with(|ta| ta.with_bytes_mut(f))
    }

    /// create a new TypedArray value with content copied from `self` typed array.
    pub fn create_to_new(
        &self,
        ctx: &JSContext,
        new_target: &JSValueConst,
        class_id: JSClassID,
    ) -> JSResult {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let count = self.length();
        let size_log2 = typed_array_size_log2(class_id as _);

        let new_buf = ctx.create_array_buffer(count << size_log2)?;

        // Note: necessary to check detacth again, because it could have been detached
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        // create an empty typed array with count capacity.
        let new_ta = ctx
            .create_typed_array_from(new_target, &new_buf, 0, count, class_id)
            .and_then(TryInto::<TypedArray>::try_into)?;

        // copy data from src_ta to new_ta
        if typed_array_size_log2(self.class_id()) == size_log2 {
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

        Ok(new_ta.into())
    }
}

macro_rules! typed_array_obj_of {
    ($name:ident, $ty:ident, $class_id:pat) => {
        #[repr(transparent)]
        #[derive(Clone)]
        pub struct $name(pub(crate) TypedArray);

        impl $name {
            #[inline(always)]
            pub fn length(&self) -> usize {
                self.0.length()
            }

            /// run closure with elements slice.
            pub fn with_elements<R>(&self, f: impl FnOnce(&[$ty]) -> R) -> R {
                let len = self.length();
                self.0.with_bytes(|b| {
                    f(unsafe { std::slice::from_raw_parts(b.as_ptr().cast::<$ty>(), len) })
                })
            }

            /// run closure with elements slice.
            pub fn with_elements_mut<R>(&self, f: impl FnOnce(&mut [$ty]) -> R) -> R {
                let len = self.length();
                self.0.with_bytes_mut(|b| {
                    f(
                        unsafe {
                            std::slice::from_raw_parts_mut(b.as_mut_ptr().cast::<$ty>(), len)
                        },
                    )
                })
            }

            /// get `nth` element value.
            /// if `nth` is out of bound, returns Err
            pub fn element(&self, nth: usize) -> Result<$ty, i32> {
                if nth < self.length() {
                    Ok(self.with_elements(|p| p[nth]))
                } else {
                    Err(-1)
                }
            }

            /// set `nth` element value.
            /// if `nth` is out of bound, returns Err
            pub fn set_element(&mut self, nth: usize, val: $ty) -> Result<(), i32> {
                if nth < self.length() {
                    self.with_elements_mut(|p| {
                        p[nth] = val;
                        Ok(())
                    })
                } else {
                    Err(-1)
                }
            }
        }

        impl TryFrom<JSObject> for $name {
            type Error = i32;

            fn try_from(obj: JSObject) -> Result<Self, Self::Error> {
                if matches!(obj.class_id(), $class_id) {
                    Ok(Self(TypedArray(obj)))
                } else {
                    Err(-1)
                }
            }
        }

        impl TryFrom<TypedArray> for $name {
            type Error = i32;

            fn try_from(value: TypedArray) -> Result<Self, Self::Error> {
                if matches!(value.0.class_id(), $class_id) {
                    Ok(Self(value))
                } else {
                    Err(-1)
                }
            }
        }

        impl TryFrom<JSValue> for $name {
            type Error = i32;

            fn try_from(value: JSValue) -> Result<Self, Self::Error> {
                if let JSValue::Object(ob) = value {
                    ob.try_into()
                } else {
                    Err(-1)
                }
            }
        }
    };
}

typed_array_obj_of!(Int8Array, i8, JS_CLASS_INT8_ARRAY);
typed_array_obj_of!(Uint8Array, u8, JS_CLASS_UINT8_ARRAY | JS_CLASS_UINT8C_ARRAY);
typed_array_obj_of!(Int16Array, i16, JS_CLASS_INT16_ARRAY);
typed_array_obj_of!(Uint16Array, u16, JS_CLASS_UINT16_ARRAY);
typed_array_obj_of!(Int32Array, i32, JS_CLASS_INT32_ARRAY);
typed_array_obj_of!(Uint32Array, u32, JS_CLASS_UINT32_ARRAY);
typed_array_obj_of!(Float32Array, f32, JS_CLASS_FLOAT32_ARRAY);
typed_array_obj_of!(Float64Array, f64, JS_CLASS_FLOAT64_ARRAY);

impl JSValue {
    /// If value is a TypedArray or DataView, returns TypedArray.
    /// Otherwise, raise Error, and returns Err.
    pub fn typed_array_must(self, ctx: &JSContext, is_dataview: bool) -> Result<TypedArray, i32> {
        if let Self::Object(ob) = self {
            if is_dataview {
                if ob.class_id() != JS_CLASS_DATAVIEW {
                    ctx.throw_type_error("not a DataView");
                    return Err(-1);
                }
            } else if !(JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&ob.class_id()) {
                ctx.throw_type_error("not a TypedArray");
                return Err(-1);
            }

            Ok(TypedArray(ob))
        } else {
            Err(-1)
        }
    }
}

impl JSContext {
    /// create an TypedArray with array buffer, offset and count.
    /// `count`: number of elements.
    /// `buffer` is consumed.
    fn do_create_typed_array(
        &self,
        new_target: &JSValue,
        buffer: JSValue,
        offset: usize,
        count: usize,
        class_id: JSClassID,
    ) -> JSResult {
        let arr = Box::new(TypedArrayObjPayload::new(
            self, buffer, class_id, offset, count,
        )?);
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
            c @ (JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY) => {
                self.throw_type_error(&format!(
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
        let ta = self
            .create_object_apply_ctor(new_target, isa)
            .map_err(|_| -1)?;

        // establish mutual references
        {
            let w = ta.downgrade();
            ta.0.payload
                .borrow_mut()
                .typed_array()
                .map(|x| x.array_buffer())
                .unwrap()
                .0
                .borrow_mut()
                .array_list
                .push(w.clone());

            ta.0.payload.borrow_mut().typed_array_mut().unwrap().obj = w;
        }

        Ok(ta.into())
    }

    /// create an TypedArray with empty array buffer.
    /// `count`: number of elements
    pub fn create_typed_array(
        &self,
        new_target: &JSValue,
        count: usize,
        class_id: JSClassID,
    ) -> JSResult {
        // create empty array buffer
        let buffer = {
            let size_log2 = typed_array_size_log2(class_id);
            self.create_array_buffer(count << size_log2)?
        };
        self.do_create_typed_array(new_target, buffer, 0, count, class_id)
    }

    /// create a TypedArray from an existing `source`. The source value could be:
    /// - undefined: create an empty TypedArray;
    /// - TypedArray: create a TypedArray with content copied from source;
    /// - ArrayBuffer, SharedArrayBuffer: create a TypedArray with array buffer;
    /// - An enumerable object: create a TypedArray with content from enumerated index property vals.
    /// Note: `source` is not consumed.
    pub fn create_typed_array_from(
        &self,
        new_target: &JSValue,
        source: &JSValue,
        offset: usize,
        length: usize,
        class_id: JSClassID,
    ) -> JSResult {
        let size_log2 = typed_array_size_log2(class_id);

        match source {
            JSValue::UNDEFINED => self.create_typed_array(new_target, length, class_id),
            JSValue::Object(p) => {
                match p.class_id() {
                    JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER => self
                        .do_create_typed_array(
                            new_target,
                            source.clone(),
                            offset,
                            length,
                            class_id,
                        ),
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        TypedArray(p.clone()).create_to_new(self, new_target, class_id)
                    }
                    _ => {
                        // source is an enumetable object.
                        // get property from source object and set to new array.
                        let iter_method = source.get_property(self, JS_ATOM_Symbol_iterator)?;
                        let (enum_obj, count) = if !iter_method.is_undefined_or_null() {
                            js_array_from_iterator(self, &source, &iter_method)?
                        } else {
                            (source.clone(), js_get_length64(self, source)? as usize)
                        };

                        let buffer = self.create_array_buffer(count << size_log2)?;
                        let new_ta =
                            self.do_create_typed_array(new_target, buffer, 0, count, class_id)?;

                        // put elements to buffer
                        for i in 0..count {
                            new_ta.set_property_u32(
                                self,
                                i as u32,
                                enum_obj.get_property_u32(self, i as u32)?,
                            )?;
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
pub unsafe fn js_typed_array_get_length_internal(
    ctx: &JSContext,
    obj: &JSValueConst,
) -> Result<u32, i32> {
    let p = obj.clone().typed_array_must(ctx, false)?;
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
    let p = dst.clone().typed_array_must(ctx, false)?;
    if p.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let offset: i64 = off.to_i64_saturating(ctx)?;
    if offset < 0 {
        ctx.throw_range_error("invalid array length");
        return Err(-1);
    }

    let src_p = src.clone().ensure_object(ctx)?;

    let count: i64;

    match src_p.class_id() {
        JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
            let src = TypedArray(src_p.clone());

            let (src_abuf, shift, cnt) = src.with(|ta| {
                (
                    ta.array_buffer(),
                    typed_array_size_log2(p.class_id()),
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

            let dest_abuf = p.with(|ta| ta.array_buffer());

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
            count = js_get_length64(ctx, &src_p.clone().into())?;
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
        // order so the ArrayBuffer finalizer may have been called
        ta.array_buffer()
            .0
            .borrow_mut()
            .array_list
            .retain(|a| match a.0.strong_count() {
                1 => {
                    if let Some(o) = a.upgrade() {
                        o.id() != id
                    } else {
                        false
                    }
                }
                0 => false,
                _ => true,
            });
    }
}

pub fn js_typed_array_mark(val: JSValue) {
    let p = unsafe { val.as_object_unchecked() };
    if let Some(ta) = p.0.payload.borrow().typed_array() {
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
