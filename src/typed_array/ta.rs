use std::{marker::PhantomData, ops::DerefMut, ptr::NonNull};

use crate::{
    array_buffer::{js_array_buffer_constructor1, JSArrayBuffer},
    atom::consts::JS_ATOM_Symbol_iterator,
    class::{js_create_from_ctor, JSClassID},
    context::{JSContext, JSContextHandle},
    error::{JSResult, JS_ThrowRangeError, JS_ThrowTypeErrorDetachedArrayBuffer},
    gc::{JS_MarkFunc, JS_MarkValue},
    iterator::js_array_from_iterator,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{list_add_tail, list_del, list_head},
    value::{
        object::{js_get_length64, JSObject, JS_ToObject},
        JSTag, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_ToInt64Sat, JS_TAG_OBJECT,
        JS_TAG_UNDEFINED, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER,
    JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
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
    pub link: list_head,    /* link to arraybuffer */
    pub obj: *mut JSObject, /* back pointer to the TypedArray/DataView object */

    /// pointer of underlaying array buffer object
    pub buffer: NonNull<JSObject>,
    /// start offset in array buffer  
    pub offset: u32,
    /// number of bytes from array buffer
    pub byte_length: u32,
}

impl JSTypedArray {
    /// check if `self` TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        unsafe {
            // XXX: could simplify test by ensuring typed_array.is_null
            // (*self.0.u.array).data.ptr.is_null()

            let abuf = self.buffer.as_ref().u.array_buffer;
            (*abuf).detached
        }
    }

    /// get underlaying array buffer object.
    #[inline(always)]
    pub fn array_buffer_obj(&self) -> &JSObject {
        unsafe { self.buffer.as_ref() }
    }

    /// get underlaying array buffer.
    #[inline(always)]
    pub fn array_buffer(&self) -> &JSArrayBuffer {
        unsafe { self.buffer.as_ref().u.array_buffer.as_ref().unwrap() }
    }

    /// get underlaying array buffer.
    #[inline(always)]
    pub fn array_buffer_mut(&mut self) -> &mut JSArrayBuffer {
        unsafe { self.buffer.as_ref().u.array_buffer.as_mut().unwrap() }
    }

    /// run closure with data slice casted as type T.
    /// T should be one of i8, u8, i16, u16, i32, u32, i64, u64, f32, f64.
    pub fn with_data<T, R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        debug_assert!(matches!(std::mem::size_of::<T>(), 1 | 2 | 4 | 8));

        let base = self.offset as usize;
        let byte_length = self.byte_length as usize;

        self.array_buffer().with_bytes(|buf| {
            let lst = unsafe {
                std::slice::from_raw_parts(
                    buf.as_ptr().add(base).cast::<T>(),
                    byte_length / std::mem::size_of::<T>(),
                )
            };
            f(lst)
        })
    }

    /// run closure with data slice casted as type T.
    /// T should be one of i8, u8, i16, u16, i32, u32, i64, u64, f32, f64.
    pub fn with_data_mut<T, R>(&mut self, f: impl FnOnce(&mut [T]) -> R) -> R {
        debug_assert!(matches!(std::mem::size_of::<T>(), 1 | 2 | 4 | 8));

        let base = self.offset as usize;
        let byte_length = self.byte_length as usize;

        self.array_buffer_mut().with_bytes_mut(|buf| {
            let lst = unsafe {
                std::slice::from_raw_parts_mut(
                    buf.as_mut_ptr().add(base).cast::<T>(),
                    byte_length / std::mem::size_of::<T>(),
                )
            };
            f(lst)
        })
    }
}

#[repr(transparent)]
pub struct TypedArrayObj<'a> {
    pub(super) obj: NonNull<JSObject>,
    mark: PhantomData<&'a ()>,
}

impl<'a> TypedArrayObj<'a> {
    #[inline(always)]
    pub fn class_id(&self) -> JSClassID {
        unsafe { self.obj.as_ref().class_id() }
    }

    /// get typed array data raw pointer.
    #[inline(always)]
    pub fn typed_array_ptr(&self) -> NonNull<JSTypedArray> {
        unsafe { NonNull::new_unchecked((*self.obj.as_ref()).u.typed_array) }
    }

    /// get typed array data.
    #[inline(always)]
    pub fn typed_array(&self) -> &JSTypedArray {
        unsafe { self.typed_array_ptr().as_ref() }
    }

    /// get typed array data.
    #[inline(always)]
    pub fn typed_array_mut(&self) -> &mut JSTypedArray {
        unsafe { self.typed_array_ptr().as_mut() }
    }

