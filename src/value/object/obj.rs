use std::{
    ffi::c_void,
    marker::PhantomData,
    mem::ManuallyDrop,
    ptr::NonNull,
    sync::atomic::{AtomicU16, Ordering},
};

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
    gc::{GCObject, GCObjectNodeData, GCObjectType, JSGCObjectHeader},
    generator::{JSAsyncGeneratorData, JSGeneratorData},
    iterator::{
        JSArrayIteratorData, JSAsyncFromSyncIteratorData, JSForInIterator, JSMapIteratorData,
    },
    map::{JSMapRecord, JSMapState},
    node_list::Node,
    promise::{JSPromiseData, JSPromiseFunctionData},
    proxy::{
        js_proxy_getPrototypeOf, js_proxy_preventExtensions, js_proxy_setPrototypeOf, JSProxyData,
    },
    regexp::{JSRegExp, JSRegExpStringIteratorData},
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    types::{list_add_tail, list_del, list_head, unlikely},
    value::{JS_TAG_FUNCTION_BYTECODE, JS_VALUE_GET_PTR},
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR,
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_OBJECT, JS_CLASS_PROXY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        JSTag, JSValue, JSValueConst, JS_AtomSymbolHasDescription, JS_ConcatString3, JS_FreeValue,
        JS_GetPrototypePrimitive, JS_ToBoolFree, JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_STRING,
        JS_TAG_UNDEFINED, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    js_free_desc, reset_weak_ref, JSObjectIsa, JSProperty, JSPropertyDescriptor, JSShape,
    JSShapeHandle, JSShapeProperty, JS_GetOwnPropertyInternal, JS_IsExtensible, JS_GPN_STRING_MASK,
    JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE,
    JS_PROP_NORMAL, JS_PROP_THROW, JS_PROP_TMASK, JS_PROP_WRITABLE,
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
    pub const fn set_extensible(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_EXTENSIBLE;
        } else {
            self.bitfield &= !Self::MASK_EXTENSIBLE;
        }
    }

    pub const fn free_mark(&self) -> bool {
        self.bitfield & Self::MASK_FREE_MARK != 0
    }
    pub const fn set_free_mark(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_FREE_MARK;
        } else {
            self.bitfield &= !Self::MASK_FREE_MARK;
        }
    }

    pub const fn is_exotic(&self) -> bool {
        self.bitfield & Self::MASK_IS_EXOTIC != 0
    }
    pub const fn set_exotic(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_EXOTIC;
        } else {
            self.bitfield &= !Self::MASK_IS_EXOTIC;
        }
    }

    pub const fn fast_array(&self) -> bool {
        self.bitfield & Self::MASK_FAST_ARRAY != 0
    }
    pub const fn set_fast_array(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_FAST_ARRAY
        } else {
            self.bitfield &= !Self::MASK_FAST_ARRAY;
        }
    }

    pub const fn is_constructor(&self) -> bool {
        self.bitfield & Self::MASK_IS_CONSTRUCTOR != 0
    }
    pub const fn set_constructor(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_CONSTRUCTOR;
        } else {
            self.bitfield &= !Self::MASK_IS_CONSTRUCTOR;
        }
    }

    pub const fn is_uncatchable_error(&self) -> bool {
        self.bitfield & Self::MASK_IS_UNCATCHABLE_ERROR != 0
    }
    pub const fn set_uncatchable_error(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_UNCATCHABLE_ERROR;
        } else {
            self.bitfield &= !Self::MASK_IS_UNCATCHABLE_ERROR;
        }
    }

    pub const fn tmp_mark(&self) -> bool {
        self.bitfield & Self::MASK_TMP_MARK != 0
    }
    pub const fn set_tmp_mark(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_TMP_MARK;
        } else {
            self.bitfield &= !Self::MASK_TMP_MARK;
        }
    }

    pub const fn is_htmldda(&self) -> bool {
        self.bitfield & Self::MASK_IS_HTMLDDA != 0
    }
    pub const fn set_htmldda(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_HTMLDDA;
        } else {
            self.bitfield &= !Self::MASK_IS_HTMLDDA;
        }
    }

    // #[deprecated]
    // pub const fn class_id(&self) -> JSClassID {
    //     self.bitfield & 0x0000_FFFF
    // }
    // #[deprecated]
    // pub const fn set_class_id(&mut self, id: JSClassID) {
    //     self.bitfield = (self.bitfield & 0xFFFF_0000) | (id & 0x0000_FFFF);
    // }
}

