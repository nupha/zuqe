use std::ptr::NonNull;

use crate::{
    array_buffer::{js_array_buffer_constructor1, JSArrayBuffer},
    atom::consts::JS_ATOM_Symbol_iterator,
    class::JSClassID,
    context::JSContext,
    error::{JSResult, JS_ThrowRangeError, JS_ThrowTypeErrorDetachedArrayBuffer},
    gc::{JS_MarkFunc, JS_MarkValue},
    iterator::js_array_from_iterator,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{list_add_tail, list_del, list_head},
    utils::{UncheckedFrom, UncheckedInto},
    value::{
        object::{js_get_length64, JSObject, JSObjectIsa, JS_ToObject},
        JSTag, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_ToInt64Sat, JS_TAG_OBJECT,
        JS_TAG_UNDEFINED, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY,
    JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT8_ARRAY,
    JS_MKPTR,
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

pub struct JSTypedArray {
    pub link: list_head, /* link to arraybuffer */

    /// back pointer to the TypedArray/DataView object
    pub obj: *mut JSObject,

    /// pointer of underlaying array buffer object
    pub buffer: NonNull<JSObject>,
    /// start offset in array buffer  
    pub offset: u32,
    /// number of bytes from array buffer
    pub byte_length: u32,
    /// number of elements in typed array
    pub count: u32,
}

impl JSTypedArray {
    /// check if `self` TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        unsafe {
            // XXX: could simplify test by ensuring typed_array.is_null
            // (*self.0.u.array).data.ptr.is_null()
            self.buffer.as_ref().isa().array_buffer().unwrap().detached
        }
    }

    /// get underlaying array buffer.
    #[inline(always)]
    pub fn array_buffer(&self) -> &JSArrayBuffer {
        unsafe { self.buffer.as_ref().isa().array_buffer().unwrap() }
    }

    /// get underlaying array buffer.
    #[inline(always)]
    pub fn array_buffer_mut(&mut self) -> &mut JSArrayBuffer {
        unsafe { self.buffer.as_mut().isa_mut().array_buffer_mut().unwrap() }
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let byte_length = self.byte_length as usize;

        unsafe {
            self.array_buffer()
                .with_bytes_ptr(|ptr| f(std::slice::from_raw_parts(ptr.add(base), byte_length)))
        }
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let byte_length = self.byte_length as usize;

        unsafe {
            self.array_buffer_mut().with_bytes_mut_ptr(|ptr| {
                f(std::slice::from_raw_parts_mut(ptr.add(base), byte_length))
            })
        }
    }

    /// run closure with underlaying bytes pointer of typed array.
    pub unsafe fn with_bytes_ptr<R>(&self, f: impl FnOnce(*const u8) -> R) -> R {
        let base = self.offset as usize;
        self.array_buffer().with_bytes_ptr(|ptr| f(ptr.add(base)))
    }

    /// run closure with underlaying bytes pointer of typed array.
    pub unsafe fn with_bytes_mut_ptr<R>(&mut self, f: impl FnOnce(*mut u8) -> R) -> R {
        let base = self.offset as usize;
        self.array_buffer_mut()
            .with_bytes_mut_ptr(|ptr| f(ptr.add(base)))
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct TypedArrayValue(JSValue);

impl TypedArrayValue {
    #[inline(always)]
    pub fn class_id(self) -> JSClassID {
        unsafe { self.0.object_unchecked().class_id() }
    }

    /// get typed array data raw pointer.
    #[inline(always)]
    pub fn typed_array_ptr(&self) -> NonNull<JSTypedArray> {
        unsafe { NonNull::new_unchecked(self.0.object_unchecked().isa().typed_array_ptr()) }
    }

    /// get typed array data.
    #[inline(always)]
    pub fn typed_array<'a>(self) -> &'a mut JSTypedArray {
        unsafe { self.typed_array_ptr().as_mut() }
    }

    /// setup typed array with array buffer `buffer`, offset in array buffer and type array element count.
    /// Dont' setup a TypedArray twice.
    /// Note: `buffer` is consumed.
    unsafe fn init(
        self,
        ctx: &mut JSContext,
        buffer: JSValue,
        offset: usize,
        count: usize,
    ) -> Result<(), i32> {
        debug_assert!(
            self.0.object_unchecked().isa().array_buffer_ptr().is_null(),
            "can't setup a TypedArray twice"
        );

        let size_log2 = typed_array_size_log2(self.class_id());

        let pbuffer = buffer.object_unchecked();
        let abuf = pbuffer.isa().array_buffer_ptr();
        debug_assert!(
            (*abuf).byte_length() >= offset + (count << size_log2),
            "len1={}, len2={}",
            (*abuf).byte_length(),
            offset + (count << size_log2)
        );

        let mut ta = unsafe {
            if let Some(p) = js_malloc_typed::<JSTypedArray>(ctx, 1, false) {
                p.write(JSTypedArray {
                    link: list_head::default(),
                    obj: self.0.object_unchecked(),
                    buffer: NonNull::from(pbuffer),
                    offset: offset as u32,
                    byte_length: (count as u32) << size_log2,
                    count: count as u32,
                });
                Box::from_raw(p.as_ptr())
            } else {
                JS_FreeValue(ctx, buffer);
                return Err(-1);
            }
        };

        list_add_tail(&mut ta.link, &mut (*abuf).array_list);

        self.0
            .object_unchecked()
            .isa_mut()
            .typed_array_replace(Box::into_raw(ta))
            .unwrap();

        Ok(())
    }

    /// check if the TypedArray is detached.
    #[inline(always)]
    pub fn is_detached(self) -> bool {
        self.typed_array().is_detached()
    }

    /// Get elements count of typed array.
    /// If underlaying array buffer is detached, returns 0.
    #[inline(always)]
    pub fn length(self) -> usize {
        unsafe {
            let ta = self.0.object_unchecked().isa().typed_array().unwrap();
            ta.count as usize
            // if ta.is_detached() {
            //     0
            // } else {
            //     ta.count as usize
            // }
        }
    }

    /// Get elements count of typed array. (calculated by underlying array buffer space).
    /// Usually this equals to that of .length(), only in case when array buffer is detached,
    /// this will still return rated count, but .length() will return 0.
    pub fn capacity(self) -> usize {
        let ta = self.typed_array();
        let size_log2 = typed_array_size_log2(self.class_id());
        (ta.byte_length >> size_log2) as usize
    }

    /// Get underlying array buffer byte offset.
    /// When underlaying array buffer is detached:
    /// - is_dataview=true, returns Err.
    /// - is_dataview=false, returns 0.
    pub fn array_buffer_byte_offset(
        self,
        ctx: &mut JSContext,
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
            Ok(self.typed_array().offset as usize)
        }
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub fn with_bytes<R>(self, f: impl FnOnce(&[u8]) -> R) -> R {
        self.typed_array().with_bytes(f)
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.typed_array().array_buffer_mut().with_bytes_mut(f)
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub unsafe fn with_bytes_ptr<R>(&self, f: impl FnOnce(*const u8) -> R) -> R {
        self.typed_array().array_buffer().with_bytes_ptr(f)
    }

    /// run closure with underlaying bytes of typed array
    #[inline(always)]
    pub unsafe fn with_bytes_mut_ptr<R>(&mut self, f: impl FnOnce(*mut u8) -> R) -> R {
        self.typed_array().array_buffer_mut().with_bytes_mut_ptr(f)
    }

    /// create a new TypedArray value with content copied from `self` typed array.
    pub fn create_to_new(
        &mut self,
        ctx: &mut JSContext,
        new_target: JSValueConst,
        class_id: JSClassID,
    ) -> JSResult {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let src_ta = self.typed_array();
        let count = self.length();
        let size_log2 = typed_array_size_log2(class_id as _);

        let dst_abuf = unsafe {
            js_array_buffer_constructor1(ctx, JSValue::UNDEFINED, count << size_log2)
                .check_exception()?
        };

        // Note: necessary to check detacth again, because it could have been detached
        if self.is_detached() {
            JS_FreeValue(ctx, dst_abuf);
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        // create and setup new TypedArray value
        let mut new_ta = unsafe {
            let isa = match class_id {
                JS_CLASS_UINT8C_ARRAY => JSObjectIsa::UINT8C_ARRAY(std::ptr::null_mut()),
                JS_CLASS_UINT8_ARRAY => JSObjectIsa::UINT8_ARRAY(std::ptr::null_mut()),
                JS_CLASS_INT8_ARRAY => JSObjectIsa::INT8_ARRAY(std::ptr::null_mut()),
                JS_CLASS_UINT16_ARRAY => JSObjectIsa::UINT16_ARRAY(std::ptr::null_mut()),
                JS_CLASS_INT16_ARRAY => JSObjectIsa::INT16_ARRAY(std::ptr::null_mut()),
                JS_CLASS_UINT32_ARRAY => JSObjectIsa::UINT32_ARRAY(std::ptr::null_mut()),
                JS_CLASS_INT32_ARRAY => JSObjectIsa::UINT32_ARRAY(std::ptr::null_mut()),
                JS_CLASS_FLOAT32_ARRAY => JSObjectIsa::FLOAT32_ARRAY(std::ptr::null_mut()),
                JS_CLASS_FLOAT64_ARRAY => JSObjectIsa::FLOAT64_ARRAY(std::ptr::null_mut()),
                _ => {
                    unreachable!()
                }
            };
            let ta = Self(ctx.create_instance(new_target, isa).map_err(|_| -1)?);
            if ta.init(ctx, dst_abuf, 0, count).is_err() {
                JS_FreeValue(ctx, ta.0);
                return Err(-1);
            }
            ta
        };

        // copy content from src_ta to new_ta
        if typed_array_size_log2(self.class_id()) == size_log2 {
            // fast path for same element size: copy bytes
            unsafe {
                src_ta.with_bytes_ptr(|src_ptr| {
                    new_ta.with_bytes_mut_ptr(|dst_ptr| {
                        std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, count << size_log2);
                    });
                });
            }
        } else {
            // slow path: get then set as props
            for i in 0..count {
                let val = self.0.get_property_u32(ctx, i as u32);
                if val.is_exception() {
                    JS_FreeValue(ctx, new_ta.0);
                    return Err(-1);
                }

                if new_ta.0.set_property_u32(ctx, i as u32, val).is_err() {
                    JS_FreeValue(ctx, new_ta.0);
                    return Err(-1);
                }
            }
        }

        Ok(new_ta.0)
    }
}

macro_rules! typed_array_obj_of {
    ($name:ident, $ty:ident, $class_id:pat) => {
        #[repr(transparent)]
        #[derive(Clone, Copy)]
        pub struct $name(TypedArrayValue);

        impl $name {
            #[inline(always)]
            pub fn length(self) -> usize {
                self.0.length()
            }

            /// run closure with data slice.
            #[inline(always)]
            pub fn with_data<R>(self, f: impl FnOnce(&[$ty]) -> R) -> R {
                let len = self.length();
                unsafe {
                    self.0
                        .typed_array()
                        .with_bytes_ptr(|ptr| f(std::slice::from_raw_parts(ptr.cast::<$ty>(), len)))
                }
            }

            /// run closure with data slice.
            #[inline(always)]
            pub fn with_data_mut<R>(&mut self, f: impl FnOnce(&mut [$ty]) -> R) -> R {
                let len = self.length();
                unsafe {
                    self.0.typed_array().with_bytes_mut_ptr(|ptr| {
                        f(std::slice::from_raw_parts_mut(ptr.cast::<$ty>(), len))
                    })
                }
            }

            /// run closure with data ptr.
            #[inline(always)]
            pub unsafe fn with_data_ptr<R>(&self, f: impl FnOnce(*const $ty) -> R) -> R {
                self.0
                    .typed_array()
                    .with_bytes_ptr(|ptr| f(ptr.cast::<$ty>()))
            }

            /// run closure with data ptr.
            #[inline(always)]
            pub unsafe fn with_data_mut_ptr<R>(&mut self, f: impl FnOnce(*mut $ty) -> R) -> R {
                self.0
                    .typed_array()
                    .with_bytes_mut_ptr(|ptr| f(ptr.cast::<$ty>()))
            }

            /// get `nth` element value.
            /// if `nth` is out of bound, returns Err
            pub fn element(&self, nth: usize) -> Result<$ty, i32> {
                if nth < self.length() {
                    Ok(unsafe { self.with_data_ptr(|p| *p.add(nth)) })
                } else {
                    Err(-1)
                }
            }

            /// set `nth` element value.
            /// if `nth` is out of bound, returns Err
            pub fn set_element(&mut self, nth: usize, val: $ty) -> Result<(), i32> {
                if nth < self.length() {
                    unsafe {
                        self.with_data_mut_ptr(|p| {
                            *p.add(nth) = val;
                            Ok(())
                        })
                    }
                } else {
                    Err(-1)
                }
            }
        }

        impl<'a> TryFrom<JSValue> for $name {
            type Error = i32;

            fn try_from(value: JSValue) -> Result<Self, Self::Error> {
                if let Some(ob) = value.object() {
                    if matches!(ob.class_id(), $class_id) {
                        Ok(unsafe { std::mem::transmute::<JSValue, $name>(value) })
                    } else {
                        Err(-1)
                    }
                } else {
                    Err(-1)
                }
            }
        }

        impl TryFrom<TypedArrayValue> for $name {
            type Error = i32;

            fn try_from(value: TypedArrayValue) -> Result<Self, Self::Error> {
                if matches!(value.class_id(), $class_id) {
                    Ok(unsafe { std::mem::transmute::<TypedArrayValue, $name>(value) })
                } else {
                    Err(-1)
                }
            }
        }

        impl UncheckedFrom<JSValue> for $name {
            #[inline(always)]
            unsafe fn unchecked_from(value: JSValue) -> $name {
                debug_assert!(value.is_object());
                debug_assert!(matches!(
                    unsafe { value.object_unchecked().class_id() },
                    $class_id
                ));
                std::mem::transmute(value)
            }
        }

        impl UncheckedFrom<TypedArrayValue> for $name {
            #[inline(always)]
            unsafe fn unchecked_from(value: TypedArrayValue) -> $name {
                debug_assert!(matches!(value.class_id(), $class_id));
                std::mem::transmute(value)
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
    /// If value is a TypedArray or DataView, returns TypedArrayValue.
    /// Otherwise, raise Error, and returns Err.
    pub fn typed_array_must(
        self,
        ctx: &mut JSContext,
        is_dataview: bool,
    ) -> Result<TypedArrayValue, i32> {
        if self.is_object() {
            let ob = unsafe { self.object_unchecked() };

            if is_dataview {
                if ob.class_id() != JS_CLASS_DATAVIEW {
                    ctx.JS_ThrowTypeError(c"not a DataView");
                    return Err(-1);
                }
            } else {
                if !(JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&ob.class_id()) {
                    ctx.JS_ThrowTypeError(c"not a TypedArray");
                    return Err(-1);
                }
            }

            Ok(unsafe { self.unchecked_into() })
        } else {
            Err(-1)
        }
    }
}

impl UncheckedFrom<JSValue> for TypedArrayValue {
    /// Inteprete JSValue as TypedArrayValue.
    /// If value is not a TypedArray or DataView, behavior is undefined.
    #[inline(always)]
    unsafe fn unchecked_from(val: JSValue) -> Self {
        debug_assert!(val.is_object());
        debug_assert!(matches!(
            val.object_unchecked().class_id(),
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY | JS_CLASS_DATAVIEW
        ));

        std::mem::transmute(val)
    }
}

impl TryFrom<JSValue> for TypedArrayValue {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if let Some(ob) = value.object() {
            if matches!(
                ob.class_id(),
                JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY | JS_CLASS_DATAVIEW
            ) {
                Ok(unsafe { std::mem::transmute::<JSValue, TypedArrayValue>(value) })
            } else {
                Err(-1)
            }
        } else {
            Err(-1)
        }
    }
}

impl From<TypedArrayValue> for JSValue {
    #[inline(always)]
    fn from(value: TypedArrayValue) -> Self {
        value.0
    }
}

impl JSContext {
    /// create an TypedArray with empty array buffer.
    /// `count`: number of elements
    pub fn create_typed_array(
        &mut self,
        new_target: JSValue,
        count: usize,
        class_id: JSClassID,
    ) -> JSResult {
        // create typed array object
        let ta = TypedArrayValue(
            self.create_instance(
                new_target,
                match class_id {
                    JS_CLASS_UINT8C_ARRAY => JSObjectIsa::UINT8C_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_UINT8_ARRAY => JSObjectIsa::UINT8_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_INT8_ARRAY => JSObjectIsa::INT8_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_UINT16_ARRAY => JSObjectIsa::UINT16_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_INT16_ARRAY => JSObjectIsa::INT16_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_UINT32_ARRAY => JSObjectIsa::UINT32_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_INT32_ARRAY => JSObjectIsa::UINT32_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_FLOAT32_ARRAY => JSObjectIsa::FLOAT32_ARRAY(std::ptr::null_mut()),
                    JS_CLASS_FLOAT64_ARRAY => JSObjectIsa::FLOAT64_ARRAY(std::ptr::null_mut()),
                    _ => {
                        unreachable!()
                    }
                },
            )
            .map_err(|_| -1)?,
        );

        // create array buffer
        let size_log2 = typed_array_size_log2(class_id);
        let buffer = unsafe {
            js_array_buffer_constructor1(self, JSValue::UNDEFINED, count << size_log2)
                .check_exception()
                .inspect_err(|_| {
                    JS_FreeValue(self, ta.into());
                })?
        };

        // bind array buffer to typed array
        if unsafe { ta.init(self, buffer, 0, count).is_err() } {
            JS_FreeValue(self, ta.0);
            return Err(-1);
        } else {
            Ok(ta.0)
        }
    }

    /// create a TypedArray from an existing `source`. The source value could be:
    /// - undefined: create an empty TypedArray;
    /// - TypedArray: create a TypedArray with content copied from source;
    /// - ArrayBuffer, SharedArrayBuffer: create a TypedArray with array buffer;
    /// - An enumerable object: create a TypedArray with content from enumerated index property vals.
    /// Note: `source` is not consumed.
    pub unsafe fn create_typed_array_from(
        &mut self,
        new_target: JSValue,
        source: JSValue,
        offset: usize,
        length: usize,
        class_id: JSClassID,
    ) -> JSResult {
        let isa = match class_id {
            JS_CLASS_UINT8C_ARRAY => JSObjectIsa::UINT8C_ARRAY(std::ptr::null_mut()),
            JS_CLASS_UINT8_ARRAY => JSObjectIsa::UINT8_ARRAY(std::ptr::null_mut()),
            JS_CLASS_INT8_ARRAY => JSObjectIsa::INT8_ARRAY(std::ptr::null_mut()),
            JS_CLASS_UINT16_ARRAY => JSObjectIsa::UINT16_ARRAY(std::ptr::null_mut()),
            JS_CLASS_INT16_ARRAY => JSObjectIsa::INT16_ARRAY(std::ptr::null_mut()),
            JS_CLASS_UINT32_ARRAY => JSObjectIsa::UINT32_ARRAY(std::ptr::null_mut()),
            JS_CLASS_INT32_ARRAY => JSObjectIsa::UINT32_ARRAY(std::ptr::null_mut()),
            JS_CLASS_FLOAT32_ARRAY => JSObjectIsa::FLOAT32_ARRAY(std::ptr::null_mut()),
            JS_CLASS_FLOAT64_ARRAY => JSObjectIsa::FLOAT64_ARRAY(std::ptr::null_mut()),
            _ => {
                unreachable!()
            }
        };
        let size_log2 = typed_array_size_log2(class_id);

        match JS_VALUE_GET_TAG(source) {
            JS_TAG_UNDEFINED => self.create_typed_array(new_target, length, class_id),
            JS_TAG_OBJECT => {
                let p = source.object_unchecked();
                match p.class_id() {
                    JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER => {
                        let abuf = p.isa().array_buffer_ptr();
                        if (*abuf).detached {
                            JS_ThrowTypeErrorDetachedArrayBuffer(self);
                            return Err(-1);
                        }

                        let byte_length = (*abuf).byte_length();

                        if (offset & ((1 << size_log2) - 1) != 0) || offset > byte_length {
                            JS_ThrowRangeError(self, c"invalid offset");
                            return Err(-1);
                        }

                        let count = if length == 0 {
                            if byte_length & ((1 << size_log2) - 1) != 0 {
                                JS_ThrowRangeError(self, c"invalid length");
                                return Err(-1);
                            }
                            (byte_length - offset) >> size_log2
                        } else {
                            if (offset + (length << size_log2)) > byte_length {
                                JS_ThrowRangeError(self, c"invalid length");
                                return Err(-1);
                            }
                            length
                        };

                        let ta =
                            TypedArrayValue(self.create_instance(new_target, isa).map_err(|_| -1)?);

                        if ta.init(self, source.dup(), offset, count).is_err() {
                            JS_FreeValue(self, source);
                            JS_FreeValue(self, ta.into());
                            return Err(-1);
                        }

                        Ok(ta.into())
                    }
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        UncheckedInto::<TypedArrayValue>::unchecked_into(source)
                            .create_to_new(self, new_target, class_id)
                    }
                    _ => {
                        // source is an enumetable object.
                        // get property from source object and set to new array.
                        let iter = source
                            .get_property(self, JS_ATOM_Symbol_iterator)
                            .check_exception()?;

                        let enum_obj: JSValue;
                        let count: usize;
                        if !iter.is_undefined_or_null() {
                            let mut len1: u32 = 0;
                            enum_obj = js_array_from_iterator(self, &mut len1, source, iter);
                            JS_FreeValue(self, iter);
                            if enum_obj.is_exception() {
                                return Err(-1);
                            }
                            count = len1 as usize;
                        } else {
                            count = js_get_length64(self, source)? as usize;
                            enum_obj = source.dup();
                        }

                        let buffer = js_array_buffer_constructor1(
                            self,
                            JSValue::UNDEFINED,
                            count << size_log2,
                        );
                        if buffer.is_exception() {
                            JS_FreeValue(self, enum_obj);
                            return Err(-1);
                        }

                        let ta = TypedArrayValue(self.create_instance(new_target, isa).map_err(
                            |_| {
                                JS_FreeValue(self, enum_obj);
                                JS_FreeValue(self, buffer);
                                -1
                            },
                        )?);
                        if ta.init(self, buffer, 0, count).is_err() {
                            JS_FreeValue(self, enum_obj);
                            JS_FreeValue(self, buffer);
                            JS_FreeValue(self, ta.into());
                            return Err(-1);
                        }

                        // let new_arr = js_create_from_ctor(self, new_target, class_id as _)
                        //     .check_exception()?
                        //     .into_rc(self.rt());

                        // if UncheckedInto::<TypedArrayValue>::unchecked_into(*new_arr)
                        //     .init(self, buffer, 0, count)
                        //     .is_err()
                        // {
                        //     JS_FreeValue(self, enum_obj);
                        //     JS_FreeValue(self, buffer);
                        //     return Err(-1);
                        // }

                        let new_arr = ta.0.into_rc(self.rt());

                        for i in 0..count {
                            let val = enum_obj.get_property_u32(self, i as u32);
                            if val.is_exception() {
                                JS_FreeValue(self, enum_obj);
                                return Err(-1);
                            }
                            if new_arr.set_property_u32(self, i as u32, val).is_err() {
                                JS_FreeValue(self, enum_obj);
                                return Err(-1);
                            }
                        }

                        JS_FreeValue(self, enum_obj);
                        Ok(new_arr.into())
                    }
                }
            }
            _ => Err(-1),
        }
    }
}

// 'obj' must be an allocated typed array object
#[deprecated(note = "use TypedArrayObj::init() instead")]
pub unsafe fn typed_array_init(
    ctx: *mut JSContext,
    obj: JSValueConst,
    buffer: JSValue,
    offset: u64,
    count: u64,
) -> i32 {
    UncheckedInto::<TypedArrayValue>::unchecked_into(obj)
        .init(ctx.as_mut().unwrap(), buffer, offset as _, count as _)
        .map_or(-1, |_| 0)
}

#[deprecated(note = "use JSValue::typed_array() instead")]
pub unsafe fn get_typed_array(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    is_dataview: bool,
) -> *mut JSObject {
    this_val
        .typed_array_must(ctx.as_mut().unwrap(), is_dataview)
        .map_or(std::ptr::null_mut(), |p| {
            p.0.object_unchecked() as *mut JSObject
        })
}

/* WARNING: 'p' must be a typed array. Works even if the array buffer
is detached */
#[deprecated(note = "use TypedArrayValue::capacity() instead")]
pub unsafe fn typed_array_get_length(ctx: *mut JSContext, p: *mut JSObject) -> usize {
    unreachable!("use TypedArrayValue::capacity() instead")
}

/// Get the length of the TypedArray, equivalent to TypedArrayObj::length() but
/// will return an error if the TypedArray is detached.
pub unsafe fn js_typed_array_get_length_internal(
    ctx: &mut JSContext,
    obj: JSValueConst,
) -> Result<u32, i32> {
    let p = obj.typed_array_must(ctx, false)?;
    if p.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        Err(-1)
    } else {
        Ok(p.length() as u32)
    }
}

pub unsafe fn js_typed_array_set_internal(
    ctx: *mut JSContext,
    dst: JSValueConst,
    src: JSValueConst,
    off: JSValueConst,
) -> JSValue {
    let p = if let Ok(p) = dst.typed_array_must(ctx.as_mut().unwrap(), false) {
        p
    } else {
        return JSValue::EXCEPTION;
    };

    if p.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return JSValue::EXCEPTION;
    }

    let mut offset: i64 = 0;
    if JS_ToInt64Sat(ctx, &mut offset, off) != 0 {
        return JSValue::EXCEPTION;
    }
    if offset < 0 {
        JS_ThrowRangeError(ctx, c"invalid array length");
        return JSValue::EXCEPTION;
    }

    let src_obj = JS_ToObject(ctx, src);
    if src_obj.is_exception() {
        JS_FreeValue(ctx, src_obj);
        return JSValue::EXCEPTION;
    }

    let count: i64;

    let src_p = JS_VALUE_GET_OBJ(src_obj);
    if (*src_p).class_id() >= JS_CLASS_UINT8C_ARRAY && (*src_p).class_id() <= JS_CLASS_FLOAT64_ARRAY
    {
        let dest_ta = p.typed_array();
        let dest_abuf = dest_ta.buffer.as_mut().isa_mut().array_buffer_ptr();
        let src_ta = (*src_p).isa().typed_array_ptr();
        let src_abuf = (*src_ta).buffer.as_ref().isa().array_buffer_ptr();
        let shift = typed_array_size_log2(p.class_id());

        if (*src_abuf).detached {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }

        count = (*src_ta).count as i64;
        if offset > p.length() as i64 - count {
            JS_ThrowRangeError(ctx, c"invalid array length");
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }

        // copying between typed objects
        if (*src_p).class_id() == p.class_id() {
            // same type, use mem copy
            let src_offset = (*src_ta).offset;

            (*src_abuf).with_bytes(|src| {
                (*dest_abuf).with_bytes_mut(|dst| {
                    std::ptr::copy(
                        src.as_ptr().offset(src_offset as isize),
                        dst.as_mut_ptr(),
                        (count << shift) as usize, // count to byte_num
                    );
                });
            });

            // let dest_ptr = (*dest_abuf)
            //     .data_mut_ptr()
            //     .offset((*dest_ta).offset as isize + (offset << shift) as isize);
            // let src_ptr = (*src_abuf).data_ptr().offset((*src_ta).offset as isize);
            // std::ptr::copy(src_ptr, dest_ptr, (src_len << shift) as usize);

            JS_FreeValue(ctx, src_obj);
            return JSValue::UNDEFINED;
        }

        if (*dest_abuf).content.id() == (*src_abuf).content.id() {
            // copying between the same buffer using different types of mappings
            // would require a temporary buffer
            todo!();

            return JSValue::UNDEFINED;
        }
    } else {
        count = if let Ok(n) = js_get_length64(ctx, src_obj) {
            n
        } else {
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        };

        if offset > p.length() as i64 - count {
            JS_ThrowRangeError(ctx, c"invalid array length");
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }
    }

    for i in 0..count as u32 {
        let val = src_obj.get_property_u32(ctx.as_mut().unwrap(), i);
        if val.is_exception() {
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }

        if dst
            .set_property_u32(ctx.as_mut().unwrap(), offset as u32 + i, val)
            .is_err()
        {
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }
    }

    JS_FreeValue(ctx, src_obj);
    JSValue::UNDEFINED
}

pub unsafe fn js_typed_array_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let ta = val
        .object_unchecked()
        .isa_mut()
        .typed_array_replace(std::ptr::null_mut())
        .unwrap();

    if !ta.is_null() {
        // during the GC the finalizers are called in an arbitrary
        // order so the ArrayBuffer finalizer may have been called
        if !((*ta).link.next).is_null() {
            list_del(&mut (*ta).link);
        }
        JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*ta).buffer.as_ptr().cast()));

        drop(Box::from_raw(ta));
    }
}

pub unsafe fn js_typed_array_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let ta = val.object_unchecked().isa().typed_array_ptr();
    if !ta.is_null() {
        JS_MarkValue(
            rt,
            JS_MKPTR!(JSTag::Object, (*ta).buffer.as_ptr().cast()),
            mark_func,
        );
    }
}

/* WARNING: 'p' must be a typed array */
#[deprecated(note = "use TypedArrayValue::is_detached() instead")]
pub unsafe fn typed_array_is_detached(p: *mut JSObject) -> bool {
    unreachable!("use TypedArrayValue::is_detached() instead")
}
