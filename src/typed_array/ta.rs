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

pub struct JSTypedArray {
    pub link: list_head,    /* link to arraybuffer */
    pub obj: *mut JSObject, /* back pointer to the TypedArray/DataView object */

    /// based array buffer  
    pub buffer: *mut JSObject,
    /// offset in array buffer  
    pub offset: u32,
    /// length of array buffer   
    pub length: u32,
}

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

#[repr(transparent)]
pub struct TypedArrayObj<'a> {
    pub(super) obj: NonNull<JSObject>,
    mark: PhantomData<&'a ()>,
}

impl<'a> TypedArrayObj<'a> {
    pub fn class_id(&self) -> JSClassID {
        unsafe { self.obj.as_ref().class_id() }
    }

    /// get typed array state.
    #[inline(always)]
    pub fn typed_array(&self) -> &JSTypedArray {
        unsafe { (*self.obj.as_ref()).u.typed_array.as_ref().unwrap() }
    }

    /// get underlaying array buffer object.
    #[inline(always)]
    pub fn array_buffer_obj(&self) -> &JSObject {
        unsafe {
            (*(*self.obj.as_ref()).u.typed_array)
                .buffer
                .as_ref()
                .unwrap()
        }
    }

    /// setup typed array with array buffer, offset and length.
    /// Note: array `buffer` is consumed.
    pub fn init(
        &mut self,
        ctx: &mut JSContext,
        buffer: JSValue,
        offset: u64,
        len: u64,
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
        let abuf = unsafe { pbuffer.u.array_buffer };

        ta.obj = self.obj.as_ptr();
        ta.buffer = pbuffer;
        ta.offset = offset as u32;
        ta.length = (len as u32) << size_log2;

        unsafe {
            list_add_tail(&mut ta.link, &mut (*abuf).array_list);
        }

        unsafe {
            let obj = self.obj.as_mut();

            obj.u.typed_array = Box::into_raw(ta);
            (*obj.u.array).typed_array_count = len as u32;
            (*obj.u.array).data.ptr = (*abuf).data.offset(offset as isize).cast();
        }

        Ok(())
    }

    /// check if the TypedArray is detached.
    pub fn is_detached(&self) -> bool {
        // XXX: could simplify test by ensuring typed_array.is_null
        // unsafe { (*self.0.u.array).data.ptr.is_null() }

        unsafe {
            let abuf = (*(*self.obj.as_ref().u.typed_array).buffer).u.array_buffer;
            (*abuf).detached
        }
    }

    /// Get typed array length (count of elements).
    /// If underlaying array buffer is detached, returns 0.
    #[inline(always)]
    pub fn length(&self) -> usize {
        unsafe { self.obj.as_ref().u.array.typed_array_count as usize }
    }

