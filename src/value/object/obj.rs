use std::{ffi::c_void, mem::ManuallyDrop, ptr::NonNull};

use crate::{
    array::JSObjectArray,
    array_buffer::JSArrayBuffer,
    atom::{consts::*, JSAtom},
    callable::JSCFunctionDataRecord,
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::{
        js_poll_interrupts, JSAsyncFunctionState, JSBoundFunction, JSCFunction, JSObjectCFunc,
        JSObjectFunc, JS_CallFree,
    },
    gc::JSGCObjectHeader,
    generator::{JSAsyncGeneratorData, JSGeneratorData},
    iterator::{
        JSArrayIteratorData, JSAsyncFromSyncIteratorData, JSForInIterator, JSMapIteratorData,
    },
    map::{JSMapRecord, JSMapState},
    promise::{JSPromiseData, JSPromiseFunctionData},
    proxy::{
        js_proxy_getPrototypeOf, js_proxy_preventExtensions, js_proxy_setPrototypeOf, JSProxyData,
    },
    regexp::{JSRegExp, JSRegExpStringIteratorData},
    typed_array::JSTypedArray,
    types::unlikely,
    value::{JS_TAG_FUNCTION_BYTECODE, JS_VALUE_GET_PTR},
    JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN, JS_CLASS_BOUND_FUNCTION, JS_CLASS_C_FUNCTION,
    JS_CLASS_DATE, JS_CLASS_ERROR, JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_PROXY,
    JS_CLASS_STRING, JS_CLASS_SYMBOL, JS_MKPTR,
};