/// Type-specific object payload storage
/// The active union member depends on the object's class ID
#[repr(C)]
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

    /// JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_GENERATOR_FUNCTION,
    /// JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR_FUNCTION
    pub func: ManuallyDrop<JSObjectFunc>,

    /// JS_CLASS_C_FUNCTION
    pub cfunc: JSObjectCFunc,

    /// JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS,
    /// JS_CLASS_UINT8C_ARRAY..JS_CLASS_FLOAT64_ARRAY, JS_CLASS_DATAVIEW
    pub array: ManuallyDrop<JSObjectArray>,

    /// JS_CLASS_REGEXP
    pub regexp: JSRegExp,

    /// for JS_SetObjectData()
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

    /* XXX: use a bit and an external hash table? */
    pub first_weak_ref: *mut JSMapRecord,

    /// object payload
    pub isa: JSObjectIsa,
}

impl GCObject for JSObject {
    fn ref_count(&self) -> i32 {
        self.header.ref_count
    }

    fn inc_ref(&mut self) -> i32 {
        self.header.ref_count += 1;
        self.header.ref_count
    }

    fn dec_ref(&mut self) -> i32 {
        self.header.ref_count -= 1;
        self.header.ref_count
    }

    fn gc_type(&self) -> GCObjectType {
        GCObjectType::Object(NonNull::from(self))
    }

    fn gc_mark(&self) -> u8 {
        self.header.gc_mark()
    }

    fn set_gc_mark(&mut self, mark: u8) {
        self.header.set_gc_mark(mark);
    }

    fn gc_dump(&self, rt: &crate::runtime::JSRuntime) {
        self.dump(rt);
    }

    fn finalize(&mut self, rt: &mut JSRuntime) {
        // used to tell the object is invalid when freeing cycles
        self.detailed.set_free_mark(true);

        // free all prop definations and prop values
        // shape can't be null
        for (prs, pr) in unsafe { (*self.shape).props.values().zip(self.prop_values.drain(..)) } {
            pr.dispose(rt, prs.flags());
        }

        // as an optimization we destroy the shape immediately without putting it in gc_zero_ref_count_list
        JSShape::free(rt, self.shape.into());

        // set shape to null means object is freed
        self.shape = std::ptr::null_mut();

        if !self.first_weak_ref.is_null() {
            unsafe {
                reset_weak_ref(rt, self);
            }
        }

        // call class finalizer
        if let Some(cb) = rt.class_array[self.class_id() as usize].finalizer {
            unsafe {
                cb(rt, JS_MKPTR!(JSTag::Object, (self as *mut Self).cast()));
            }
        }

        // ManuallyDrop union
        match self.class_id() {
            JS_CLASS_BYTECODE_FUNCTION => {
                // (*(*p).u.func).home_object = std::ptr::null_mut();
                // (*self.isa.func).var_refs.clear();
                // ManuallyDrop::drop(&mut (*p).u.func);
            }
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                // ManuallyDrop::drop(&mut (*(*p).u.array).data.values);
                // ManuallyDrop::drop(&mut (*p).u.array);
            }
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                // ManuallyDrop::drop(&mut (*p).u.array);
            }
            _ => {
                // (*p).u.opaque = std::ptr::null_mut();
            }
        }
    }

    fn gc_list_node(&self) -> NonNull<Node<GCObjectNodeData>> {
        debug_assert!(!self.header.list_node.is_null());
        unsafe { NonNull::new_unchecked(self.header.list_node) }
    }
}

#[cfg(debug_assertions)]
impl Drop for JSObject {
    fn drop(&mut self) {
        //debug_assert_eq!(self.class_id(), 0);
        debug_assert_eq!(self.ref_count(), 0);
        debug_assert!(self.shape.is_null());
        debug_assert!(self.prop_values.is_empty());
        debug_assert!(self.first_weak_ref.is_null());
    }
}

impl JSObject {
    /// Get object's class_id
    #[inline(always)]
    pub const fn class_id(&self) -> JSClassID {
        //self.detailed.class_id()
        self.isa.class_id()
    }

