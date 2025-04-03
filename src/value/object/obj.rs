use std::{
    ffi::{c_void, CStr},
    mem::ManuallyDrop,
};

use crate::{
    array_buffer::JSArrayBuffer,
    atom::{
        consts::*, js_symbol_to_atom, JSAtom, JSAtomTypeHint, JS_FreeAtom, JS_ValueToAtom,
        JS_ATOM_NULL,
    },
    class::JSClassID,
    context::{JSContext, JS_FreeContext},
    error::JS_ThrowTypeErrorPrivateNotFound,
    function::{
        js_class_has_bytecode, js_poll_interrupts, JSAsyncFunctionState, JSBoundFunction,
        JSCFunction, JSCFunctionDataRecord, JSObjectCFunc, JSObjectFunc, JS_CallFree,
    },
    gc::{remove_gc_object, JSGCObjectHeader, JSGCPhaseEnum},
    generator::{JSAsyncGeneratorData, JSGeneratorData},
    iterator::{
        JSArrayIteratorData, JSAsyncFromSyncIteratorData, JSForInIterator, JSMapIteratorData,
    },
    map::{JSMapRecord, JSMapState},
    mem::js_free_rt,
    promise::{JSPromiseData, JSPromiseFunctionData},
    proxy::{
        js_proxy_getPrototypeOf, js_proxy_preventExtensions, js_proxy_setPrototypeOf, JSProxyData,
    },
    regexp::{JSRegExp, JSRegExpStringIteratorData},
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    types::{list_add_tail, list_del, unlikely},
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN,
    JS_CLASS_BOUND_FUNCTION, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION, JS_CLASS_DATE,
    JS_CLASS_ERROR, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_PROXY,
    JS_CLASS_STRING, JS_CLASS_SYMBOL, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        primitives::{JS_GetPrototypePrimitive, JS_ToBoolFree},
        string::JS_ConcatString3,
        symbol::{JS_AtomSymbolHasDescription, JS_NewSymbolFromAtom},
        value::{
            set_value, JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_FreeValueRT,
            JS_IsException, JS_IsNull, JS_IsObject, JS_IsUndefined, JS_TAG_NULL, JS_TAG_OBJECT,
            JS_TAG_STRING, JS_TAG_SYMBOL, JS_TAG_UNDEFINED, JS_UNDEFINED, JS_VALUE_GET_NORM_TAG,
            JS_VALUE_GET_OBJ, JS_VALUE_GET_STRING, JS_VALUE_GET_TAG,
        },
    },
    property::{
        add_property, find_own_property, find_own_property1, free_property, JSProperty,
        JSPropertyDescriptor, JSPropertyEnum, JS_DefineProperty, JS_DefinePropertyDesc,
        JS_DefinePropertyValue, JS_GetOwnPropertyInternal, JS_GetProperty, JS_GPN_ENUM_ONLY,
        JS_GPN_STRING_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_AUTOINIT, JS_PROP_CONFIGURABLE,
        JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE,
        JS_PROP_NORMAL, JS_PROP_THROW, JS_PROP_TMASK, JS_PROP_VARREF, JS_PROP_WRITABLE,
    },
    property_name::{js_free_desc, js_free_prop_enum, JS_GetOwnPropertyNamesInternal},
    shape::{
        find_hashed_shape_proto, js_dup_shape, js_free_shape, js_new_shape, JSShape,
        JS_NewObjectFromShape,
    },
};

#[repr(C)]
pub struct JSObject {
    pub header: JSGCObjectHeader,

    /// object flags
    pub detailed: JSObjectDetailed,

    /// Shape of object, to denote prototype and property names + flag
    /// could be NULL while freeing an object
    pub shape: *mut JSShape,

    /// property value slots
    pub prop_values: Vec<JSProperty>,

    pub first_weak_ref: *mut JSMapRecord, /* XXX: use a bit and an external hash table? */

    /// payload
    pub u: JSObjectUnion,
}

impl JSObject {
    /// get object shape.
    /// could be None when object is being freed
    #[inline(always)]
    pub fn shape(&self) -> Option<&JSShape> {
        unsafe { self.shape.as_ref() }
    }

    /// get object shape mut.
    /// could be None when object is being freed
    #[inline(always)]
    pub fn shape_mut(&mut self) -> Option<&mut JSShape> {
        unsafe { self.shape.as_mut() }
    }
}

#[cfg(debug_assertions)]
impl Drop for JSObject {
    fn drop(&mut self) {
        // println!("JSObject::drop({self:p})");

        debug_assert_ne!(self.class_id(), 0);
        debug_assert_eq!(self.header.ref_count, 0);
        debug_assert!(self.shape.is_null());
        debug_assert!(self.prop_values.is_empty());
        debug_assert!(self.first_weak_ref.is_null());

        // ManuallyDrop union
        unsafe {
            match self.class_id() {
                JS_CLASS_BYTECODE_FUNCTION => {
                    ManuallyDrop::drop(&mut self.u.func);
                }
                JS_CLASS_ARRAY
                | JS_CLASS_ARGUMENTS
                | JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                    ManuallyDrop::drop(&mut self.u.array);
                }

                _ => {}
            }
        }
    }
}