use super::{
    super::{
        JSTag, JSValue, JSValueConst, JS_AtomSymbolHasDescription, JS_ConcatString3, JS_FreeValue,
        JS_GetPrototypePrimitive, JS_IsException, JS_ToBoolFree, JS_TAG_NULL, JS_TAG_OBJECT,
        JS_TAG_STRING, JS_TAG_UNDEFINED, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    find_hashed_shape_proto, js_dup_shape, js_free_desc, js_new_shape, JSProperty,
    JSPropertyDescriptor, JSShape, JSShapeProperty, JS_GetOwnPropertyInternal, JS_IsExtensible,
    JS_GPN_STRING_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE,
    JS_PROP_HAS_WRITABLE, JS_PROP_NORMAL, JS_PROP_THROW, JS_PROP_TMASK, JS_PROP_WRITABLE,
};

/// Bit flags and metadata for JavaScript objects
/// Encodes object state and class information in a compact bitfield
#[derive(Debug, Clone, Copy, Default)]
pub struct JSObjectFlags {
    /// Bitfield storing object state flags and class ID:
    /// - Bits 31-24: Object state flags
    ///   [31] extensible: Can add new properties
    ///   [30] free_mark: Object destruction marker
    ///   [29] is_exotic: Special property handling
    ///   [28] fast_array: Uses optimized array storage
    ///   [27] constructor: Object is a constructor
    ///   [26] uncatchable_error: Error cannot be caught
    ///   [25] tmp_mark: Temporary serialization marker
    ///   [24] HTMLDDA: Special browser compatibility behavior
    /// - Bits 15-0: Class ID (JS_CLASS_* constant)
    bitfield: u32,
}

impl JSObjectFlags {
    const MASK_EXTENSIBLE: u32 = 0x8000_0000;
    const MASK_FREE_MARK: u32 = 0x4000_0000;
    const MASK_IS_EXOTIC: u32 = 0x2000_0000;
    const MASK_FAST_ARRAY: u32 = 0x1000_0000;
    const MASK_IS_CONSTRUCTOR: u32 = 0x0800_0000;
    const MASK_IS_UNCATCHABLE_ERROR: u32 = 0x0400_0000;
    const MASK_TMP_MARK: u32 = 0x0200_0000;
    const MASK_IS_HTMLDDA: u32 = 0x0100_0000;

    pub const fn extensible(&self) -> bool {
        self.bitfield & Self::MASK_EXTENSIBLE != 0
    }
    pub fn set_extensible(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_EXTENSIBLE;
        } else {
            self.bitfield &= !Self::MASK_EXTENSIBLE;
        }
    }

    pub const fn free_mark(&self) -> bool {
        self.bitfield & Self::MASK_FREE_MARK != 0
    }
    pub fn set_free_mark(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_FREE_MARK;
        } else {
            self.bitfield &= !Self::MASK_FREE_MARK;
        }
    }

    pub const fn is_exotic(&self) -> bool {
        self.bitfield & Self::MASK_IS_EXOTIC != 0
    }
    pub fn set_exotic(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_EXOTIC;
        } else {
            self.bitfield &= !Self::MASK_IS_EXOTIC;
        }
    }

    pub const fn fast_array(&self) -> bool {
        self.bitfield & Self::MASK_FAST_ARRAY != 0
    }
    pub fn set_fast_array(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_FAST_ARRAY
        } else {
            self.bitfield &= !Self::MASK_FAST_ARRAY;
        }
    }

    pub const fn is_constructor(&self) -> bool {
        self.bitfield & Self::MASK_IS_CONSTRUCTOR != 0
    }
    pub fn set_constructor(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_CONSTRUCTOR;
        } else {
            self.bitfield &= !Self::MASK_IS_CONSTRUCTOR;
        }
    }

    pub const fn is_uncatchable_error(&self) -> bool {
        self.bitfield & Self::MASK_IS_UNCATCHABLE_ERROR != 0
    }
    pub fn set_uncatchable_error(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_UNCATCHABLE_ERROR;
        } else {
            self.bitfield &= !Self::MASK_IS_UNCATCHABLE_ERROR;
        }
    }

    pub const fn tmp_mark(&self) -> bool {
        self.bitfield & Self::MASK_TMP_MARK != 0
    }
    pub fn set_tmp_mark(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_TMP_MARK;
        } else {
            self.bitfield &= !Self::MASK_TMP_MARK;
        }
    }

    pub const fn is_htmldda(&self) -> bool {
        self.bitfield & Self::MASK_IS_HTMLDDA != 0
    }
    pub fn set_htmldda(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_HTMLDDA;
        } else {
            self.bitfield &= !Self::MASK_IS_HTMLDDA;
        }
    }

    pub const fn class_id(&self) -> JSClassID {
        self.bitfield & 0x0000_FFFF
    }
    pub fn set_class_id(&mut self, id: JSClassID) {
        self.bitfield = (self.bitfield & 0xFFFF_0000) | (id & 0x0000_FFFF);
    }
}

/// Type-specific object payload storage
/// The active union member depends on the object's class ID
pub union JSObjectUnion {
    /// generic pointer
    pub opaque: *mut c_void,

    /// JS_CLASS_BOUND_FUNCTION
    pub bound_function: *mut JSBoundFunction,
    /// JS_CLASS_C_FUNCTION_DATA
    pub c_function_data_record: *mut JSCFunctionDataRecord,
    /// JS_CLASS_FOR_IN_ITERATOR
    pub for_in_iterator: *mut JSForInIterator,
    /// JS_CLASS_ARRAY_BUFFER, JS_CLASS_SHARED_ARRAY_BUFFER
    pub array_buffer: *mut JSArrayBuffer,
    /// JS_CLASS_UINT8C_ARRAY..JS_CLASS_DATAVIEW
    pub typed_array: *mut JSTypedArray,
    /// JS_CLASS_MAP..JS_CLASS_WEAKSET
    pub map_state: *mut JSMapState,
    /// JS_CLASS_MAP_ITERATOR, JS_CLASS_SET_ITERATOR
    pub map_iterator_data: *mut JSMapIteratorData,
    /// JS_CLASS_ARRAY_ITERATOR, JS_CLASS_STRING_ITERATOR
    pub array_iterator_data: *mut JSArrayIteratorData,
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

#[repr(C)]
pub struct JSObject {
    pub header: JSGCObjectHeader,