    /// get underlaying array buffer object.
    #[inline(always)]
    pub fn array_buffer_obj(&self) -> &JSObject {
        unsafe { (*(*self.obj.as_ref()).u.typed_array).buffer.as_ref() }
    }

    /// setup typed array with array buffer, offset in array buffer and type array element count.
    /// Note: array `buffer` is consumed.
    fn init(
        &mut self,
        ctx: &mut JSContext,
        buffer: JSValue,
        offset: usize,
        count: usize,
    ) -> Result<(), i32> {
        let size_log2 = typed_array_size_log2(self.class_id());

        let mut ta = unsafe {
            if let Some(p) = js_malloc_typed::<JSTypedArray>(ctx, 1, false) {
                Box::from_raw(p.as_ptr())
            } else {
                JS_FreeValue(ctx, buffer);
                return Err(-1);
            }
        };

        let pbuffer = unsafe { buffer.object_unchecked() };
        let mut abuf = unsafe { pbuffer.u.array_buffer };

        ta.obj = self.obj.as_ptr();
        ta.buffer = NonNull::from(pbuffer);
        ta.offset = offset as u32;
        ta.byte_length = (count as u32) << size_log2;

        unsafe {
            list_add_tail(&mut ta.link, &mut (*abuf).array_list);

            let obj = self.obj.as_mut();

            obj.u.typed_array = Box::into_raw(ta);
            (*obj.u.array).typed_array_count = count as u32;
            (*obj.u.array).data.ptr = (*abuf).content.as_mut_ptr().offset(offset as isize).cast();
        }

        Ok(())
    }

    /// check if the TypedArray is detached.
    #[inline(always)]
    pub fn is_detached(&self) -> bool {
        self.typed_array().is_detached()
    }

    /// Get count of elements in typed array.
    /// If underlaying array buffer is detached, returns 0.
    #[inline(always)]
    pub fn length(&self) -> usize {
        unsafe { self.obj.as_ref().u.array.typed_array_count as usize }
    }

    /// Get count of elements according to underlying array buffer space.
    /// Works even if the array buffer is detached.
    pub fn capacity(&self) -> usize {
        let ta = self.typed_array();
        let size_log2 = typed_array_size_log2(self.class_id());
        (ta.byte_length >> size_log2) as usize
    }

    /// Get underlying array buffer byte offset.
    /// When underlaying array buffer is detached:
    /// - is_dataview=true, returns Err.
    /// - is_dataview=false, returns 0.
    pub fn array_buffer_byte_offset(
        &self,
        ctx: &mut JSContext,
        is_dataview: bool,
    ) -> Result<usize, i32> {
        if self.is_detached() {
            return if is_dataview {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                Err(-1)
            } else {
                Ok(0)
            };
        } else {
            Ok(self.typed_array().offset as usize)
        }
    }

    /// run closure with data slice casted as type T.
    /// T should be one of i8, u8, i16, u16, i32, u32, i64, u64, f32, f64.
    #[inline(always)]
    pub fn with_data<T, R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        self.typed_array().with_data(f)
    }

    /// run closure with data slice casted as type T.
    /// T should be one of i8, u8, i16, u16, i32, u32, i64, u64, f32, f64.
    #[inline(always)]
    pub fn with_data_mut<T, R>(&mut self, f: impl FnOnce(&mut [T]) -> R) -> R {
        self.typed_array_mut().with_data_mut(f)
    }

    /// create a new TypedArray object with content copy from `self` array.
    pub fn create_new(
        &mut self,
        ctx: &mut JSContext,
        new_target: JSValueConst,
        class_id: JSClassID,
    ) -> JSResult {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let ta = self.typed_array();
        let count = self.length();

        let src_abuf = unsafe { ta.buffer.as_ref().u.array_buffer };
        let size_log2 = typed_array_size_log2(class_id as _);

        let buffer = unsafe {
            js_array_buffer_constructor1(ctx, JSValue::UNDEFINED, count << size_log2)
                .check_exception()?
        };

        // necessary to check detacth again, because it could have been detached
        if self.is_detached() {
            unsafe {
                JS_FreeValue(ctx, buffer);
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            }
            return Err(-1);
        }

        let abuf = unsafe {
            self.obj
                .as_ref()
                .opaque::<JSArrayBuffer>(JS_CLASS_ARRAY_BUFFER)
                .unwrap()
                .as_mut()
        };

        let new_arr = unsafe { js_create_from_ctor(ctx, new_target, class_id).check_exception()? };

        if unsafe { new_arr.typed_array_obj_unchecked() }
            .init(ctx, buffer, 0, count)
            .is_err()
        {
            unsafe {
                JS_FreeValue(ctx, new_arr);
            }
            return Err(-1);
        }

        if self.class_id() == class_id {
            // same type: copy the content
            unsafe {
                (*src_abuf).with_bytes(|src| {
                    let cnt = abuf.byte_length();

                    abuf.with_bytes_mut(|dst| {
                        std::ptr::copy_nonoverlapping(
                            src.as_ptr().offset((*ta).offset as isize),
                            dst.as_mut_ptr(),
                            cnt,
                        );
                    });
                });
            }
        } else {
            let src_obj = JS_MKPTR!(JSTag::Object, self.obj.as_ptr().cast());

            for i in 0..count {
                let val = src_obj.get_property_u32(ctx, i as u32);
                if val.is_exception() {
                    unsafe {
                        JS_FreeValue(ctx, new_arr);
                    }
                    return Err(-1);
                }

                if new_arr.set_property_u32(ctx, i as u32, val).is_err() {
                    unsafe {
                        JS_FreeValue(ctx, new_arr);
                    }
                    return Err(-1);
                }
            }
        }

        Ok(new_arr)
    }
}