    /// Get underlying array buffer length (count of elements).
    /// Works even if the array buffer is detached.
    pub fn array_buffer_length(&self) -> usize {
        let ta = unsafe { self.obj.as_ref().u.typed_array };
        let size_log2 = typed_array_size_log2(self.class_id());
        unsafe { ((*ta).length >> size_log2) as usize }
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
                unsafe {
                    JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                }
                Err(-1)
            } else {
                Ok(0)
            };
        } else {
            Ok(self.typed_array().offset as usize)
        }
    }

    /// get typed array data pointer as type T.
    /// T should be one of i8, u8, i16, u16, i32, u32, i64, u64, f32, f64.
    /// if array is detached, behaviour is undefined.
    #[inline(always)]
    pub unsafe fn data_ptr_unchecked<T>(&self) -> NonNull<T> {
        debug_assert!(!self.is_detached());
        NonNull::new_unchecked(self.obj.as_ref().u.array.data.ptr.cast::<T>())
    }

    /// get data ptr and count
    fn data_ptr_and_count<T>(&self) -> (NonNull<T>, usize) {
        let len = self.length();
        let count = if len > 0 {
            let shift = typed_array_size_log2(self.class_id());
            if (1 << shift) == std::mem::size_of::<T>() as u8 {
                len
            } else {
                len as usize * (1 << shift) / std::mem::size_of::<T>()
            }
        } else {
            0
        };

        (
            unsafe { NonNull::new_unchecked(self.obj.as_ref().u.array.data.ptr.cast::<T>()) },
            count,
        )
    }

    /// get data as slice
    #[inline]
    pub fn as_slice<T>(&self) -> &[T] {
        let (ptr, count) = self.data_ptr_and_count::<T>();
        if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(ptr.as_ptr(), count) }
        }
    }

    /// get data as slice mut
    #[inline]
    pub fn as_slice_mut<T>(&mut self) -> &mut [T] {
        let (ptr, count) = self.data_ptr_and_count::<T>();
        if count == 0 {
            &mut []
        } else {
            unsafe { std::slice::from_raw_parts_mut(ptr.as_ptr(), count) }
        }
    }

    /// create a new TypedArray object with content copy from `self` array.
    pub fn create_new(
        &mut self,
        ctx: &mut JSContext,
        new_target: JSValueConst,
        classid: JSClassID,
    ) -> JSResult {
        if self.is_detached() {
            unsafe {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            }
            return Err(-1);
        }

        let ta = self.typed_array();
        let len = self.length();
        let src_buffer = ta.buffer;
        let src_abuf = unsafe { (*src_buffer).u.array_buffer };
        let size_log2 = typed_array_size_log2(classid as _);

        let buffer = unsafe {
            js_array_buffer_constructor1(ctx, JSValue::UNDEFINED, (len as u64) << size_log2)
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
                .as_ref()
        };

        let new_arr = unsafe { js_create_from_ctor(ctx, new_target, classid).check_exception()? };
        if unsafe { new_arr.typed_array_obj_unchecked() }
            .init(ctx, buffer, 0, len as _)
            .is_err()
        {
            unsafe {
                JS_FreeValue(ctx, new_arr);
            }
            return Err(-1);
        }

        if self.class_id() == classid {
            // same type: copy the content
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (*src_abuf).data.offset((*ta).offset as isize),
                    abuf.data,
                    abuf.byte_length as usize,
                );
            }
        } else {
            let src_obj = JS_MKPTR!(JSTag::Object, self.obj.as_ptr().cast());

            for i in 0..len {
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
    /// Convert object to TypedArrayObj. if Object is not a TypedArray or DataView, returns None.
    #[inline]
    pub fn as_typed_array_obj(&mut self) -> Option<TypedArrayObj> {
        if matches!(
            self.class_id(),
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY | JS_CLASS_DATAVIEW
        ) {
            Some(unsafe { std::mem::transmute(self) })
            // Some(TypedArrayObj {
            //     obj: unsafe { NonNull::new_unchecked(self) },
            //     mark: PhantomData,
            // })
        } else {
            None
        }
    }

    /// Convert object to TypedArrayObj. if Object is not a TypedArray or DataView, behavior is undefined.
    #[inline(always)]
    pub unsafe fn as_typed_array_obj_unchecked(&mut self) -> TypedArrayObj {
        debug_assert!(matches!(
            self.class_id(),
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY | JS_CLASS_DATAVIEW
        ));

        std::mem::transmute(self)
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

        if JS_VALUE_GET_TAG(self) != JS_TAG_OBJECT {
            ctx.deref_mut().JS_ThrowTypeError(if is_dataview {
                c"not a DataView"
            } else {
                c"not a TypedArray"
            });
            return Err(-1);
        }

        let p = unsafe { self.object_unchecked() };
        if is_dataview {
            if p.class_id() != JS_CLASS_DATAVIEW {
                ctx.JS_ThrowTypeError(if is_dataview {
                    c"not a DataView"
                } else {
                    c"not a TypedArray"
                });
                return Err(-1);
            }
        } else {
            if !((JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&p.class_id())) {
                ctx.JS_ThrowTypeError(if is_dataview {
                    c"not a DataView"
                } else {
                    c"not a TypedArray"
                });
                return Err(-1);
            }
        }

        Ok(unsafe { p.as_typed_array_obj_unchecked() })
    }
}

impl JSContext {
    /// create an TypedArray with empty array buffer.
    pub unsafe fn create_typed_array(
        &mut self,
        new_target: JSValue,
        len: usize,
        classid: JSClassID,
    ) -> JSResult {
        let size_log2 = typed_array_size_log2(classid);

        let buffer = unsafe {
            js_array_buffer_constructor1(self, JSValue::UNDEFINED, (len << size_log2) as u64)
                .check_exception()?
        };

        let obj = unsafe { js_create_from_ctor(self, new_target, classid).check_exception()? };
        if obj
            .typed_array_obj_unchecked()
            .init(self, buffer, 0, len as u64)
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

                        if (offset & ((1 << size_log2) - 1) != 0)
                            || offset > (*abuf).byte_length as usize
                        {
                            JS_ThrowRangeError(self, c"invalid offset");
                            return Err(-1);
                        }

                        let len = if length == 0 {
                            if (*abuf).byte_length & ((1 << size_log2) - 1) != 0 {
                                JS_ThrowRangeError(self, c"invalid length");
                                return Err(-1);
                            }
                            ((*abuf).byte_length as usize - offset) >> size_log2
                        } else {
                            if (offset + (length << size_log2)) > (*abuf).byte_length as usize {
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
                            .init(self, buffer, offset as u64, len as u64)
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
                        let len: i64;

                        if !iter.is_undefined_or_null() {
                            let mut len1: u32 = 0;
                            enum_obj = js_array_from_iterator(self, &mut len1, source, iter);
                            JS_FreeValue(self, iter);
                            if enum_obj.is_exception() {
                                return Err(-1);
                            }
                            len = len1 as i64;
                        } else {
                            len = js_get_length64(self, source)?;
                            enum_obj = source.dup();
                        }

                        let buffer = js_array_buffer_constructor1(
                            self,
                            JSValue::UNDEFINED,
                            (len as u64) << size_log2,
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
                            .init(self, buffer, 0, len as u64)
                            .is_err()
                        {
                            JS_FreeValue(self, enum_obj);
                            JS_FreeValue(self, buffer);
                            return Err(-1);
                        }

                        for i in 0..len {
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
    len: u64,
) -> i32 {
    obj.typed_array_obj_unchecked()
        .init(ctx.as_mut().unwrap(), buffer, offset, len)
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
    (*p).as_typed_array_obj().unwrap().array_buffer_length()
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

    let src_len: i64;

    let src_p = JS_VALUE_GET_OBJ(src_obj);
    if (*src_p).class_id() >= JS_CLASS_UINT8C_ARRAY && (*src_p).class_id() <= JS_CLASS_FLOAT64_ARRAY
    {
        let dest_ta: *mut JSTypedArray = (*p).u.typed_array;
        let dest_abuf = (*(*dest_ta).buffer).u.array_buffer;
        let src_ta = (*src_p).u.typed_array;
        let src_abuf = (*(*src_ta).buffer).u.array_buffer;
        let shift = typed_array_size_log2((*p).class_id());

        if (*src_abuf).detached {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }

        src_len = (*src_p).u.array.typed_array_count as i64;
        if offset > (*p).u.array.typed_array_count as i64 - src_len {
            JS_ThrowRangeError(ctx, c"invalid array length");
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }

        // copying between typed objects
        if (*src_p).class_id() == (*p).class_id() {
            // same type, use memmove
            let dest_ptr = (*dest_abuf)
                .data
                .offset((*dest_ta).offset as isize + (offset << shift) as isize);
            let src_ptr = (*src_abuf).data.offset((*src_ta).offset as isize);
            std::ptr::copy(dest_ptr, src_ptr, (src_len << shift) as usize);

            JS_FreeValue(ctx, src_obj);
            return JSValue::UNDEFINED;
        }
        if (*dest_abuf).data == (*src_abuf).data {
            // copying between the same buffer using different types of mappings
            // would require a temporary buffer
        }
        // otherwise, default behavior is slow but correct
    } else {
        src_len = if let Ok(n) = js_get_length64(ctx, src_obj) {
            n
        } else {
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        };

        if offset > (*p).u.array.typed_array_count as i64 - src_len {
            JS_ThrowRangeError(ctx, c"invalid array length");
            JS_FreeValue(ctx, src_obj);
            return JSValue::EXCEPTION;
        }
    }

    for i in 0..src_len as u32 {
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
        JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*ta).buffer.cast()));

        drop(Box::from_raw(ta));
    }
}

pub unsafe fn js_typed_array_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let p = val.object_unchecked();
    let ta = p.u.typed_array;
    if !ta.is_null() {
        JS_MarkValue(rt, JS_MKPTR!(JSTag::Object, (*ta).buffer.cast()), mark_func);
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