    /// object flags
    pub detailed: JSObjectFlags,

    /// Shape of object, to denote prototype and property names + flag
    /// could be NULL while freeing an object
    pub shape: *mut JSShape,

    /// property value slots
    pub prop_values: Vec<JSProperty>,

    pub first_weak_ref: *mut JSMapRecord, /* XXX: use a bit and an external hash table? */

    /// object payload
    pub u: JSObjectUnion,
}

impl JSObject {
    /// Get object's class_id
    #[inline(always)]
    pub const fn class_id(&self) -> JSClassID {
        self.detailed.class_id()
    }

    /// Get object's property layout descriptor
    /// Returns None if object is being destroyed
    #[inline(always)]
    pub const fn shape(&self) -> Option<&JSShape> {
        unsafe { self.shape.as_ref() }
    }

    /// Get mutable access to property layout
    /// Returns None during garbage collection
    #[inline(always)]
    pub fn shape_mut(&mut self) -> Option<&mut JSShape> {
        unsafe { self.shape.as_mut() }
    }

    /// Retrieve associated payload raw pointer, cast to type T.
    /// If object is not instance of `class_id`, returns None.
    /// If payload pointer is null, returns None.
    #[inline]
    pub fn opaque<T>(&self, class_id: JSClassID) -> Option<NonNull<T>> {
        // debug_assert!(
        //     unsafe { !self.u.opaque.is_null() },
        //     "JSObject must have opaque payload"
        // );

        if self.class_id() == class_id {
            unsafe { NonNull::new(self.u.opaque.cast::<T>()) }
        } else {
            None
        }
    }

    /// Retrieve associated payload raw pointer, cast to type T.
    /// If object is not instance of `class_id`, or if payload pointer is null,
    /// throw error and returns Err.
    pub fn opaque_must<T>(
        &self,
        class_id: JSClassID,
        ctx: &mut JSContext,
    ) -> Result<NonNull<T>, i32> {
        self.opaque::<T>(class_id).ok_or_else(|| {
            ctx.JS_ThrowTypeErrorInvalidClass(class_id);

            -1
        })
    }

    /// Associate native payload raw pointer with `self` object.
    ///
    /// Safety: Caller maintains `opaque` pointer validity
    #[inline(always)]
    pub fn set_opaque(&mut self, opaque: *mut c_void) {
        self.u.opaque = opaque;
    }
}

#[cfg(debug_assertions)]
impl Drop for JSObject {
    fn drop(&mut self) {
        debug_assert_eq!(self.class_id(), 0);
        debug_assert_eq!(self.header.ref_count, 0);
        debug_assert!(self.shape.is_null());
        debug_assert!(self.prop_values.is_empty());
        debug_assert!(self.first_weak_ref.is_null());
    }
}

impl JSValue {
    /// Get JSObject payload reference of JSValue.
    /// Safety: if JSValue is not a JS_TAG_OBJECT or JS_TAG_FUNCTION_BYTECODE, behaviour is undefined.
    #[inline(always)]
    pub unsafe fn object_unchecked<'a>(self) -> &'a mut JSObject {
        debug_assert!(matches!(
            JS_VALUE_GET_TAG(self),
            JS_TAG_OBJECT | JS_TAG_FUNCTION_BYTECODE
        ));
        debug_assert!(!JS_VALUE_GET_PTR(self).is_null());

        JS_VALUE_GET_PTR(self).cast::<JSObject>().as_mut().unwrap()
    }

