use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION,
    JS_CLASS_DATAVIEW, JS_CLASS_DATE, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_GENERATOR_FUNCTION,
    JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_PROXY, JS_CLASS_REGEXP, JS_CLASS_STRING,
    JS_CLASS_SYMBOL, JS_CLASS_UINT8C_ARRAY,
    atom::{JSAtom, consts::*},
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::CallContext,
    object::GCObjectWeakref,
    proxy::{js_proxy_getPrototypeOf, js_proxy_preventExtensions, js_proxy_setPrototypeOf},
    runtime::JSRuntime,
    value::{JS_GetPrototypePrimitive, JSValue},
};
use std::fmt::Formatter;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ops::Deref,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use super::{
    GCObject, JS_GPN_STRING_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE,
    JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_THROW,
    JS_PROP_WRITABLE, JSProperty, JSShape, ObjectDetail,
};

/// Bit flags and metadata for JavaScript objects
/// Encodes object state and class information in a compact bitfield
#[derive(Debug, Clone, Copy, Default)]
#[repr(transparent)]
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
}

pub(crate) struct JSObjectInner {
    rt: NonNull<JSRuntime>,

    /// object flags
    pub detailed: Cell<JSObjectFlags>,

    /// Shape of object, to denote prototype and property names + flag
    /// could be NULL while freeing an object
    pub shape: RefCell<Option<JSShape>>,

    /// property value slots
    pub prop_values: RefCell<Vec<JSProperty>>,

    /// this object is weak referenced in state.
    pub weak_refs: RefCell<Vec<JSObjectWeakref>>,

    /// object detail data
    pub detail: RefCell<ObjectDetail>,

    pub gc_mark: Cell<bool>,
}

impl Drop for JSObjectInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl std::fmt::Display for JSObjectInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let rt = self.rt();
        let class_id = self.class_id();
        let atom = rt.class_defs[class_id as usize].class_name;
        let name = atom.to_str(rt);

        #[cfg(not(debug_assertions))]
        {
            write!(f, "[{name}]")
        }
        #[cfg(debug_assertions)]
        {
            write!(
                f,
                "[{name} class={class_id} id={:p}]",
                self.id() as *const std::ffi::c_void
            )
        }
    }
}

impl JSObjectInner {
    pub(crate) fn new(
        ctx: &JSContext,
        shape: Option<JSShape>,
        isa: ObjectDetail,
    ) -> Result<Self, ObjectDetail> {
        ctx.rt_mut().gc_req = true;
        //js_trigger_gc(ctx.rt_mut(), std::mem::size_of::<JSValue>());

        // alloc prop value slots, initializing with default value
        let mut prop_values = Vec::<JSProperty>::new();
        if let Some(sh) = &shape {
            let prop_count = sh.0.borrow().prop_defs.len();

            if prop_values
                .try_reserve_exact(std::cmp::max(JS_PROP_INITIAL_SIZE as usize, prop_count))
                .is_err()
            {
                ctx.JS_ThrowOutOfMemory();
                return Err(isa);
            };

            for _ in 0..prop_count {
                prop_values.push(JSProperty::Normal(JSValue::UNDEFINED))
            }
        }

        let shape_id = shape.as_ref().map(|s| s.id());

        let mut flags = JSObjectFlags::default();
        flags.set_extensible(true);
        flags.set_free_mark(false);
        flags.set_exotic(false);
        flags.set_fast_array(false);
        flags.set_constructor(false);
        flags.set_uncatchable_error(false);
        flags.set_tmp_mark(false);
        flags.set_htmldda(false);

        let mut add_length_prop = false;

        match isa.class_id() {
            JS_CLASS_OBJECT => {}

            JS_CLASS_BYTECODE_FUNCTION
            | JS_CLASS_GENERATOR_FUNCTION
            | JS_CLASS_ASYNC_FUNCTION
            | JS_CLASS_ASYNC_GENERATOR_FUNCTION => {}

            JS_CLASS_ARRAY => {
                debug_assert!(shape_id.is_some());

                flags.set_exotic(true);
                flags.set_fast_array(true);

                // Array: the length property is always at index 0
                if shape_id.unwrap()
                    == ctx
                        .0
                        .borrow()
                        .array_shape
                        .as_ref()
                        .map_or(usize::MAX, |a| a.id())
                {
                    prop_values[0] = JSProperty::Normal(JSValue::Int(0));
                } else {
                    add_length_prop = true;
                };
            }
            JS_CLASS_ARGUMENTS | JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                flags.set_exotic(true);
                flags.set_fast_array(true);
            }
            JS_CLASS_DATAVIEW => {}

            class_id @ (JS_CLASS_NUMBER | JS_CLASS_STRING | JS_CLASS_BOOLEAN | JS_CLASS_SYMBOL
            | JS_CLASS_DATE | JS_CLASS_BIG_INT) => {
                if !ctx.rt().class_defs[class_id as usize].exotic.is_null() {
                    flags.set_exotic(true);
                }
            }

            JS_CLASS_C_FUNCTION => {}

            JS_CLASS_REGEXP => {
                unimplemented!("JS_CLASS_REGEXP");
                // *(*p).u.regexp.pattern = std::ptr::null_mut();
                // (*p).u.regexp.bytecode = std::ptr::null_mut();
                // if !(*ctx.rt)
                //     .class_array
                //     .wrapping_add(class_id as usize)
                //     .as_mut()
                //     .unwrap()
                //     .exotic
                //     .is_null()
                // {
                //     (*p). detailed.set_exotic(true);
                // }
            }

            class_id => {
                if !ctx.rt().class_defs[class_id as usize].exotic.is_null() {
                    flags.set_exotic(true);
                }
            }
        }