impl JSObject {
    /// Cast object `self` as TypedArrayObj. if `self` is not a TypedArray or DataView, behavior is undefined.
    #[inline(always)]
    pub unsafe fn as_typed_array_obj_unchecked(&self) -> TypedArrayObj {
        debug_assert!(matches!(
            self.class_id(),
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY | JS_CLASS_DATAVIEW
        ));

        std::mem::transmute(self)
    }

    /// Cast object `self` as TypedArrayObj. if `self` is not a TypedArray or DataView, returns None.
    #[inline]
    pub fn as_typed_array_obj(&self) -> Option<TypedArrayObj> {
        if matches!(
            self.class_id(),
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY | JS_CLASS_DATAVIEW
        ) {
            Some(unsafe { std::mem::transmute(self) })
        } else {
            None
        }
    }

    /// If object `self` is of class TYPED_ARRAY or DATA_VIEW, cast object as TypedArrayObj.
    /// Otherwise, throw exception, and returns Err.
    pub fn as_typed_array_obj_must<X: Into<JSContextHandle>>(
        &self,
        ctx: X,
        is_dataview: bool,
    ) -> Result<TypedArrayObj, i32> {
        let mut ctx: JSContextHandle = ctx.into();

        if is_dataview {
            if self.class_id() != JS_CLASS_DATAVIEW {
                ctx.JS_ThrowTypeError(c"not a DataView");
                return Err(-1);
            }
        } else {
            if !(JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&self.class_id()) {
                ctx.JS_ThrowTypeError(c"not a TypedArray");
                return Err(-1);
            }
        }

        Ok(unsafe { self.as_typed_array_obj_unchecked() })
    }
}

impl JSValue {
    /// Get inner TypedArrayObj of typed array value.
    /// If value is not a TypedArray or DataView, behavior is undefined.
    #[inline]
    pub unsafe fn typed_array_obj_unchecked<'a>(self) -> TypedArrayObj<'a> {
        self.object_unchecked().as_typed_array_obj_unchecked()
    }

    /// If value is a TypedArray or DataView, returns the inner TypedArrayObj.
    /// Otherwise, raise Error to ctx, and returns Err.
    pub fn typed_array_obj<'a, X: Into<JSContextHandle>>(
        self,
        ctx: X,
        is_dataview: bool,
    ) -> Result<TypedArrayObj<'a>, i32> {
        let mut ctx: JSContextHandle = ctx.into();

        if !self.is_object() {
            ctx.deref_mut().JS_ThrowTypeError(if is_dataview {
                c"not a DataView"
            } else {
                c"not a TypedArray"
            });
            Err(-1)
        } else {
            unsafe { self.object_unchecked() }.as_typed_array_obj_must(ctx, is_dataview)
        }
    }
}