    /// Get JSObject payload reference of JSValue.
    /// Returns None for non-object and non-bytecode-function
    #[inline]
    pub fn object<'a>(self) -> Option<&'a mut JSObject> {
        if matches!(
            JS_VALUE_GET_TAG(self),
            JS_TAG_OBJECT | JS_TAG_FUNCTION_BYTECODE
        ) {
            debug_assert!(!JS_VALUE_GET_PTR(self).is_null());
            Some(unsafe { JS_VALUE_GET_PTR(self).cast::<JSObject>().as_mut().unwrap() })
        } else {
            None
        }
    }

    #[inline]
    pub fn is_constructor(self) -> bool {
        if JS_VALUE_GET_TAG(self) != JS_TAG_OBJECT {
            false
        } else {
            unsafe { self.object_unchecked().detailed.is_constructor() }
        }
    }

    /// Seal object if the value is an object, otherwise returns the value itself duplicated.
    pub fn seal(self, ctx: &mut JSContext, freeze: bool) -> Self {
        if !self.is_object() {
            return self.dup();
        }

        match unsafe { JS_PreventExtensions(ctx, self) } {
            Ok(false) => {
                return ctx.JS_ThrowTypeError(c"proxy preventExtensions handler returned false");
            }
            Err(_) => {
                return JSValue::EXCEPTION;
            }
            _ => {}
        }

        let p = unsafe { self.object_unchecked() as *mut JSObject };
        let flags = JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK;

        let r = unsafe {
            (*p).own_property_names_foreach(ctx, flags, |cx, name| {
                let prop = name.atom;
                let mut flags = JS_PROP_THROW | JS_PROP_HAS_CONFIGURABLE;

                if freeze {
                    let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                    let res = JS_GetOwnPropertyInternal(cx, &mut desc, p, prop);
                    if res < 0 {
                        return Err(-1);
                    }
                    if res != 0 {
                        if desc.flags & JS_PROP_WRITABLE != 0 {
                            flags |= JS_PROP_HAS_WRITABLE;
                        }
                        js_free_desc(cx, &mut desc);
                    }
                }

                self.define_property(
                    cx,
                    prop,
                    JSValue::UNDEFINED,
                    JSValue::UNDEFINED,
                    JSValue::UNDEFINED,
                    flags,
                )
                .map(|_| ())
            })
        };

        if r.is_err() {
            JSValue::EXCEPTION
        } else {
            self.dup()
        }
    }

    /// check if value is sealed. Object value will check property, Non-object value is always treated as sealed.
    pub unsafe fn is_sealed(self, ctx: &mut JSContext, is_frozen: bool) -> Result<bool, i32> {
        if !self.is_object() {
            return Ok(true);
        }

        let mut p = NonNull::from(self.object_unchecked());
        let mut check_unsealed: Option<bool> = None;

        let r = p.as_mut().own_property_names_foreach(
            ctx,
            JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
            |cx, name| {
                if check_unsealed.is_none() {
                    let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                    let res = JS_GetOwnPropertyInternal(cx, &mut desc, p.as_mut(), name.atom);
                    if res < 0 {
                        return Err(-1);
                    }
                    if res != 0 {
                        js_free_desc(cx, &mut desc);
                        if desc.flags & JS_PROP_CONFIGURABLE != 0
                            || (is_frozen && (desc.flags & JS_PROP_WRITABLE != 0))
                        {
                            // has any writable prop - not sealed
                            check_unsealed.replace(true);
                        }
                    }
                }

                Ok(())
            },
        );

        if r.is_err() {
            return Err(-1);
        }

        if check_unsealed.is_some() {
            // not sealed
            Ok(false)
        } else {
            JS_IsExtensible(ctx, self)
        }
    }
}

pub const JS_PROP_INITIAL_SIZE: u32 = 2;
pub const JS_ARRAY_INITIAL_SIZE: u32 = 2;

#[cfg(target_pointer_width = "64")]
pub const JS_PROP_INITIAL_HASH_SIZE: usize = 8;
#[cfg(not(target_pointer_width = "64"))]
pub const JS_PROP_INITIAL_HASH_SIZE: usize = 4;

impl JSContext {
    /// create new instance with specified prototype value and class id.
    /// `proto` is Prototype object (can be JSValue::NULL).
    /// `proto` is not consumed.
    pub fn create_object_with_proto_and_classid(
        &mut self,
        proto: JSValue,
        class_id: JSClassID,
    ) -> JSResult {
        let shape: *mut JSShape = unsafe {
            let proto_ob = get_proto_obj(proto);
            match find_hashed_shape_proto(self.rt_mut(), proto_ob) {
                Some(sh) => js_dup_shape(sh),
                None => {
                    let new_sh = js_new_shape(self, proto_ob);
                    if new_sh.is_null() {
                        return Err(-1);
                    } else {
                        new_sh
                    }
                }
            }
        };

        self.create_object_from_shape(shape.into(), class_id)
    }