impl JSValue {
    pub fn is_function(&self, ctx: &JSContext) -> bool {
        if JS_VALUE_GET_TAG(*self) != JS_TAG_OBJECT {
            return false;
        }

        let p = JS_VALUE_GET_OBJ(*self);
        match unsafe { (*p).class_id() } {
            JS_CLASS_BYTECODE_FUNCTION => true,
            JS_CLASS_PROXY => {
                //(*p).u.proxy_data.is_func as BOOL
                unimplemented!()
            }
            cid => ctx
                .rt()
                .class_array
                .get(cid as usize)
                .and_then(|c| c.call)
                .is_some(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct JSObjectDetailed {
    // __gc_ref_count: i32, // corresponds to header.ref_count
    // __gc_mark: u8,       // corresponds to header.mark+gc_obj_type

    // corresponding header.dummy1 8 bits are used as:
    // extensible        1 bit
    // free_mark            1 bit, only used when freeing objects with cycles
    // is_exotic             1 bit, TRUE if object has exotic property handlers
    // fast_array  1 bit, TRUE if u.array is used for get/put (for JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS and typed arrays)
    // is_constructor 1 bit, TRUE if object is a constructor function
    // is_uncatchable_error: 1 bit, if TRUE, error is not catchable
    // tmp_mark:  1 bit, used in JS_WriteObjectRec()
    // is_HTMLDDA:  1 bit, specific annex B IsHtmlDDA behavior
    //
    // class_id: u16 (corresponds to header.dummy2) see JS_CLASS_x
    bitfield: u32,
}

impl JSObjectDetailed {
    const MASK_EXTENSIBLE: u32 = 0x8000_0000;
    const MASK_FREE_MARK: u32 = 0x4000_0000;
    const MASK_IS_EXOTIC: u32 = 0x2000_0000;
    const MASK_FAST_ARRAY: u32 = 0x1000_0000;
    const MASK_IS_CONSTRUCTOR: u32 = 0x0800_0000;
    const MASK_IS_UNCATCHABLE_ERROR: u32 = 0x0400_0000;
    const MASK_TMP_MARK: u32 = 0x0200_0000;
    const MASK_IS_HTMLDDA: u32 = 0x0100_0000;

    pub fn extensible(&self) -> bool {
        self.bitfield & Self::MASK_EXTENSIBLE != 0
    }
    pub fn set_extensible(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_EXTENSIBLE;
        } else {
            self.bitfield &= !Self::MASK_EXTENSIBLE;
        }
    }

    pub fn free_mark(&self) -> bool {
        self.bitfield & Self::MASK_FREE_MARK != 0
    }
    pub fn set_free_mark(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_FREE_MARK;
        } else {
            self.bitfield &= !Self::MASK_FREE_MARK;
        }
    }

    pub fn is_exotic(&self) -> bool {
        self.bitfield & Self::MASK_IS_EXOTIC != 0
    }
    pub fn set_exotic(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_EXOTIC;
        } else {
            self.bitfield &= !Self::MASK_IS_EXOTIC;
        }
    }

    pub fn fast_array(&self) -> bool {
        self.bitfield & Self::MASK_FAST_ARRAY != 0
    }
    pub fn set_fast_array(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_FAST_ARRAY
        } else {
            self.bitfield &= !Self::MASK_FAST_ARRAY;
        }
    }

    pub fn is_constructor(&self) -> bool {
        self.bitfield & Self::MASK_IS_CONSTRUCTOR != 0
    }
    pub fn set_constructor(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_CONSTRUCTOR;
        } else {
            self.bitfield &= !Self::MASK_IS_CONSTRUCTOR;
        }
    }

    pub fn is_uncatchable_error(&self) -> bool {
        self.bitfield & Self::MASK_IS_UNCATCHABLE_ERROR != 0
    }
    pub fn set_uncatchable_error(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_UNCATCHABLE_ERROR;
        } else {
            self.bitfield &= !Self::MASK_IS_UNCATCHABLE_ERROR;
        }
    }

    pub fn tmp_mark(&self) -> bool {
        self.bitfield & Self::MASK_TMP_MARK != 0
    }
    pub fn set_tmp_mark(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_TMP_MARK;
        } else {
            self.bitfield &= !Self::MASK_TMP_MARK;
        }
    }

    pub fn is_htmldda(&self) -> bool {
        self.bitfield & Self::MASK_IS_HTMLDDA != 0
    }
    pub fn set_htmldda(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_HTMLDDA;
        } else {
            self.bitfield &= !Self::MASK_IS_HTMLDDA;
        }
    }

    pub fn class_id(&self) -> JSClassID {
        self.bitfield & 0x0000_FFFF
    }
    pub fn set_class_id(&mut self, id: JSClassID) {
        self.bitfield = (self.bitfield & 0xFFFF_0000) | (id & 0x0000_FFFF);
    }
}

#[repr(C)]
pub union JSObjectUnion {
    pub opaque: *mut std::ffi::c_void,