        let obj = Self {
            rt: ctx.rt_ptr(),
            detailed: Cell::new(flags),
            shape: RefCell::new(shape),
            prop_values: RefCell::new(prop_values),
            weak_refs: RefCell::new(Vec::new()),
            detail: RefCell::new(isa),
            gc_mark: Cell::new(false),
        };

        if add_length_prop {
            // only used for the fast array, cannot fail
            obj.add_shape_property_then(
                ctx,
                JS_ATOM_length,
                JS_PROP_WRITABLE | JS_PROP_LENGTH,
                |pr| {
                    *pr = JSProperty::Normal(JSValue::Int(0));
                },
            )
            .unwrap()
        }

        Ok(obj)
    }

    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub const fn rt_ptr(&self) -> NonNull<JSRuntime> {
        self.rt
    }

    #[inline(always)]
    pub const fn rt(&self) -> &JSRuntime {
        unsafe { self.rt.as_ref() }
    }

    #[inline(always)]
    pub const fn rt_mut(&self) -> &mut JSRuntime {
        unsafe { self.rt.as_ptr().as_mut().unwrap() }
    }

    pub fn class_id(&self) -> JSClassID {
        self.detail.borrow().class_id()
    }

    /// Test if object `self` can be called like a function, that means value is a:
    ///
    /// - Compiled bytecode functions
    /// - Proxy objects that wrapping a function
    /// - Objects of native class which has `call` handler
    pub fn is_callable(&self) -> bool {
        match self.detail.borrow().deref() {
            ObjectDetail::BYTECODE_FUNCTION(_) => true,
            ObjectDetail::PROXY(x) => x.is_func,
            s => {
                // check if class proto is callable
                self.rt()
                    .class_defs
                    .get(s.class_id() as usize)
                    .and_then(|c| c.call)
                    .is_some()
            }
        }
    }

    fn finalize(&mut self) {
        let class_id = self.class_id();

        // free shape props and props
        self.shape.take();
        self.prop_values.borrow_mut().clear();

        while let Some(weak) = self.weak_refs.borrow_mut().pop() {
            if let Some(o) = weak.upgrade() {
                let mut isa = o.0.detail.borrow_mut();
                let ms = isa.map_state_mut().unwrap();
                ms.remove_weak(self)
            }
        }

        let rt = unsafe { self.rt_ptr().as_mut() };

        // call class finalizer
        if let Some(f) = rt.class_defs[class_id as usize].finalizer {
            f(rt, self.detail.borrow().deref(), self.id());
        }

        // untrack from gc
        rt.gc_obj_list.remove(&self.id());
    }
}

#[derive(Clone)]
pub struct JSObject(pub(crate) Rc<JSObjectInner>);

impl PartialEq for JSObject {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl From<JSObject> for GCObject {
    fn from(value: JSObject) -> Self {
        Self::Object(value)
    }
}

impl std::fmt::Display for JSObject {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.as_ref())
    }
}
impl std::fmt::Debug for JSObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

impl JSObject {
    #[inline(always)]
    pub(crate) fn inner(&self) -> &Rc<JSObjectInner> {
        &self.0
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.id()
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        self.0.rt()
    }

    #[inline(always)]
    pub fn rt_mut(&self) -> &mut JSRuntime {
        self.0.rt_mut()
    }

    /// Get object class id
    #[inline(always)]
    pub fn class_id(&self) -> JSClassID {
        self.0.detail.borrow().class_id()
    }