    /// create new instance of registered class_id, use registered prototype value of the class_id as prototype.
    /// returns Err if fail.
    #[inline(always)]
    pub fn create_object_with_classid(&mut self, class_id: JSClassID) -> JSResult {
        self.create_object_with_proto_and_classid(self.class_proto[class_id as usize], class_id)
    }

    /// create new instance of basic object with given prototype value.
    /// returns Err if fail.
    /// `proto` is not consumed.
    #[inline(always)]
    pub fn create_object_with_proto(&mut self, proto: JSValue) -> JSResult {
        self.create_object_with_proto_and_classid(proto, JS_CLASS_OBJECT)
    }

    /// Create new instance of basic object using default Object prototype
    /// returns Err if fail.
    #[inline(always)]
    pub fn create_object(&mut self) -> JSResult {
        self.create_object_with_classid(JS_CLASS_OBJECT)
    }
}

#[deprecated(note = "use JSContext::create_object_with_proto_and_classid() instead")]
pub unsafe fn JS_NewObjectProtoClass(
    ctx: *mut JSContext,
    proto_val: JSValueConst,
    class_id: JSClassID,
) -> JSValue {
    (*ctx)
        .create_object_with_proto_and_classid(proto_val, class_id)
        .unwrap_or(JSValue::EXCEPTION)

    // let proto = get_proto_obj(proto_val);
    // let sh: *mut JSShape = match find_hashed_shape_proto((*ctx).rt_mut(), proto) {
    //     Some(sh) => js_dup_shape(sh),
    //     None => {
    //         let new_sh = js_new_shape(ctx, proto);
    //         if new_sh.is_null() {
    //             return JSValue::EXCEPTION;
    //         } else {
    //             new_sh
    //         }
    //     }
    // };

    // JS_NewObjectFromShape(ctx, sh, class_id)
}

/// Instantiate object of specific class
/// Args:
/// - ctx: Runtime context
/// - class_id: JS_CLASS_* identifier
/// Returns: New object or Exception
#[deprecated(note = "use JSContext::create_object_with_classid() instead")]
pub unsafe fn JS_NewObjectClass(ctx: *mut JSContext, class_id: JSClassID) -> JSValue {
    (*ctx)
        .create_object_with_classid(class_id)
        .unwrap_or(JSValue::EXCEPTION)
    // JS_NewObjectProtoClass(ctx, (*ctx).class_proto[class_id as usize], class_id)
}

/// Create plain object with specified prototype
/// Parameters:
/// - ctx: JavaScript context
/// - proto: Prototype object (can be null)
#[deprecated(note = "use JSContext::create_object_with_proto() instead")]
pub unsafe fn JS_NewObjectProto(ctx: *mut JSContext, proto: JSValueConst) -> JSValue {
    JS_NewObjectProtoClass(ctx, proto, JS_CLASS_OBJECT)
}

/// Create basic object using default Object prototype
#[deprecated(note = "use JSContext::create_object() instead")]
pub unsafe fn JS_NewObject(ctx: *mut JSContext) -> JSValue {
    (*ctx).create_object().unwrap_or(JSValue::EXCEPTION)
    // JS_NewObjectProtoClass(
    //     ctx,
    //     (*ctx).class_proto[JS_CLASS_OBJECT as usize],
    //     JS_CLASS_OBJECT,
    // )
}

/// Modify an object's prototype chain
/// Args:
/// - ctx: Runtime context
/// - obj: Object to modify
/// - proto_val: New prototype (or null)
/// - throw_flag: Whether to throw on error
/// Returns: Result indicating success/failure
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

/// Checks if a JSValue is a specific C function.
/// Verifies both the function pointer and magic value match.
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
        std::ptr::fn_addr_eq((*p).u.cfunc.c_function.generic, func)
            && (*p).u.cfunc.magic == magic as i16
    } else {
        false
    }
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
    if !obj.is_object() {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"invalid 'instanceof' right operand");
        return Err(-1);
    }

    let method = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_hasInstance);
    if method.is_exception() {
        return Err(-1);
    }

    if !method.is_undefined_or_null() {
        let mut val = val;
        let ret = JS_CallFree(ctx.as_mut().unwrap(), method, obj, 1, &mut val);
        return JS_ToBoolFree(ctx, ret);
    }

    // legacy case
    if !obj.is_callable(ctx.as_ref().unwrap()) {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"invalid 'instanceof' right operand");
        return Err(-1);
    }

    JS_OrdinaryIsInstanceOf(ctx, val, obj)
}