    #[inline(always)]
    pub fn isa(&self) -> &JSObjectIsa {
        &self.isa
    }

    #[inline(always)]
    pub fn isa_mut(&mut self) -> &mut JSObjectIsa {
        &mut self.isa
    }

    /// Get object using shape.
    /// Returns None if object is being destroyed
    #[inline(always)]
    pub const fn shape(&self) -> Option<&JSShape> {
        unsafe { self.shape.as_ref() }
    }

    /// Get object using shape.
    /// Returns None if object is being destroyed
    #[inline(always)]
    pub const fn shape_mut(&mut self) -> Option<&mut JSShape> {
        unsafe { self.shape.as_mut() }
    }

    /// Retrieve associated payload raw pointer, cast to type T.
    /// If object is not instance of `class_id`, returns None.
    /// If payload pointer is null, returns None.
    #[inline]
    pub const fn opaque<T>(&self, class_id: JSClassID) -> Option<NonNull<T>> {
        unimplemented!()

        // debug_assert!(
        //     unsafe { !self.u.opaque.is_null() },
        //     "JSObject must have opaque payload"
        // );

        // if self.class_id() == class_id {
        //     unsafe { NonNull::new(self.u.opaque.cast::<T>()) }
        // } else {
        //     None
        // }
    }

    /// Retrieve associated payload raw pointer, cast to type T.
    /// If object is not instance of `class_id`, or if payload pointer is null,
    /// throw error and returns Err.
    pub fn opaque_must<T>(
        &self,
        class_id: JSClassID,
        ctx: &mut JSContext,
    ) -> Result<NonNull<T>, i32> {
        unreachable!("unused");
        // self.opaque::<T>(class_id).ok_or_else(|| {
        //     ctx.JS_ThrowTypeErrorInvalidClass(class_id);

        //     -1
        // })
    }

    /// Associate native payload raw pointer with `self` object.
    ///
    /// Safety: Caller maintains `opaque` pointer validity
    #[inline(always)]
    pub const fn set_opaque(&mut self, opaque: *mut c_void) {
        unimplemented!()
        // self.u.opaque = opaque;
    }
}