    /// JS_CLASS_BOUND_FUNCTION
    pub bound_function: *mut JSBoundFunction,
    /// JS_CLASS_C_FUNCTION_DATA
    pub c_function_data_record: *mut JSCFunctionDataRecord,
    pub for_in_iterator: *mut JSForInIterator, /* JS_CLASS_FOR_IN_ITERATOR */
    pub array_buffer: *mut JSArrayBuffer, /* JS_CLASS_ARRAY_BUFFER, JS_CLASS_SHARED_ARRAY_BUFFER */
    pub typed_array: *mut JSTypedArray,   /* JS_CLASS_UINT8C_ARRAY..JS_CLASS_DATAVIEW */
    pub map_state: *mut JSMapState,       /* JS_CLASS_MAP..JS_CLASS_WEAKSET */
    pub map_iterator_data: *mut JSMapIteratorData, /* JS_CLASS_MAP_ITERATOR, JS_CLASS_SET_ITERATOR */
    pub array_iterator_data: *mut JSArrayIteratorData, /* JS_CLASS_ARRAY_ITERATOR, JS_CLASS_STRING_ITERATOR */
    pub regexp_string_iterator_data: *mut JSRegExpStringIteratorData, /* JS_CLASS_REGEXP_STRING_ITERATOR */
    pub generator_data: *mut JSGeneratorData,                         /* JS_CLASS_GENERATOR */
    pub proxy_data: *mut JSProxyData,                                 /* JS_CLASS_PROXY */
    pub promise_data: *mut JSPromiseData,                             /* JS_CLASS_PROMISE */
    pub promise_function_data: *mut JSPromiseFunctionData, /* JS_CLASS_PROMISE_RESOLVE_FUNCTION, JS_CLASS_PROMISE_REJECT_FUNCTION */
    pub async_function_data: *mut JSAsyncFunctionState, /* JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_CLASS_ASYNC_FUNCTION_REJECT */
    /// JS_CLASS_ASYNC_FROM_SYNC_ITERATOR
    pub async_from_sync_iterator_data: *mut JSAsyncFromSyncIteratorData,
    /// JS_CLASS_ASYNC_GENERATOR
    pub async_generator_data: *mut JSAsyncGeneratorData,
    ///  JS_CLASS_BYTECODE_FUNCTION: 12/24 bytes
    pub func: ManuallyDrop<JSObjectFunc>,
    /// JS_CLASS_C_FUNCTION: 12/20 bytes
    pub cfunc: JSObjectCFunc,
    /// JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS, JS_CLASS_UINT8C_ARRAY..JS_CLASS_FLOAT64_ARRAY
    pub array: ManuallyDrop<JSObjectArray>,
    /// JS_CLASS_REGEXP: 8/16 bytes  
    pub regexp: JSRegExp,
    /// for JS_SetObjectData(): 8/16/16 bytes  
    pub object_data: JSValue,
}

pub struct JSObjectArray {
    pub u1: JSObjectArrayU1,
    pub u: JSObjectArrayData,

    /// array length, 0..u32::MAX; 0 for a detached typed array
    pub count: u32,
}

#[derive(Clone, Copy)]
pub union JSObjectArrayU1 {
    pub size: u32,                      /* JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS */
    pub typed_array: *mut JSTypedArray, /* JS_CLASS_UINT8C_ARRAY..JS_CLASS_FLOAT64_ARRAY */
}

pub union JSObjectArrayData {
    /// JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS
    pub values: *mut JSValue,

    /// JS_CLASS_UINT8C_ARRAY..JS_CLASS_FLOAT64_ARRAY
    pub ptr: *mut std::ffi::c_void,

    pub int8_ptr: *mut i8,    /* JS_CLASS_INT8_ARRAY */
    pub uint8_ptr: *mut u8,   /* JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY */
    pub int16_ptr: *mut i16,  /* JS_CLASS_INT16_ARRAY */
    pub uint16_ptr: *mut u16, /* JS_CLASS_UINT16_ARRAY */
    pub int32_ptr: *mut i32,  /* JS_CLASS_INT32_ARRAY */
    pub uint32_ptr: *mut u32, /* JS_CLASS_UINT32_ARRAY */
    pub int64_ptr: *mut i64,  /* JS_CLASS_INT64_ARRAY */
    pub uint64_ptr: *mut u64, /* JS_CLASS_UINT64_ARRAY */
    pub float_ptr: *mut f32,  /* JS_CLASS_FLOAT32_ARRAY */
    pub double_ptr: *mut f64, /* JS_CLASS_FLOAT64_ARRAY */
}

impl JSObject {
    #[inline(always)]
    pub fn class_id(&self) -> JSClassID {
        self.detailed.class_id()
    }
}

pub const JS_PROP_INITIAL_SIZE: u32 = 2;
pub const JS_ARRAY_INITIAL_SIZE: u32 = 2;

#[cfg(target_pointer_width = "64")]
pub const JS_PROP_INITIAL_HASH_SIZE: usize = 8;
#[cfg(not(target_pointer_width = "64"))]
pub const JS_PROP_INITIAL_HASH_SIZE: usize = 4;

pub unsafe fn JS_NewObjectProtoClass(
    ctx: *mut JSContext,
    proto_val: JSValueConst,
    class_id: JSClassID,
) -> JSValue {
    let proto = get_proto_obj(proto_val);
    let sh: *mut JSShape = match find_hashed_shape_proto((*ctx).rt_mut(), proto) {
        Some(sh) => js_dup_shape(sh),
        None => {
            let new_sh = js_new_shape(ctx, proto);
            if new_sh.is_null() {
                return JSValue::EXCEPTION;
            } else {
                new_sh
            }
        }
    };

    JS_NewObjectFromShape(ctx, sh, class_id)
}

/// Create class instance
pub unsafe fn JS_NewObjectClass(ctx: *mut JSContext, class_id: JSClassID) -> JSValue {
    JS_NewObjectProtoClass(ctx, (*ctx).class_proto[class_id as usize], class_id)
}

/// Create `Object` value using `proto` as prototype
pub unsafe fn JS_NewObjectProto(ctx: *mut JSContext, proto: JSValueConst) -> JSValue {
    JS_NewObjectProtoClass(ctx, proto, JS_CLASS_OBJECT)
}

/// Create `Object` value
pub unsafe fn JS_NewObject(ctx: *mut JSContext) -> JSValue {
    /* inline JS_NewObjectClass(ctx, JS_CLASS_OBJECT); */
    JS_NewObjectProtoClass(
        ctx,
        (*ctx).class_proto[JS_CLASS_OBJECT as usize],
        JS_CLASS_OBJECT,
    )
}