impl JSContext {
    /// create an TypedArray with empty array buffer.
    /// `count`: number of elements
    pub unsafe fn create_typed_array(
        &mut self,
        new_target: JSValue,
        count: usize,
        classid: JSClassID,
    ) -> JSResult {
        let size_log2 = typed_array_size_log2(classid);

        let buffer = unsafe {
            js_array_buffer_constructor1(self, JSValue::UNDEFINED, count << size_log2)
                .check_exception()?
        };

        let obj = unsafe { js_create_from_ctor(self, new_target, classid).check_exception()? };
        if obj
            .typed_array_obj_unchecked()
            .init(self, buffer, 0, count)
            .is_err()
        {
            JS_FreeValue(self, buffer);
            JS_FreeValue(self, obj);
            return Err(-1);
        }

        Ok(obj)
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
        classid: JSClassID,
    ) -> JSResult {
        let size_log2 = typed_array_size_log2(classid);

        match JS_VALUE_GET_TAG(source) {
            JS_TAG_UNDEFINED => {
                return self.create_typed_array(new_target, length, classid);
            }
            JS_TAG_OBJECT => {
                let p = source.object_unchecked();
                match p.class_id() {
                    JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER => {
                        let abuf = p.u.array_buffer;
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

                        let buffer = source.dup();

                        let obj =
                            js_create_from_ctor(self, new_target, classid).check_exception()?;
                        if obj
                            .typed_array_obj_unchecked()
                            .init(self, buffer, offset, count)
                            .is_err()
                        {
                            JS_FreeValue(self, buffer);
                            JS_FreeValue(self, obj);
                            return Err(-1);
                        }

                        Ok(obj)
                    }
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        return p
                            .as_typed_array_obj_unchecked()
                            .create_new(self, new_target, classid);
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

                        let new_arr = js_create_from_ctor(self, new_target, classid as _)
                            .check_exception()?
                            .into_rc(self.rt());

                        if new_arr
                            .typed_array_obj_unchecked()
                            .init(self, buffer, 0, count)
                            .is_err()
                        {
                            JS_FreeValue(self, enum_obj);
                            JS_FreeValue(self, buffer);
                            return Err(-1);
                        }

                        for i in 0..count {
                            let val = enum_obj.get_property_u32(self, i as u32);
                            if val.is_exception() {
                                JS_FreeValue(self, enum_obj);
                                JS_FreeValue(self, buffer);
                                return Err(-1);
                            }

                            if new_arr.set_property_u32(self, i as u32, val).is_err() {
                                JS_FreeValue(self, enum_obj);
                                JS_FreeValue(self, buffer);
                                return Err(-1);
                            }
                        }

                        JS_FreeValue(self, enum_obj);
                        return Ok(new_arr.into());
                    }
                }
            }
            _ => {
                return Err(-1);
            }
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
    obj.typed_array_obj_unchecked()
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
        .typed_array_obj(ctx, is_dataview)
        .map_or(std::ptr::null_mut(), |p| p.obj.as_ptr())
}

/* WARNING: 'p' must be a typed array. Works even if the array buffer
is detached */
#[deprecated(note = "use TypedArrayObj::array_buffer_length() instead")]
pub unsafe fn typed_array_get_length(ctx: *mut JSContext, p: *mut JSObject) -> usize {
    (*p).as_typed_array_obj().unwrap().capacity()
}

/// Get the length of the TypedArray, equivalent to TypedArrayObj::length() but
/// will return an error if the TypedArray is detached.
pub unsafe fn js_typed_array_get_length_internal(
    ctx: &mut JSContext,
    obj: JSValueConst,
) -> Result<u32, i32> {
    let p = obj.typed_array_obj(ctx.handle(), false)?;
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
    let p = get_typed_array(ctx, dst, false);
    if p.is_null() {
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

    if typed_array_is_detached(p) {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
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
        let dest_ta: *mut JSTypedArray = (*p).u.typed_array;
        let dest_abuf = (*dest_ta).buffer.as_mut().u.array_buffer;
        let src_ta = (*src_p).u.typed_array;
        let src_abuf = (*src_ta).buffer.as_ref().u.array_buffer;
        let shift = typed_array_size_log2((*p).class_id());

        if (*src_abuf).detached {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }

        count = (*src_p).u.array.typed_array_count as i64;
        if offset > (*p).u.array.typed_array_count as i64 - count {
            JS_ThrowRangeError(ctx, c"invalid array length");
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }

        // copying between typed objects
        if (*src_p).class_id() == (*p).class_id() {
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

        if offset > (*p).u.array.typed_array_count as i64 - count {
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
    let p = val.object_unchecked();
    let ta = p.u.typed_array;
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
    let p = val.object_unchecked();
    let ta = p.u.typed_array;
    if !ta.is_null() {
        JS_MarkValue(
            rt,
            JS_MKPTR!(JSTag::Object, (*ta).buffer.as_ptr().cast()),
            mark_func,
        );
    }
}

/* WARNING: 'p' must be a typed array */
#[deprecated(note = "use TypedArrayObj::is_detached() instead")]
pub unsafe fn typed_array_is_detached(p: *mut JSObject) -> bool {
    (*p).as_typed_array_obj().unwrap().is_detached()

    // let ta = (*p).u.typed_array;
    // let abuf = (*(*ta).buffer).u.array_buffer;

    // /* XXX: could simplify test by ensuring that
    // p->u.array.u.ptr is NULL iff it is detached */
    // (*abuf).detached
}