impl JSValue {
    /// Get JSObject payload reference of JSValue.
    /// Safety: if JSValue is not a JS_TAG_OBJECT or JS_TAG_FUNCTION_BYTECODE, behaviour is undefined.
    #[inline(always)]
    pub unsafe fn object_unchecked<'a>(self) -> &'a mut JSObject {
        debug_assert!(
            matches!(
                JS_VALUE_GET_TAG(self),
                JS_TAG_OBJECT | JS_TAG_FUNCTION_BYTECODE
            ),
            "tag={}",
            JS_VALUE_GET_TAG(self)
        );
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
    /// create new instance of isa type, with prototype and isa payload data.
    /// when fail, isa is returned back in Err, caller should free it.
    /// `proto` is Prototype object (can be JSValue::NULL).
    /// Note: `proto` is not consumed.
    pub fn create_instance_from_proto(
        &mut self,
        proto: JSValue,
        isa: JSObjectIsa,
    ) -> Result<JSValue, JSObjectIsa> {
        // find shape for prototype
        let shape: JSShapeHandle = unsafe {
            let proto_ob = get_proto_obj(proto);
            match self.rt().find_shape_by_proto(proto_ob) {
                Some(sh) => {
                    (*sh.as_ptr()).inc_ref();
                    sh
                }
                None => {
                    if let Ok(sh) = JSShape::new(self, proto_ob.as_mut()) {
                        sh
                    } else {
                        return Err(isa);
                    }
                }
            }
        };

        self.create_instance_from_shape(shape, isa)
    }

    /// create new instance with isa payload data.
    /// returns Err if fail.
    #[inline(always)]
    pub fn create_instance(&mut self, isa: JSObjectIsa) -> Result<JSValue, JSObjectIsa> {
        self.create_instance_from_proto(self.class_proto[isa.class_id() as usize], isa)
    }

    /// create new instance of basic object with given prototype value.
    /// returns Err if fail.
    /// `proto` is not consumed.
    #[inline(always)]
    pub fn create_base_object_with_proto(&mut self, proto: JSValue) -> JSResult {
        self.create_instance_from_proto(proto, JSObjectIsa::OBJECT)
            .map_err(|_| -1)
    }

    /// Create new instance of basic object using default Object prototype
    /// returns Err if fail.
    #[inline(always)]
    pub fn create_base_object(&mut self) -> JSResult {
        self.create_instance(JSObjectIsa::OBJECT).map_err(|_| -1)
    }
}

#[deprecated(note = "use JSContext::create_object_with_proto_and_classid() instead")]
pub unsafe fn JS_NewObjectProtoClass(
    ctx: *mut JSContext,
    proto_val: JSValueConst,
    class_id: JSClassID,
) -> JSValue {
    unreachable!("unused")

    // (*ctx)
    //     .create_object_with_proto_and_classid(proto_val, class_id)
    //     .unwrap_or(JSValue::EXCEPTION)
}

/// Instantiate object of specific class
///
///  Args:
/// - ctx: Runtime context
/// - class_id: JS_CLASS_* identifier
///
/// Returns New object or Exception
#[deprecated(note = "use JSContext::create_object_with_classid() instead")]
pub unsafe fn JS_NewObjectClass(ctx: *mut JSContext, class_id: JSClassID) -> JSValue {
    unreachable!("unused");

    // (*ctx)
    //     .create_object_with_classid(class_id)
    //     .unwrap_or(JSValue::EXCEPTION)
}

/// Create plain object with specified prototype
/// Parameters:
/// - ctx: JavaScript context
/// - proto: Prototype object (can be null)
#[deprecated(note = "use JSContext::create_base_object_with_proto() instead")]
pub unsafe fn JS_NewObjectProto(ctx: *mut JSContext, proto: JSValueConst) -> JSValue {
    JS_NewObjectProtoClass(ctx, proto, JS_CLASS_OBJECT)
}

/// Create basic object using default Object prototype
#[deprecated(note = "use JSContext::create_base_object() instead")]
pub unsafe fn JS_NewObject(ctx: *mut JSContext) -> JSValue {
    (*ctx).create_base_object().unwrap_or(JSValue::EXCEPTION)
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
        .shape_prepare_update(ctx, std::ptr::null_mut())
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

#[inline(always)]
pub unsafe fn get_proto_obj(proto_val: JSValue) -> *mut JSObject {
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
    val.object().is_some_and(|o| {
        if let JSObjectIsa::C_FUNCTION(cfunc) = o.isa() {
            std::ptr::fn_addr_eq(cfunc.c_function.generic, func) && cfunc.magic == magic as i16
        } else {
            false
        }
    })
}

/// `val` is not consumed.
pub unsafe fn JS_SetConstructorBit(
    ctx: *const JSContext,
    func_obj: JSValueConst,
    val: bool,
) -> bool {
    if JS_VALUE_GET_TAG(func_obj) != JS_TAG_OBJECT {
        false
    } else {
        let p = JS_VALUE_GET_OBJ(func_obj);
        (*p).detailed.set_constructor(val);
        true
    }
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

    if let JSObjectIsa::BOUND_FUNCTION(bfunc) = (*p).isa() {
        let s = *bfunc;
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
        t @ (JSTag::Int | JSTag::Float64 | JSTag::String | JSTag::Symbol) => (*ctx)
            .create_instance(match t {
                JSTag::Int | JSTag::Float64 => JSObjectIsa::NUMBER(val.dup()),
                JSTag::String => JSObjectIsa::STRING(val.dup()),
                JSTag::Symbol => JSObjectIsa::STRING(val.dup()),
                _ => unreachable!(),
            })
            .inspect_err(|_| {
                JS_FreeValue(ctx, val);
            })
            .unwrap_or(JSValue::EXCEPTION),
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
/// Note: `val` is consumed.
pub unsafe fn JS_SetObjectData(ctx: *mut JSContext, obj: JSValueConst, val: JSValue) -> i32 {
    if let Some(o) = obj.object() {
        if let Ok(old) = o.isa_mut().value_replace((*ctx).rt_mut(), val) {
            // free old value
            JS_FreeValue(ctx.as_mut().unwrap(), old);
            0
        } else {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"invalid object type");
            -1
        }
    } else {
        JS_FreeValue(ctx, val);
        if !obj.is_exception() {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"invalid object type");
        }
        -1
    }
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