pub unsafe fn JS_SetPrototypeInternal(
    ctx: *mut JSContext,
    obj: JSValueConst,
    proto_val: JSValueConst,
    throw_flag: bool,
) -> Result<bool, i32> {
    if throw_flag {
        if JS_VALUE_GET_TAG(obj) == JS_TAG_NULL || JS_VALUE_GET_TAG(obj) == JS_TAG_UNDEFINED {
            (*ctx).JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }
    } else {
        if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
            (*ctx).JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }
    }

    let p = JS_VALUE_GET_OBJ(obj);
    let proto = if JS_VALUE_GET_TAG(proto_val) != JS_TAG_OBJECT {
        if JS_VALUE_GET_TAG(proto_val) != JS_TAG_NULL {
            (*ctx).JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }
        std::ptr::null_mut()
    } else {
        JS_VALUE_GET_OBJ(proto_val)
    };

    if throw_flag && JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        return Ok(true);
    }
    if unlikely((*p).class_id() == JS_CLASS_PROXY) {
        return js_proxy_setPrototypeOf(ctx, obj, proto_val, throw_flag);
    }

    if (*p).shape().unwrap().proto == proto {
        return Ok(true);
    }

    if !(*p).detailed.extensible() {
        if throw_flag {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"object is not extensible");
            return Err(-1);
        } else {
            return Ok(false);
        }
    }
    if !proto.is_null() {
        // check if there is a cycle
        let mut p1 = proto;

        loop {
            if p1 == p {
                if throw_flag {
                    ctx.as_mut()
                        .unwrap()
                        .JS_ThrowTypeError(c"circular prototype chain");
                    return Err(-1);
                } else {
                    return Ok(false);
                }
            }

            // Note: for Proxy objects, proto is NULL
            p1 = (*p1).shape().unwrap().proto;
            if p1.is_null() {
                break;
            }
        }

        proto_val.dup();
    }

    if p.as_mut()
        .unwrap()
        .js_shape_prepare_update(ctx, std::ptr::null_mut())
        != 0
    {
        return Err(-1);
    }

    let sh = (*p).shape_mut().unwrap();
    if !sh.proto.is_null() {
        JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, sh.proto.cast()));
    }
    sh.proto = proto;

    Ok(true)
}

pub unsafe fn get_proto_obj(proto_val: JSValueConst) -> *mut JSObject {
    if JS_VALUE_GET_TAG(proto_val) != JS_TAG_OBJECT {
        std::ptr::null_mut()
    } else {
        JS_VALUE_GET_OBJ(proto_val)
    }
}

pub unsafe fn free_object(rt: *mut JSRuntime, p: *mut JSObject) {
    /* used to tell the object is invalid when freeing cycles */
    (*p).detailed.set_free_mark(true);

    let sh = (*p).shape_mut().unwrap(); // shape can't be null now

    // free all prop values
    for (prs, mut v) in sh.props.values().zip((*p).prop_values.drain(..)) {
        free_property(rt, &mut v, prs.flags());
    }

    // as an optimization we destroy the shape immediately without putting it in gc_zero_ref_count_list
    js_free_shape(rt, sh);

    // set shape to null means object is freed
    (*p).shape = std::ptr::null_mut();

    if !(*p).first_weak_ref.is_null() {
        reset_weak_ref(rt, p);
    }

    // call class finalizer
    if let Some(cb) = (*rt).class_array[(*p).class_id() as usize].finalizer {
        cb(rt, JS_MKPTR!(JSTag::Object, p.cast()));
    }

    (*(*p).u.func).home_object = std::ptr::null_mut();
    (*p).u.opaque = std::ptr::null_mut();

    remove_gc_object(&mut (*p).header);

    if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES && (*p).header.ref_count != 0 {
        list_add_tail(&mut (*p).header.link, &mut (*rt).gc_zero_ref_count_list);
    } else {
        drop(Box::from_raw(p));
    }
}

pub unsafe fn reset_weak_ref(rt: *mut JSRuntime, p: *mut JSObject) {
    // first pass to remove the records from the WeakMap/WeakSet lists
    let mut mr = (*p).first_weak_ref;
    while mr != std::ptr::null_mut() {
        let s = (*mr).map;
        assert!((*s).is_weak);
        assert!(!(*mr).empty); // no iterator on WeakMap/WeakSet
                               // C2RS: Use addr_of_mut! to get raw pointers to struct fields
        list_del(std::ptr::addr_of_mut!((*mr).hash_link));
        list_del(std::ptr::addr_of_mut!((*mr).link));
        mr = (*mr).next_weak_ref;
    }

    // second pass to free the values
    mr = (*p).first_weak_ref;
    while mr != std::ptr::null_mut() {
        let mr_next = (*mr).next_weak_ref;
        JS_FreeValueRT(rt, (*mr).value);
        js_free_rt(rt, mr.cast());
        mr = mr_next;
    }

    (*p).first_weak_ref = std::ptr::null_mut(); // fail safe
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSAutoInitIDEnum {
    JS_AUTOINIT_ID_PROTOTYPE,
    JS_AUTOINIT_ID_MODULE_NS,
    JS_AUTOINIT_ID_PROP,
}

#[inline(always)]
pub unsafe fn js_autoinit_get_id(pr: *const JSProperty) -> i32 {
    ((*pr).u.init.realm_and_id & 0b11) as _
}

#[inline(always)]
pub unsafe fn js_autoinit_get_realm(pr: *const JSProperty) -> *mut JSContext {
    ((*pr).u.init.realm_and_id & !0b11) as *mut JSContext
}

#[inline(always)]
pub unsafe fn js_autoinit_free(rt: *const JSRuntime, pr: *const JSProperty) {
    JS_FreeContext(js_autoinit_get_realm(pr));
}

#[deprecated(note = "use JSValue::is_function() instead")]
pub unsafe fn JS_IsFunction(ctx: *mut JSContext, val: JSValueConst) -> bool {
    val.is_function(ctx.as_ref().unwrap())
}

/// 检查给定的 JSValue是否是一个特定的 C 函数。不仅检查函数指针是否相同，还会检查 magic 值是否匹配
#[inline(always)]
pub unsafe fn JS_IsCFunction(
    ctx: *const JSContext,
    val: JSValueConst,
    func: JSCFunction,
    magic: i32,
) -> bool {
    if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
        return false;
    }
    let p = JS_VALUE_GET_OBJ(val);
    if (*p).class_id() == JS_CLASS_C_FUNCTION {
        (*p).u.cfunc.c_function.generic == func && (*p).u.cfunc.magic == magic as i16
    } else {
        false
    }
}