pub unsafe fn JS_OrdinaryIsInstanceOf(
    ctx: *mut JSContext,
    val: JSValueConst,
    obj: JSValueConst,
) -> Result<bool, i32> {
    if !obj.is_callable(ctx.as_ref().unwrap()) {
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
    let obj_proto = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_prototype);
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
                    if js_poll_interrupts(ctx.as_mut().unwrap()) != 0 {
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

/// Convert JS value to object equivalent
/// Handles primitive wrapping according to ES6 ToObject conversion rules
/// Returns exception for null/undefined inputs
pub unsafe fn JS_ToObject(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    macro_rules! set_value {
        ($o:expr) => {
            if !$o.is_exception() {
                JS_SetObjectData(ctx, $o, val.dup());
            }
            return $o;
        };
    }

    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::Null | JSTag::Undefined => (*ctx).JS_ThrowTypeError(c"cannot convert to object"),
        JSTag::Object => val.dup(),
        JSTag::Exception => JSValue::EXCEPTION,
        JSTag::BigInt => {
            // Create BigInt object wrapper
            // let obj = JS_NewObjectClass(ctx, JS_CLASS_BIG_INT);
            // if !obj.is_exception() {
            //     JS_SetObjectData(ctx, obj, val.dup());
            // }
            // return obj;
            unimplemented!()
        }
        JSTag::Int | JSTag::Float64 => {
            let obj = (*ctx)
                .create_object_with_classid(JS_CLASS_NUMBER)
                .unwrap_or(JSValue::EXCEPTION);
            set_value!(obj);
        }
        JSTag::String => {
            let obj = (*ctx)
                .create_object_with_classid(JS_CLASS_STRING)
                .unwrap_or(JSValue::EXCEPTION);
            set_value!(obj);
        }
        JSTag::Symbol => {
            let obj = (*ctx)
                .create_object_with_classid(JS_CLASS_SYMBOL)
                .unwrap_or(JSValue::EXCEPTION);
            set_value!(obj);
        }
        _ => (*ctx).JS_ThrowTypeError(c"cannot convert to object"),
    }
}

pub unsafe fn JS_ToObjectFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    let obj = JS_ToObject(ctx, val);
    JS_FreeValue(ctx, val);
    obj
}

/// Store primitive value in object wrapper
/// Only works for primitive wrapper objects (Number, String, Boolean, etc.)
/// Returns -1 on error (invalid object type)
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