    /// Get object class name.
    /// Note: returned atom is not duplicated, caller should clone to keep reference.
    pub fn class_name(&self) -> JSAtom {
        self.rt().class_defs[self.class_id() as usize].class_name
    }

    #[inline(always)]
    pub fn flags(&self) -> JSObjectFlags {
        self.0.detailed.get()
    }

    pub fn shape(&self) -> Option<JSShape> {
        self.0.shape.borrow().clone()
    }

    /// Test if object `self` can be called like a function, that means value is a:
    ///
    /// - Compiled bytecode functions
    /// - Proxy objects that wrapping a function
    /// - Objects of native class which has `call` handler
    #[inline(always)]
    pub fn is_callable(&self) -> bool {
        self.0.is_callable()
    }

    #[inline(always)]
    pub fn downgrade(&self) -> JSObjectWeakref {
        JSObjectWeakref(Rc::downgrade(&self.0))
    }

    #[cfg(debug_assertions)]
    pub fn ref_count(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&JSObjectInner) -> R) -> R {
        let o = self.0.deref();
        f(o)
    }

    // fn gc_status(&self) -> GCObjectStatus {
    //     self.0.header.borrow().gc_status()
    // }

    // fn gc_set_status(&self, s: GCObjectStatus) {
    //     self.0.borrow_mut().header.gc_set_status(s)
    // }

    pub fn gc_has_mark(&self) -> bool {
        self.0.gc_mark.get()
    }

    pub fn gc_set_mark(&self, mark: bool) {
        self.0.gc_mark.set(mark);
    }

    pub fn gc_dump(&self, rt: &crate::runtime::JSRuntime) {
        self.dump(rt);
    }
}

#[derive(Debug, Clone)]
pub struct JSObjectWeakref(pub(crate) Weak<JSObjectInner>);

impl From<JSObjectWeakref> for GCObjectWeakref {
    fn from(value: JSObjectWeakref) -> Self {
        Self::Object(value)
    }
}

impl Default for JSObjectWeakref {
    fn default() -> Self {
        Self(Weak::new())
    }
}

impl JSObjectWeakref {
    #[inline]
    pub fn upgrade(&self) -> Option<JSObject> {
        self.0.upgrade().map(JSObject)
    }
}

impl JSValue {
    pub const fn object(&self) -> Option<&JSObject> {
        if let Self::Object(ob) = self {
            Some(ob)
        } else {
            None
        }
    }

    pub unsafe fn object_unchecked(&self) -> &JSObject {
        if let Self::Object(ob) = self {
            ob
        } else {
            #[cfg(debug_assertions)]
            unreachable!("{self:?}");

            #[cfg(not(debug_assertions))]
            unreachable_unchecked();
        }
    }

    /// Convert JS value to object equivalent
    /// Handles primitive wrapping according to ES6 ToObject conversion rules
    /// Returns exception for null/undefined inputs.
    /// `self` is not consumed
    pub fn coerce_to_object(&self, ctx: &JSContext) -> JSResult {
        match self {
            Self::Object(_) => Ok(self.clone()),
            Self::Int(_) | Self::Float64(_) => ctx
                .create_instance(ObjectDetail::NUMBER(self.clone()))
                .map_err(|_| -1),
            Self::Bool(_) => ctx
                .create_instance(ObjectDetail::BOOLEAN(self.clone()))
                .map_err(|_| -1),
            Self::String(_) | Self::Symbol(_) => ctx
                .create_instance(ObjectDetail::STRING(self.clone()))
                .map_err(|_| -1),
            Self::NULL | Self::UNDEFINED => {
                ctx.JS_ThrowTypeError("cannot convert to object");
                Err(-1)
            }
            Self::EXCEPTION => Err(-1),
            _ => {
                ctx.JS_ThrowTypeError("cannot convert to object");
                Err(-1)
            }
        }
    }

    pub fn is_constructor(&self) -> bool {
        if let Self::Object(ob) = self {
            ob.flags().is_constructor()
        } else {
            false
        }
    }

    /// Seal object if the value is an object, otherwise returns the value itself duplicated.
    pub fn seal(self, ctx: &JSContext, freeze: bool) -> Result<Self, i32> {
        if let JSValue::Object(p) = &self {
            let this = JSValue::Object(p.clone());
            if !JS_PreventExtensions(ctx, &this)? {
                ctx.JS_ThrowTypeError("proxy preventExtensions handler returned false");
                return Err(-1);
            }

            p.own_property_names_foreach(
                ctx,
                JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
                |cx, name| {
                    let prop = name.atom;
                    let mut flags = JS_PROP_THROW | JS_PROP_HAS_CONFIGURABLE;

                    if freeze {
                        if let Some(desc) = p.get_own_property(ctx, prop)? {
                            if desc.flags & JS_PROP_WRITABLE != 0 {
                                flags |= JS_PROP_HAS_WRITABLE;
                            }
                        }
                    }

                    this.define_property(
                        cx,
                        prop,
                        &JSValue::UNDEFINED,
                        &JSValue::UNDEFINED,
                        &JSValue::UNDEFINED,
                        flags,
                    )
                    .map(|_| ())
                },
            )?;
        }

        Ok(self)
    }