#[inline(always)]
pub unsafe fn JS_IsConstructor(ctx: *const JSContext, val: JSValueConst) -> bool {
    if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
        return false;
    }
    let p = JS_VALUE_GET_OBJ(val);
    (*p).detailed.is_constructor()
}

#[inline(always)]
pub unsafe fn JS_SetConstructorBit(
    ctx: *const JSContext,
    func_obj: JSValueConst,
    val: bool,
) -> bool {
    if JS_VALUE_GET_TAG(func_obj) != JS_TAG_OBJECT {
        return false;
    }
    let p = JS_VALUE_GET_OBJ(func_obj);
    (*p).detailed.set_constructor(val);
    true
}

#[inline(always)]
pub unsafe fn JS_IsError(ctx: *const JSContext, val: JSValueConst) -> bool {
    if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
        return false;
    }
    let p = JS_VALUE_GET_OBJ(val);
    (*p).class_id() == JS_CLASS_ERROR
}

pub unsafe fn JS_IsInstanceOf(
    ctx: *mut JSContext,
    val: JSValueConst,
    obj: JSValueConst,
) -> Result<bool, i32> {
    if !JS_IsObject(obj) {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"invalid 'instanceof' right operand");
        return Err(-1);
    }

    let method = JS_GetProperty(ctx, obj, JS_ATOM_Symbol_hasInstance);
    if JS_IsException(method) {
        return Err(-1);
    }

    if !JS_IsNull(method) && !JS_IsUndefined(method) {
        let mut val = val;
        let ret = JS_CallFree(ctx, method, obj, 1, &mut val);
        return JS_ToBoolFree(ctx, ret);
    }

    // legacy case
    if !JS_IsFunction(ctx, obj) {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"invalid 'instanceof' right operand");
        return Err(-1);
    }

    return JS_OrdinaryIsInstanceOf(ctx, val, obj);
}

pub unsafe fn JS_OrdinaryIsInstanceOf(
    ctx: *mut JSContext,
    val: JSValueConst,
    obj: JSValueConst,
) -> Result<bool, i32> {
    if !JS_IsFunction(ctx, obj) {
        return Ok(false);
    }

    let p = JS_VALUE_GET_OBJ(obj);
    if (*p).class_id() == JS_CLASS_BOUND_FUNCTION {
        let s = (*p).u.bound_function;
        return JS_IsInstanceOf(ctx, val, (*s).func_obj);
    }

    // Only explicitly boxed values are instances of constructors
    if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
        return Ok(false);
    }
    let obj_proto = JS_GetProperty(ctx, obj, JS_ATOM_prototype);
    if JS_VALUE_GET_TAG(obj_proto) != JS_TAG_OBJECT {
        if !obj_proto.is_exception() {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"operand 'prototype' property is not an object");
        }
        return Err(-1);
    }

    let proto = JS_VALUE_GET_OBJ(obj_proto);
    let mut p = JS_VALUE_GET_OBJ(val);

    let ret: Result<bool, i32> = loop {
        let proto1 = (*p).shape().unwrap().proto;
        if proto1.is_null() {
            let r = if unlikely((*p).class_id() == JS_CLASS_PROXY) {
                // slow case if proxy in the prototype chain
                let mut obj1 = JS_MKPTR!(JSTag::Object, p.cast()).dup();
                loop {
                    obj1 = JS_GetPrototypeFree(ctx, obj1);
                    if obj1.is_exception() {
                        break Err(-1);
                    } else if obj1.is_null() {
                        break Ok(false);
                    } else if proto == JS_VALUE_GET_OBJ(obj1) {
                        JS_FreeValue(ctx, obj1);
                        break Ok(true);
                    }

                    // must check for timeout to avoid infinite loop
                    if js_poll_interrupts(ctx) != 0 {
                        JS_FreeValue(ctx, obj1);
                        break Err(-1);
                    }
                }
            } else {
                Ok(false)
            };
            break r;
        } else {
            p = proto1;
            if proto == p {
                break Ok(true);
            }
        }
    };

    JS_FreeValue(ctx, obj_proto);
    ret
}

pub unsafe fn JS_GetOpaque(obj: JSValueConst, class_id: JSClassID) -> *mut c_void {
    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        return std::ptr::null_mut();
    }
    let p = JS_VALUE_GET_OBJ(obj);
    if (*p).class_id() != class_id {
        return std::ptr::null_mut();
    }
    (*p).u.opaque
}

pub unsafe fn JS_GetOpaque2(
    ctx: *mut JSContext,
    obj: JSValueConst,
    class_id: JSClassID,
) -> *mut c_void {
    let p = JS_GetOpaque(obj, class_id);
    if unlikely(!p.is_null()) {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeErrorInvalidClass(class_id);
    }
    p
}

pub unsafe fn JS_SetOpaque(obj: JSValue, opaque: *mut c_void) {
    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(obj);
        (*p).u.opaque = opaque;
    }
}