/// Return an Object, JS_NULL or JS_EXCEPTION in case of Proxy object.
/// `obj` keep ref count unchanged after calling.
pub unsafe fn JS_GetPrototype(ctx: *mut JSContext, obj: JSValueConst) -> JSValue {
    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let ob = obj.object_unchecked();
        if ob.class_id() == JS_CLASS_PROXY {
            js_proxy_getPrototypeOf(ctx, obj)
        } else {
            let proto = ob.shape().unwrap().proto;
            if proto.is_null() {
                JSValue::NULL
            } else {
                JS_MKPTR!(JSTag::Object, proto.cast()).dup()
            }
        }
    } else {
        JS_GetPrototypePrimitive(ctx, obj).dup()
    }
}

/// `obj` is consumed
pub unsafe fn JS_GetPrototypeFree(ctx: *mut JSContext, obj: JSValue) -> JSValue {
    let ret = JS_GetPrototype(ctx, obj);
    JS_FreeValue(ctx, obj);
    ret
}

/// Check if object has a valid 'name' property
/// Returns true if:
/// 1. Has a non-normal 'name' property (getter/setter)
/// 2. 'name' is a non-empty string
/// 3. 'name' is a symbol with description
pub unsafe fn js_object_has_name(obj: JSValueConst) -> bool {
    if let Some((prs, pr)) = (*JS_VALUE_GET_OBJ(obj)).find_own_property(JS_ATOM_name) {
        if prs.flags() & JS_PROP_TMASK != JS_PROP_NORMAL {
            true
        } else {
            let val = pr.u.value;
            if JS_VALUE_GET_TAG(val) != JS_TAG_STRING {
                true
            } else {
                !val.string().is_empty()
            }
        }
    } else {
        false
    }
}

pub unsafe fn js_get_function_name(ctx: *mut JSContext, name: JSAtom) -> JSValue {
    let mut name_str = name.to_value(ctx.as_mut().unwrap(), true);
    if JS_AtomSymbolHasDescription(ctx, name) {
        name_str = JS_ConcatString3(ctx, c"[", name_str, c"]");
    }
    name_str
}

// Note: all the fields are already sealed except length
pub unsafe fn seal_template_obj(ctx: *mut JSContext, obj: JSValueConst) -> i32 {
    let p = JS_VALUE_GET_OBJ(obj);

    if let Some(prs) = (*p).find_own_property1_mut(JS_ATOM_length) {
        let prs_flags = prs.flags();
        let mut prs = prs as *mut JSShapeProperty;

        if (*p).js_update_property_flags(
            ctx,
            std::ptr::addr_of_mut!(prs),
            prs_flags & !(JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE),
        ) != 0
        {
            return -1;
        }
    }
    (*p).detailed.set_extensible(false);

    0
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
    Ok(true)
}

pub unsafe fn js_object_seal(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    freeze_flag: i32,
) -> JSValue {
    let obj = *argv.offset(0);
    obj.seal(ctx.as_mut().unwrap(), freeze_flag != 0)
}

pub unsafe fn JS_SpeciesConstructor(
    ctx: *mut JSContext,
    obj: JSValueConst,
    default_constructor: JSValueConst,
) -> JSValue {
    if !obj.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let ctor = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_constructor);
    if ctor.is_exception() {
        return ctor;
    }
    if ctor.is_undefined() {
        return default_constructor.dup();
    }
    if !ctor.is_object() {
        JS_FreeValue(ctx, ctor);
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let species = ctor.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_species);
    JS_FreeValue(ctx, ctor);
    if species.is_exception() {
        return species;
    }

    if species.is_undefined_or_null() {
        return default_constructor.dup();
    }

    if !species.is_constructor() {
        JS_FreeValue(ctx, species);
        return (*ctx).JS_ThrowTypeError(c"not a constructor");
    }

    species
}