    /// check if value is sealed. Object value will check property, Non-object value is always treated as sealed.
    pub fn is_sealed(&self, ctx: &JSContext, is_frozen: bool) -> Result<bool, i32> {
        if !self.is_object() {
            return Ok(true);
        }

        let p = unsafe { self.object_unchecked() };
        let mut check_unsealed: Option<bool> = None;

        p.own_property_names_foreach(ctx, JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK, |cx, name| {
            if check_unsealed.is_none() {
                if let Some(desc) = p.get_own_property(cx, name.atom)? {
                    if desc.flags & JS_PROP_CONFIGURABLE != 0
                        || (is_frozen && (desc.flags & JS_PROP_WRITABLE != 0))
                    {
                        // has any writable prop - not sealed
                        check_unsealed.replace(true);
                    }
                }
            }

            Ok(())
        })?;

        if check_unsealed.is_some() {
            // not sealed
            Ok(false)
        } else {
            self.is_extensible(ctx)
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
    /// create new instance with detailed object data.
    /// when failed, detail data is returned back in Err, caller should free it.
    /// `proto` is Prototype object (can be JSValue::NULL).
    ///
    /// Note:
    /// - New instance will apply the shape determined by `proto`.
    /// - The `detail` data determines what class the new instance is.
    pub fn create_instance_from_proto(
        &self,
        proto: &JSValue,
        detail: ObjectDetail,
    ) -> Result<JSValue, ObjectDetail> {
        // find shape by prototype
        let shape = {
            let proto_ob = proto.object();
            match self.rt().find_shape_by_proto(proto_ob) {
                Some(sh) => sh,
                None => {
                    if let Ok(sh) = JSShape::new(self, proto_ob) {
                        sh
                    } else {
                        return Err(detail);
                    }
                }
            }
        };
        self.create_instance_from_shape(shape, detail)
    }

    /// create new instance with isa payload data.
    /// returns Err if fail.
    #[inline(always)]
    pub fn create_instance(&self, isa: ObjectDetail) -> Result<JSValue, ObjectDetail> {
        self.create_instance_from_proto(&self.0.borrow().class_proto[isa.class_id() as usize], isa)
    }

    /// create new instance of basic object with given prototype value.
    /// returns Err if failed.
    /// `proto` is not consumed.
    #[inline(always)]
    pub fn create_base_object_from_proto(&self, proto: &JSValue) -> JSResult {
        self.create_instance_from_proto(proto, ObjectDetail::OBJECT)
            .map_err(|_| -1)
    }

    /// Create new instance of basic object using default Object prototype
    /// returns Err if fail.
    #[inline(always)]
    pub fn create_base_object(&self) -> JSResult {
        self.create_instance(ObjectDetail::OBJECT).map_err(|_| -1)
    }
}

/// Modify an object's prototype chain
/// Args:
/// - ctx: Runtime context
/// - obj: Object to modify
/// - proto_val: New prototype (or null)
/// - throw_flag: Whether to throw on error
/// Returns: Result indicating success/failure
pub fn JS_SetPrototypeInternal(
    ctx: &JSContext,
    obj: &JSValue,
    proto_val: JSValue,
    throw_flag: bool,
) -> Result<bool, i32> {
    if throw_flag && obj.is_undefined_or_null() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    if let JSValue::Object(p) = obj {
        let proto = if !proto_val.is_object() {
            if !proto_val.is_null() {
                ctx.JS_ThrowTypeErrorNotAnObject();
                return Err(-1);
            }
            None
        } else {
            Some(unsafe { proto_val.object_unchecked() })
        };

        if throw_flag && !obj.is_object() {
            return Ok(true);
        }
        if p.class_id() == JS_CLASS_PROXY {
            return unsafe { js_proxy_setPrototypeOf(ctx, obj, proto_val, throw_flag) };
        }

        if p.shape().unwrap().0.borrow().proto.as_ref() == proto {
            return Ok(true);
        }

        if !p.flags().extensible() {
            if throw_flag {
                ctx.JS_ThrowTypeError("object is not extensible");
                return Err(-1);
            } else {
                return Ok(false);
            }
        }
        if let Some(up) = proto {
            // check if there is a cycle
            let mut p1 = up.clone();

            loop {
                if p1.eq(p) {
                    if throw_flag {
                        ctx.JS_ThrowTypeError("circular prototype chain");
                        return Err(-1);
                    } else {
                        return Ok(false);
                    }
                }

                // Note: for Proxy objects, proto is NULL
                if let Some(p2) = p1.shape().and_then(|s| s.0.borrow().proto.clone()) {
                    p1 = p2;
                } else {
                    break;
                }
            }
        }

        p.shape_prepare_update(ctx)?;

        let sh = p.shape().unwrap();
        sh.0.borrow_mut().proto = proto.cloned();

        Ok(true)
    } else {
        ctx.JS_ThrowTypeErrorNotAnObject();
        Err(-1)
    }
}

/// `func_obj` is not consumed.
pub fn JS_SetConstructorBit(ctx: &JSContext, func_obj: &JSValue, val: bool) -> bool {
    if let JSValue::Object(p) = func_obj {
        unsafe {
            (*p.0.detailed.as_ptr()).set_constructor(val);
        }
        true
    } else {
        false
    }
}

/// Store primitive value in object wrapper
/// Only works for primitive wrapper objects (Number, String, Boolean, etc.)
/// Returns -1 on error (invalid object type)
/// Note: `val` is consumed.
pub unsafe fn JS_SetObjectData(ctx: &JSContext, obj: JSValue, val: JSValue) -> i32 {
    if let Some(o) = obj.object() {
        if let Ok(old) = o.0.detail.borrow_mut().replace_value(val) {
            drop(old);
            0
        } else {
            ctx.JS_ThrowTypeError("invalid object type");
            -1
        }
    } else {
        if !obj.is_exception() {
            ctx.JS_ThrowTypeError("invalid object type");
        }
        -1
    }
}

/// Check if object has a valid 'name' property
/// Returns true if:
/// 1. Has a non-normal 'name' property (getter/setter)
/// 2. 'name' is a non-empty string
/// 3. 'name' is a symbol with description
pub fn js_object_has_name(obj: &JSValue) -> bool {
    let ob = unsafe { obj.object_unchecked() };
    if let Some((idx, _)) = ob.find_shape_property(JS_ATOM_name) {
        if let JSProperty::Normal(JSValue::String(ss)) = ob.0.prop_values.borrow().get(idx).unwrap()
        {
            !ss.is_empty()
        } else {
            true
        }
    } else {
        false
    }
}

/// Note: all the fields are already sealed except length
pub fn seal_template_obj(ctx: &JSContext, obj: JSValue) -> i32 {
    let p = unsafe { obj.object_unchecked() };

    let (_, cur_flags) = p
        .find_shape_property(JS_ATOM_length)
        .expect("length property must exist");

    if p.js_update_property_flags(
        ctx,
        JS_ATOM_length,
        cur_flags & !(JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE),
    )
    .is_err()
    {
        return -1;
    }

    unsafe {
        (*p.0.detailed.as_ptr()).set_extensible(false);
    }

    0
}

/// return Err(-1) if exception (Proxy object only) or TRUE/FALSE
pub fn JS_PreventExtensions(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    if let JSValue::Object(p) = obj {
        if p.class_id() == JS_CLASS_PROXY {
            js_proxy_preventExtensions(ctx, obj)
        } else {
            unsafe {
                (*p.0.detailed.as_ptr()).set_extensible(false);
            }
            Ok(true)
        }
    } else {
        Ok(false)
    }
}

pub fn js_object_seal(ctx: &JSContext, cc: CallContext) -> JSResult {
    let freeze_flag = cc.magic();
    let obj = cc.arg(0).clone();
    obj.seal(ctx, freeze_flag != 0)
}

pub fn JS_SpeciesConstructor(
    ctx: &JSContext,
    obj: &JSValue,
    default_constructor: JSValue,
) -> JSResult {
    if !obj.is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let ctor = obj
        .get_property(ctx, JS_ATOM_constructor)
        .check_exception()?;
    if ctor.is_undefined() {
        return Ok(default_constructor);
    }

    if !ctor.is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let species = ctor
        .get_property(ctx, JS_ATOM_Symbol_species)
        .check_exception()?;
    if species.is_undefined_or_null() {
        return Ok(default_constructor);
    }

    if !species.is_constructor() {
        ctx.JS_ThrowTypeError("not a constructor");
        return Err(-1);
    }

    Ok(species)
}