pub unsafe fn JS_ToObject(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    macro_rules! goto_set_value {
        ($o:expr) => {
            if !JS_IsException($o) {
                JS_SetObjectData(ctx, $o, JS_DupValue(ctx, val));
            }
            return $o;
        };
    }

    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::Null | JSTag::Undefined => {
            return ctx
                .as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"cannot convert to object");
        }
        JSTag::Object => {
            return val.dup();
        }
        JSTag::Exception => {
            return val;
        }
        JSTag::BigInt => {
            // obj = JS_NewObjectClass(ctx, JS_CLASS_BIG_INT);
            // // 跳转到set_value逻辑
            // if !JS_IsException(obj) {
            //     JS_SetObjectData(ctx, obj, JS_DupValue(ctx, val));
            // }
            // return obj;
            unimplemented!()
        }
        JSTag::Int | JSTag::Float64 => {
            let obj = JS_NewObjectClass(ctx, JS_CLASS_NUMBER);
            goto_set_value!(obj);
        }
        JSTag::String => {
            let obj = JS_NewObjectClass(ctx, JS_CLASS_STRING);
            goto_set_value!(obj);
        }
        JSTag::Symbol => {
            let obj = JS_NewObjectClass(ctx, JS_CLASS_SYMBOL);
            goto_set_value!(obj);
        }
        _ => {
            return ctx
                .as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"cannot convert to object");
        }
    }
}

pub unsafe fn JS_ToObjectFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    let obj = JS_ToObject(ctx, val);
    JS_FreeValue(ctx, val);
    return obj;
}

pub unsafe fn JS_SetObjectData(ctx: *mut JSContext, obj: JSValueConst, val: JSValue) -> i32 {
    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(obj);
        match (*p).class_id() {
            JS_CLASS_NUMBER | JS_CLASS_STRING | JS_CLASS_BOOLEAN | JS_CLASS_SYMBOL
            | JS_CLASS_DATE | JS_CLASS_BIG_INT => {
                JS_FreeValue(ctx, (*p).u.object_data);
                (*p).u.object_data = val;
                return 0;
            }
            _ => {}
        }
    }

    JS_FreeValue(ctx, val);

    if !JS_IsException(obj) {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"invalid object type");
    }

    -1
}

// Return an Object, JS_NULL or JS_EXCEPTION in case of Proxy object.
pub unsafe fn JS_GetPrototype(ctx: *mut JSContext, obj: JSValueConst) -> JSValue {
    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let obj_ptr = JS_VALUE_GET_OBJ(obj);
        if unlikely((*obj_ptr).class_id() == JS_CLASS_PROXY) {
            js_proxy_getPrototypeOf(ctx, obj)
        } else {
            let proto = (*obj_ptr).shape().unwrap().proto;
            if proto.is_null() {
                JSValue::NULL
            } else {
                JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, proto.cast()))
            }
        }
    } else {
        JS_DupValue(ctx, JS_GetPrototypePrimitive(ctx, obj))
    }
}

pub unsafe fn JS_GetPrototypeFree(ctx: *mut JSContext, obj: JSValue) -> JSValue {
    let ret = JS_GetPrototype(ctx, obj);
    JS_FreeValue(ctx, obj);
    return ret;
}

pub unsafe fn JS_ObjectDefineProperties(
    ctx: *mut JSContext,
    obj: JSValueConst,
    properties: JSValueConst,
) -> Result<(), i32> {
    if !JS_IsObject(obj) {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let mut props = JS_ToObject(ctx, properties);
    if JS_IsException(props) {
        return Err(-1);
    }

    let p = JS_VALUE_GET_OBJ(props);
    let mut desc = JS_UNDEFINED;
    let mut atoms: *mut JSPropertyEnum = std::ptr::null_mut();
    let mut len: u32 = 0;

    macro_rules! exit {
        ( $ret:expr) => {
            js_free_prop_enum(ctx, atoms, len as usize);
            JS_FreeValue(ctx, props);
            JS_FreeValue(ctx, desc);
            return $ret;
        };
    }

    if JS_GetOwnPropertyNamesInternal(
        ctx,
        &mut atoms,
        &mut len,
        p,
        JS_GPN_ENUM_ONLY | JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
    )
    .is_err()
    {
        exit!(Err(-1));
    }

    for i in 0..len {
        JS_FreeValue(ctx, desc);

        desc = JS_GetProperty(ctx, props, (*atoms.offset(i as isize)).atom);
        if JS_IsException(desc) {
            exit!(Err(-1));
        }

        if JS_DefinePropertyDesc(
            ctx,
            obj,
            (*atoms.offset(i as isize)).atom,
            desc,
            JS_PROP_THROW,
        )
        .is_err()
        {
            exit!(Err(-1));
        }
    }

    exit!(Ok(()));
}

// return TRUE if 'obj' has a non empty 'name' string
pub unsafe fn js_object_has_name(ctx: *const JSContext, obj: JSValueConst) -> bool {
    let mut pr: *mut JSProperty = std::ptr::null_mut();
    let prs = find_own_property(&mut pr, JS_VALUE_GET_OBJ(obj), JS_ATOM_name);
    if prs.is_null() {
        return false;
    }
    if (*prs).flags() & JS_PROP_TMASK != JS_PROP_NORMAL {
        return true;
    }

    let val = (*pr).u.value;
    if JS_VALUE_GET_TAG(val) != JS_TAG_STRING {
        return true;
    }
    let p = JS_VALUE_GET_STRING(val);
    return (*p).len() != 0;
}

pub unsafe fn js_get_function_name(ctx: *mut JSContext, name: JSAtom) -> JSValue {
    let mut name_str = name.to_value(ctx.as_mut().unwrap(), true);
    if JS_AtomSymbolHasDescription(ctx, name) {
        name_str = JS_ConcatString3(ctx, c"[", name_str, c"]");
    }
    return name_str;
}

pub unsafe fn JS_DefineObjectName(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSAtom,
    flags: u32,
) -> i32 {
    if name != JS_ATOM_NULL
        && JS_IsObject(obj)
        && !js_object_has_name(ctx, obj)
        && JS_DefinePropertyValue(
            ctx,
            obj,
            JS_ATOM_name,
            name.to_value(ctx.as_mut().unwrap(), true),
            flags,
        ) < 0
    {
        return -1;
    }
    return 0;
}

pub unsafe fn JS_DefineObjectNameComputed(
    ctx: *mut JSContext,
    obj: JSValueConst,
    str: JSValueConst,
    flags: u32,
) -> i32 {
    if JS_IsObject(obj) && !js_object_has_name(ctx, obj) {
        let prop = JS_ValueToAtom(ctx, str);
        if prop == JS_ATOM_NULL {
            return -1;
        }

        let name_str = js_get_function_name(ctx, prop);
        JS_FreeAtom(ctx, prop);
        if name_str.is_exception() {
            return -1;
        }
        if JS_DefinePropertyValue(ctx, obj, JS_ATOM_name, name_str, flags) < 0 {
            return -1;
        }
    }

    0
}

// Note: all the fields are already sealed except length
pub unsafe fn seal_template_obj(ctx: *mut JSContext, obj: JSValueConst) -> i32 {
    let p = JS_VALUE_GET_OBJ(obj);
    let mut prs = find_own_property1(p, JS_ATOM_length);
    if !prs.is_null() {
        if p.as_mut().unwrap().js_update_property_flags(
            ctx,
            std::ptr::addr_of_mut!(prs),
            (*prs).flags() & !(JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE),
        ) != 0
        {
            return -1;
        }
    }
    (*p).detailed.set_extensible(false);

    return 0;
}

// return Err(-1) if exception (Proxy object only) or TRUE/FALSE
pub unsafe fn JS_PreventExtensions(ctx: *mut JSContext, obj: JSValueConst) -> Result<bool, i32> {
    if unlikely(JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT) {
        return Ok(false);
    }
    let p = JS_VALUE_GET_OBJ(obj);
    if unlikely((*p).class_id() == JS_CLASS_PROXY) {
        return js_proxy_preventExtensions(ctx, obj);
    }
    (*p).detailed.set_extensible(false);
    return Ok(true);
}

pub unsafe fn js_object_seal(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    freeze_flag: i32,
) -> JSValue {
    let obj = argv.offset(0).read();
    if !JS_IsObject(obj) {
        return JS_DupValue(ctx, obj);
    }

    match JS_PreventExtensions(ctx, obj) {
        Ok(false) => {
            return (*ctx).JS_ThrowTypeError(c"proxy preventExtensions handler returned false");
        }
        Err(_) => {
            return JSValue::EXCEPTION;
        }
        _ => {}
    }

    let p = JS_VALUE_GET_OBJ(obj);
    let flags = JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK;

    let mut props: *mut JSPropertyEnum = std::mem::zeroed();
    let mut len: u32 = 0;
    if JS_GetOwnPropertyNamesInternal(ctx, &mut props, &mut len, p, flags).is_err() {
        return JSValue::EXCEPTION;
    }

    let mut desc_flags: u32;
    for i in 0..len {
        let prop = props.offset(i as isize).read().atom;

        desc_flags = JS_PROP_THROW | JS_PROP_HAS_CONFIGURABLE;
        if freeze_flag != 0 {
            let mut desc: JSPropertyDescriptor = std::mem::zeroed();
            let res = JS_GetOwnPropertyInternal(ctx, &mut desc, p, prop);
            if res < 0 {
                // C2RS: goto exception; 转换为 rust 的异常处理逻辑
                js_free_prop_enum(ctx, props, len as _);
                return JSValue::EXCEPTION;
            }
            if res != 0 {
                if desc.flags & JS_PROP_WRITABLE != 0 {
                    desc_flags |= JS_PROP_HAS_WRITABLE;
                }
                js_free_desc(ctx, &mut desc);
            }
        }
        if JS_DefineProperty(
            ctx,
            obj,
            prop,
            JS_UNDEFINED,
            JS_UNDEFINED,
            JS_UNDEFINED,
            desc_flags,
        ) < 0
        {
            js_free_prop_enum(ctx, props, len as _);
            return JSValue::EXCEPTION;
        }
    }
    js_free_prop_enum(ctx, props, len as _);

    obj.dup()
}

// return a boolean telling if the brand of the home object of 'func'
// is present on 'obj'
pub unsafe fn JS_CheckBrand(
    ctx: *mut JSContext,
    obj: JSValueConst,
    func: JSValueConst,
) -> Result<bool, i32> {
    // get the home object of 'func'
    if unlikely(JS_VALUE_GET_TAG(func) != JS_TAG_OBJECT) {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let p1 = JS_VALUE_GET_OBJ(func);
    if !js_class_has_bytecode((*p1).class_id()) {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let home_obj = (*p1).u.func.home_object;
    if home_obj.is_null() {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let mut pr = std::ptr::null_mut::<JSProperty>();
    let prs = find_own_property(&mut pr, home_obj, JS_ATOM_Private_brand);
    if prs.is_null() {
        (*ctx).JS_ThrowTypeError(c"expecting <brand> private field");
        return Err(-1);
    }

    let brand = (*pr).u.value;
    // safety check
    if unlikely(JS_VALUE_GET_TAG(brand) != JS_TAG_SYMBOL) {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    // get the brand array of 'obj'
    if unlikely(JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT) {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let p = JS_VALUE_GET_OBJ(obj);
    let prs = find_own_property(&mut pr, p, js_symbol_to_atom(ctx, brand));

    Ok(!prs.is_null())
}

// add a private brand field to 'home_obj' if not already present and
// if obj is != null add a private brand to it
pub unsafe fn JS_AddBrand(ctx: *mut JSContext, obj: JSValueConst, home_obj: JSValueConst) -> i32 {
    if unlikely(JS_VALUE_GET_TAG(home_obj) != JS_TAG_OBJECT) {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return -1;
    }

    let p = JS_VALUE_GET_OBJ(home_obj);
    let mut pr = std::ptr::null_mut::<JSProperty>();
    let mut prs = find_own_property(&mut pr, p, JS_ATOM_Private_brand);

    let brand = if prs.is_null() {
        // if the brand is not present, add it
        let br = JS_NewSymbolFromAtom(ctx, JS_ATOM_brand, JSAtomTypeHint::Private);
        if JS_IsException(br) {
            return -1;
        }
        pr = add_property(ctx, p, JS_ATOM_Private_brand, JS_PROP_C_W_E);
        if pr.is_null() {
            JS_FreeValue(ctx, br);
            return -1;
        }
        (*pr).u.value = JS_DupValue(ctx, br);
        br
    } else {
        JS_DupValue(ctx, (*pr).u.value)
    };
    let brand_atom = js_symbol_to_atom(ctx, brand);

    if JS_IsObject(obj) {
        let p1 = JS_VALUE_GET_OBJ(obj);
        prs = find_own_property(&mut pr, p1, brand_atom);
        if unlikely(!prs.is_null()) {
            JS_FreeAtom(ctx, brand_atom);
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"private method is already present");
            return -1;
        }

        pr = add_property(ctx, p1, brand_atom, JS_PROP_C_W_E);
        JS_FreeAtom(ctx, brand_atom);
        if pr.is_null() {
            return -1;
        }
        (*pr).u.value = JS_UNDEFINED;
    } else {
        JS_FreeAtom(ctx, brand_atom);
    }

    0
}

// Private fields can be added even on non extensible objects or Proxies
pub unsafe fn JS_DefinePrivateField(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSValueConst,
    val: JSValue,
) -> i32 {
    if unlikely(JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT) {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        JS_FreeValue(ctx, val);
        return -1;
    }

    // safety check
    if unlikely(JS_VALUE_GET_TAG(name) != JS_TAG_SYMBOL) {
        (*ctx).JS_ThrowTypeErrorNotASymbol();
        JS_FreeValue(ctx, val);
        return -1;
    }

    let prop = js_symbol_to_atom(ctx, name);
    let p = JS_VALUE_GET_OBJ(obj);
    let mut pr = std::ptr::null_mut::<JSProperty>();
    let prs = find_own_property(&mut pr, p, prop);

    if !prs.is_null() {
        (*ctx).JS_ThrowTypeError(CStr::from_ptr(
            format!(
                "private class field '{}' already exists\0",
                prop.to_str((*ctx).rt())
            )
            .as_ptr()
            .cast(),
        ));

        JS_FreeValue(ctx, val);
        return -1;
    }

    pr = add_property(ctx, p, prop, JS_PROP_C_W_E);
    if unlikely(pr.is_null()) {
        JS_FreeValue(ctx, val);
        return -1;
    }
    (*pr).u.value = val;
    0
}

pub unsafe fn JS_GetPrivateField(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSValueConst,
) -> JSValue {
    if unlikely(JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT) {
        return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
    }
    // safety check
    if unlikely(JS_VALUE_GET_TAG(name) != JS_TAG_SYMBOL) {
        return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotASymbol();
    }

    let prop = js_symbol_to_atom(ctx, name as JSValue);
    let p = JS_VALUE_GET_OBJ(obj);
    let mut pr = std::ptr::null_mut::<JSProperty>();
    let prs = find_own_property(&mut pr, p, prop);

    if prs.is_null() {
        JS_ThrowTypeErrorPrivateNotFound(ctx, prop);
        return JSValue::EXCEPTION;
    }

    JS_DupValue(ctx, (*pr).u.value)
}

pub unsafe fn JS_SetPrivateField(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSValueConst,
    val: JSValue,
) -> i32 {
    if unlikely(JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT) {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        JS_FreeValue(ctx, val);
        return -1;
    }

    // ensure name must be a Symbol
    if JS_VALUE_GET_TAG(name) != JS_TAG_SYMBOL {
        (*ctx).JS_ThrowTypeErrorNotASymbol();
        JS_FreeValue(ctx, val);
        return -1;
    }

    let prop = js_symbol_to_atom(ctx, name as JSValue);
    let p = JS_VALUE_GET_OBJ(obj);
    let mut pr = std::ptr::null_mut::<JSProperty>();
    let prs = find_own_property(&mut pr, p, prop);

    if prs.is_null() {
        JS_ThrowTypeErrorPrivateNotFound(ctx, prop);
        JS_FreeValue(ctx, val);
        return -1;
    }

    set_value(ctx, &mut (*pr).u.value, val);
    0
}

pub unsafe fn JS_SpeciesConstructor(
    ctx: *mut JSContext,
    obj: JSValueConst,
    defaultConstructor: JSValueConst,
) -> JSValue {
    if !JS_IsObject(obj) {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let ctor = JS_GetProperty(ctx, obj, JS_ATOM_constructor);
    if JS_IsException(ctor) {
        return ctor;
    }
    if JS_IsUndefined(ctor) {
        return defaultConstructor.dup();
    }
    if !JS_IsObject(ctor) {
        JS_FreeValue(ctx, ctor);
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let species = JS_GetProperty(ctx, ctor, JS_ATOM_Symbol_species);
    JS_FreeValue(ctx, ctor);
    if JS_IsException(species) {
        return species;
    }

    if JS_IsUndefined(species) || JS_IsNull(species) {
        return defaultConstructor.dup();
    }

    if !JS_IsConstructor(ctx, species) {
        JS_FreeValue(ctx, species);
        return (*ctx).JS_ThrowTypeError(c"not a constructor");
    }

    species
}
