use std::{fmt::Formatter, ops::Deref};

use gc_lite::{GcRef, GcTracable, GcTraceOp, GcWeak};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION,
    JS_CLASS_DATAVIEW, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_NUMBER,
    JS_CLASS_OBJECT, JS_CLASS_STRING, JS_CLASS_UINT8C_ARRAY, atom::Atom, class::ClassID,
    context::ContextImpl, error::JSResult, gcref_wrapper, runtime::RuntimePtr, utils::unlikely,
    value::JSValue,
};

use super::{
    JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE,
    JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_THROW, JS_PROP_WRITABLE, JSProperty,
    ObjectPayload, Shape,
};

/// TODO: use bitfield instead
/// Bit flags and metadata for JavaScript objects
/// Encodes object state and class information in a compact bitfield
#[derive(Debug, Clone, Copy, Default)]
#[repr(transparent)]
pub struct ObjectFlags {
    /// Bitfield storing object state flags and class ID:
    /// - Bits 31-23: Object attr flags
    ///   [31] extensible: Can add new properties
    ///   [30] free_mark: Object destruction marker
    ///   [29] is_exotic: Special property handling
    ///   [28] fast_array: Uses optimized array storage
    ///   [27] constructor: Object is a constructor
    ///   [26] immutable prototype
    ///   [25] uncatchable_error: Error cannot be caught
    ///   [24] tmp_mark: Temporary serialization marker
    ///   [23] HTMLDDA: Special browser compatibility behavior
    /// - Bits 15-0: Class ID (JS_CLASS_* constant)
    bitfield: u32,
}

impl ObjectFlags {
    const MASK_EXTENSIBLE: u32 = 0x8000_0000;
    const MASK_FREE_MARK: u32 = 0x4000_0000;
    const MASK_IS_EXOTIC: u32 = 0x2000_0000;
    const MASK_FAST_ARRAY: u32 = 0x1000_0000;
    const MASK_IS_CONSTRUCTOR: u32 = 0x0800_0000;
    const MASK_IMMUTABLE_PROTOTYPE: u32 = 0x0400_0000;
    const MASK_IS_UNCATCHABLE_ERROR: u32 = 0x0200_0000;
    const MASK_TMP_MARK: u32 = 0x0100_0000;
    const MASK_IS_HTMLDDA: u32 = 0x0080_0000;

    pub const fn extensible(&self) -> bool {
        self.bitfield & Self::MASK_EXTENSIBLE != 0
    }
    const fn set_extensible(&mut self, b: bool) {
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

    pub const fn is_fast_array(&self) -> bool {
        self.bitfield & Self::MASK_FAST_ARRAY != 0
    }
    const fn set_fast_array(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_FAST_ARRAY
        } else {
            self.bitfield &= !Self::MASK_FAST_ARRAY;
        }
    }

    pub const fn is_constructor(&self) -> bool {
        self.bitfield & Self::MASK_IS_CONSTRUCTOR != 0
    }
    const fn set_constructor(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IS_CONSTRUCTOR;
        } else {
            self.bitfield &= !Self::MASK_IS_CONSTRUCTOR;
        }
    }

    pub const fn immutable_prototype(&self) -> bool {
        self.bitfield & Self::MASK_IMMUTABLE_PROTOTYPE != 0
    }
    const fn set_immutable_prototype(&mut self, b: bool) {
        if b {
            self.bitfield |= Self::MASK_IMMUTABLE_PROTOTYPE;
        } else {
            self.bitfield &= !Self::MASK_IMMUTABLE_PROTOTYPE;
        }
    }

    pub const fn is_uncatchable_error(&self) -> bool {
        self.bitfield & Self::MASK_IS_UNCATCHABLE_ERROR != 0
    }
    const fn set_uncatchable_error(&mut self, b: bool) {
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

pub struct ObjectImpl {
    rt: RuntimePtr,

    /// object flags
    obj_flags: ObjectFlags,

    /// Shape of object, to denote prototype and property names + flag
    shape: Option<Shape>,

    /// property value slots
    pub(super) prop_values: Vec<JSProperty>,

    /// this object is weak referenced in state.
    #[cfg(feature = "builtin_mapset")]
    pub(crate) weak_refs: Vec<GcWeak<ObjectImpl>>,

    /// object payload.
    pub(super) payload: ObjectPayload,
}

unsafe impl GcTracable for ObjectImpl {
    fn trace(&self, tr: GcTraceOp) {
        self.shape.trace(tr);
        for p in self.prop_values.iter() {
            p.trace(tr);
        }
        self.payload.trace(tr);
    }
}

impl Drop for ObjectImpl {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl std::fmt::Display for ObjectImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let rt = self.rt();
        let class_id = self.class_id();
        let name = rt.class_registry[class_id as usize].class_name.to_utf8(&rt);

        #[cfg(debug_assertions)]
        {
            write!(
                f,
                "[{name} {:p} shape={:p} proto={}]",
                self.id() as *const std::ffi::c_void,
                self.shape().map_or(0, |s| s.id()) as *const std::ffi::c_void,
                self.shape()
                    .and_then(|s| s.proto())
                    .map_or("<null>".to_string(), |x| x.to_string())
            )
        }
        #[cfg(not(debug_assertions))]
        {
            write!(f, "[{name}]")
        }
    }
}

impl ObjectImpl {
    pub(crate) fn create(
        ctx: &ContextImpl,
        mut shape: Option<Shape>,
        payload: ObjectPayload,
    ) -> Result<Object, ObjectPayload> {
        #[cfg(debug_assertions)]
        if let Some(sh) = &shape {
            debug_assert!(
                ctx.rt_ptr().is_shape_registered(sh),
                "shape must be registered when used by object"
            );
            debug_assert!(sh.use_count() > 0);
        }

        // alloc prop value slots, initializing with default value
        let mut prop_values = Vec::<JSProperty>::new();
        if let Some(sh) = &mut shape {
            let prop_count = sh.len();
            if prop_values
                .try_reserve_exact(std::cmp::max(JS_PROP_INITIAL_SIZE as usize, prop_count))
                .is_err()
            {
                sh.dec_count_noop();
                ctx.throw_out_of_memory();
                return Err(payload);
            };

            for _ in 0..prop_count {
                prop_values.push(JSProperty::default())
            }
        }

        let shape_id = shape.as_ref().map(|s| s.id());

        let mut flags = ObjectFlags::default();
        flags.set_extensible(true);
        flags.set_free_mark(false);
        flags.set_exotic(false);
        flags.set_fast_array(false);
        flags.set_constructor(false);
        flags.set_immutable_prototype(false);
        flags.set_uncatchable_error(false);
        flags.set_tmp_mark(false);
        flags.set_htmldda(false);

        let mut add_length_prop = false;

        match payload.class_id() {
            JS_CLASS_OBJECT => {}

            JS_CLASS_BYTECODE_FUNCTION
            | JS_CLASS_GENERATOR_FUNCTION
            | JS_CLASS_ASYNC_FUNCTION
            | JS_CLASS_ASYNC_GENERATOR_FUNCTION => {}

            JS_CLASS_ARRAY => {
                debug_assert!(shape_id.is_some());

                flags.set_exotic(true);
                flags.set_fast_array(true);

                if shape_id.unwrap() == ctx.array_shape.as_ref().map_or(usize::MAX, |sh| sh.id()) {
                    // set length property to 0
                    prop_values[0] = JSProperty::Normal(JSValue::Int(0));
                } else {
                    // we are creating the Array proto object.
                    // by this moment, the default shape for array is not initialized yet.
                    add_length_prop = true;
                };
            }
            JS_CLASS_ARGUMENTS | JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                flags.set_exotic(true);
                flags.set_fast_array(true);
            }
            JS_CLASS_DATAVIEW => {}

            class_id
            @ (JS_CLASS_NUMBER | JS_CLASS_STRING | JS_CLASS_BOOLEAN | JS_CLASS_BIG_INT) => {
                if ctx.rt_ptr().class_registry[class_id as usize]
                    .exotic
                    .is_some()
                {
                    flags.set_exotic(true);
                }
            }
            #[cfg(feature = "builtin_symbol")]
            crate::JS_CLASS_SYMBOL => {
                if ctx.rt_ptr().class_registry[crate::JS_CLASS_SYMBOL as usize]
                    .exotic
                    .is_some()
                {
                    flags.set_exotic(true);
                }
            }

            JS_CLASS_C_FUNCTION => {}

            #[cfg(feature = "builtin_regexp")]
            crate::JS_CLASS_REGEXP => {}

            class_id => {
                if ctx.rt_ptr().class_registry[class_id as usize]
                    .exotic
                    .is_some()
                {
                    flags.set_exotic(true);
                }
            }
        }

        let data = Self {
            rt: ctx.rt_ptr(),
            obj_flags: flags,
            shape,
            prop_values,
            payload,
            #[cfg(feature = "builtin_mapset")]
            weak_refs: Vec::new(),
        };

        let ob_impl = ctx.rt_ptr().gc_alloc(data).map_err(|(_, mut o)| {
            ctx.throw_out_of_memory();
            std::mem::replace(&mut o.payload, ObjectPayload::NUMBER(JSValue::Int(0)))
        })?;
        let mut obj = Object(ob_impl);

        if add_length_prop {
            // only for the Array prototype object
            obj.add_own_property(
                ctx,
                Atom::CONST_length,
                JS_PROP_WRITABLE | JS_PROP_LENGTH,
                JSProperty::Normal(JSValue::Int(0)),
            )
            .inspect_err(|_| {
                if let Some(mut sh0) = obj.shape.take() {
                    sh0.dec_count_noop();
                }
            })
            .unwrap();
        }

        Ok(obj)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub(crate) const fn rt(&self) -> RuntimePtr {
        self.rt
    }

    /// get object payload
    #[inline(always)]
    pub const fn payload(&self) -> &ObjectPayload {
        &self.payload
    }

    /// get object payload
    #[inline(always)]
    pub const fn payload_mut(&mut self) -> &mut ObjectPayload {
        &mut self.payload
    }

    #[inline(always)]
    pub const fn class_id(&self) -> ClassID {
        self.payload.class_id()
    }

    /// Get object class name atom.
    #[inline(always)]
    pub fn class_name(&self) -> &Atom {
        debug_assert!((self.class_id() as usize) < self.rt.class_registry.len());
        unsafe {
            &self
                .rt
                .class_registry
                .get_unchecked(self.class_id() as usize)
                .class_name
        }
    }

    /// Get object flags
    #[inline(always)]
    pub const fn flags(&self) -> ObjectFlags {
        self.obj_flags
    }

    /// test if object is fast array (could be one of Array, Argument, TypedArray fast array).
    #[inline(always)]
    pub const fn is_fast_array(&self) -> bool {
        self.flags().is_fast_array()
    }

    #[inline(always)]
    pub const fn is_extensible(&self) -> bool {
        self.flags().extensible()
    }

    #[inline(always)]
    pub const fn set_flag_fast_array(&mut self, b: bool) {
        self.obj_flags.set_fast_array(b);
    }

    #[inline(always)]
    pub const fn set_flag_constructor(&mut self, b: bool) {
        self.obj_flags.set_constructor(b);
    }

    #[inline(always)]
    pub const fn set_flag_extensible(&mut self, b: bool) {
        self.obj_flags.set_extensible(b);
    }

    #[inline(always)]
    pub const fn set_flag_immutable_prototype(&mut self, b: bool) {
        self.obj_flags.set_immutable_prototype(b);
    }

    #[inline(always)]
    pub const fn set_flag_uncatchable_error(&mut self, b: bool) {
        self.obj_flags.set_uncatchable_error(b);
    }

    #[inline(always)]
    pub const fn is_constructor(&self) -> bool {
        self.obj_flags.is_constructor()
    }

    /// Test if object `self` can be called like a function, that means value is a:
    ///
    /// - bytecode functions
    /// - native function
    /// - Proxy objects that wrapping a function
    /// - Objects of native class which has `call` handler
    pub fn is_function(&self) -> bool {
        match &self.payload {
            ObjectPayload::BYTECODE_FUNCTION(_) => true,
            #[cfg(feature = "builtin_proxy")]
            ObjectPayload::PROXY(x) => x.is_func(),
            isa => {
                // check if class proto has exotic call handler
                self.rt()
                    .class_registry
                    .get(isa.class_id() as usize)
                    .and_then(|c| c.call)
                    .is_some()
            }
        }
    }

    #[inline(always)]
    pub const fn shape(&self) -> Option<&Shape> {
        self.shape.as_ref()
    }

    #[inline(always)]
    pub const fn take_shape(&mut self) -> Option<Shape> {
        self.shape.take()
    }

    pub fn set_shape(&mut self, shape: Shape) {
        #[cfg(debug_assertions)]
        {
            debug_assert!(self.rt().is_shape_registered(&shape));
            debug_assert!(
                self.shape.as_ref().is_none_or(|sh0| *sh0 != shape),
                "can't set same shape"
            );
        }

        let obj = unsafe { Object::from_ref_unchecked(self) };

        log::trace!(
            "[object_shape] {:?} : {:?}",
            obj.gc_ref().node_info(),
            shape.gc_ref().node_info()
        );

        let obj_scope = obj.gc_ref().node_info().get_partition_id();
        if obj_scope != shape.gc_ref().node_info().get_partition_id() {
            self.rt()
                .gc_heap_mut()
                .set_xref(obj_scope, shape.gc_ref().node_ptr());
        }

        self.shape.replace(shape);
    }

    /// only Proxy may return Err(-1)
    pub fn prevent_extensions(&mut self, ctx: &ContextImpl) -> Result<bool, i32> {
        #[cfg(feature = "builtin_proxy")]
        {
            Object(unsafe { GcRef::from_ref_unchecked(self) })
                .as_Proxy_mut()
                .map_or_else(
                    || {
                        self.set_flag_extensible(false);
                        Ok(true)
                    },
                    |p| p.prevent_extensions(ctx),
                )
        }

        #[cfg(not(feature = "builtin_proxy"))]
        {
            self.set_flag_extensible(false);
            Ok(true)
        }
    }

    fn finalize(&mut self) {
        let mut rt = self.rt();

        self.shape.take();
        self.prop_values.clear();

        // Map Set object remove weakref
        #[cfg(feature = "builtin_mapset")]
        {
            let heap = rt.gc_heap();
            while let Some(w) = self.weak_refs.pop() {
                if let Some(mut o) = w.upgrade(heap) {
                    o.payload.mapset_mut().unwrap().remove_weak_obj(self)
                }
            }
        }

        // call class finalize handler.
        if let Some(cb) = rt.class_registry[self.class_id() as usize].finalize {
            let id = self.id();
            cb(&mut rt, &self.payload, id);
        }
    }
}

/// An object reference
#[derive(Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct Object(pub(crate) GcRef<ObjectImpl>);

gcref_wrapper!(Object, ObjectImpl);

impl From<Object> for JSValue {
    #[inline(always)]
    fn from(value: Object) -> Self {
        Self::Object(value)
    }
}

impl TryFrom<JSValue> for Object {
    type Error = i32;

    #[inline]
    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if let JSValue::Object(o) = value {
            Ok(o)
        } else {
            Err(-1)
        }
    }
}

impl From<GcRef<ObjectImpl>> for Object {
    #[inline(always)]
    fn from(value: GcRef<ObjectImpl>) -> Self {
        Self(value)
    }
}

impl From<GcRef<ObjectImpl>> for JSValue {
    #[inline(always)]
    fn from(value: GcRef<ObjectImpl>) -> Self {
        Self::Object(Object(value))
    }
}

impl std::fmt::Display for Object {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.deref())
    }
}

impl std::fmt::Debug for Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

impl Object {
    #[inline(always)]
    pub fn downgrade(&self) -> GcWeak<ObjectImpl> {
        self.0.downgrade(self.rt().gc_heap_mut())
    }

    /// Seal the object.
    pub fn seal(&mut self, ctx: &ContextImpl, freeze: bool) -> Result<(), i32> {
        if unlikely(!self.prevent_extensions(ctx)?) {
            ctx.throw_type_error("proxy preventExtensions handler returned false");
            Err(-1)
        } else {
            let this = *self;
            let _ = this.own_property_names_foreach(
                ctx,
                JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK,
                |_, pkey| {
                    let mut flags = JS_PROP_THROW | JS_PROP_HAS_CONFIGURABLE;

                    if freeze
                        && let Some(desc) = self.get_own_property(ctx, pkey.atom)?
                        && desc.flags & JS_PROP_WRITABLE != 0
                    {
                        flags |= JS_PROP_HAS_WRITABLE;
                    }

                    // equivelent to set {configurable: false},
                    // or {configurable:false, writable: false} if freeze=true
                    self.define_property(
                        ctx,
                        pkey.atom,
                        JSValue::UNDEFINED,
                        JSValue::UNDEFINED,
                        JSValue::UNDEFINED,
                        flags,
                    )?;

                    Ok(())
                },
            );

            Ok(())
        }
    }

    /// check if object is sealed.
    pub fn is_sealed(&mut self, ctx: &ContextImpl, is_frozen: bool) -> Result<bool, i32> {
        const NOT_SEALED_ERR: i32 = i32::MAX;

        let this = *self;

        if let Err(e) =
            this.own_property_names_foreach(ctx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK, |_, pk| {
                if let Some(desc) = self.get_own_property(ctx, pk.atom)?
                    && (desc.flags & JS_PROP_CONFIGURABLE != 0
                        || (is_frozen && (desc.flags & JS_PROP_WRITABLE != 0)))
                {
                    // found writable prop: that means object is not sealed
                    // return Err to stop iterate name
                    return Err(NOT_SEALED_ERR);
                }
                Ok(())
            })
        {
            return if e == NOT_SEALED_ERR {
                Ok(false) // not sealed
            } else {
                Err(e) // real error
            };
        }

        self.is_extensible(ctx).map(|b| !b)
    }
}

impl JSValue {
    #[inline]
    pub const fn as_object(&self) -> Option<&Object> {
        if let Self::Object(o) = self {
            Some(o)
        } else {
            None
        }
    }

    #[inline]
    pub const fn as_object_mut(&mut self) -> Option<&mut Object> {
        if let Self::Object(o) = self {
            Some(o)
        } else {
            None
        }
    }

    #[inline]
    pub unsafe fn as_object_unchecked(&self) -> &Object {
        if let Self::Object(o) = self {
            o
        } else {
            #[cfg(debug_assertions)]
            unreachable!("{self:?}");
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    #[inline]
    pub unsafe fn as_object_mut_unchecked(&mut self) -> &mut Object {
        if let Self::Object(o) = self {
            o
        } else {
            #[cfg(debug_assertions)]
            unreachable!("{self:?}");
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    #[inline]
    pub const fn to_object(self) -> Option<Object> {
        if let Self::Object(o) = self {
            Some(o)
        } else {
            None
        }
    }

    #[inline]
    pub unsafe fn to_object_unchecked(self) -> Object {
        if let Self::Object(o) = self {
            o
        } else {
            #[cfg(debug_assertions)]
            unreachable!("{self:?}");
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// Migrage value to object.
    /// Handles primitive wrapping according to ES6 ToObject conversion rules.
    ///
    /// For example:
    /// - object => object (as-is)
    /// - 10 => \[object 10\]
    /// - true => \[object true\]
    /// - "abc" => \[object "abc"\]
    /// - Symbol() => \[object Symbol()\]
    /// - null, undefined => Error
    pub fn objectify(&self, ctx: &ContextImpl) -> Result<Object, i32> {
        match self {
            Self::Object(o) => Ok(*o),
            Self::String(s) => s.objectify(ctx, JSValue::UNDEFINED),

            Self::Int(_) | Self::Float64(_) => ctx
                .create_object(ObjectPayload::NUMBER(*self))
                .map_err(|_| -1),
            Self::Bool(_) => ctx
                .create_object(ObjectPayload::BOOLEAN(*self))
                .map_err(|_| -1),
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(_) => ctx
                .create_object(ObjectPayload::SYMBOL(*self))
                .map_err(|_| -1),

            Self::EXCEPTION => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                Err(-1)
            }
            _ => {
                ctx.throw_type_error_not_an_object();
                Err(-1)
            }
        }
    }

    pub fn is_constructor(&self) -> bool {
        if let Self::Object(o) = self {
            o.flags().is_constructor()
        } else {
            false
        }
    }

    #[inline]
    pub fn prevent_extensions(&mut self, ctx: &ContextImpl) -> Result<bool, i32> {
        self.as_object_mut()
            .map_or(Ok(false), |o| o.prevent_extensions(ctx))
    }

    /// Seal `self` if it is an object.
    pub fn seal(&mut self, ctx: &ContextImpl, freeze: bool) -> Result<(), i32> {
        self.as_object_mut().map_or(Ok(()), |o| o.seal(ctx, freeze))
    }

    /// check if value is sealed. Object value will check property, Non-object value is always treated as sealed.
    pub fn is_sealed(&mut self, ctx: &ContextImpl, is_frozen: bool) -> Result<bool, i32> {
        self.as_object_mut()
            .map_or(Ok(true), |o| o.is_sealed(ctx, is_frozen))
    }
}

const JS_PROP_INITIAL_SIZE: u32 = 2;
//pub const JS_ARRAY_INITIAL_SIZE: u32 = 2;

pub(crate) const JS_PROP_INITIAL_HASH_SIZE: usize = if cfg!(target_pointer_width = "64") {
    32
} else {
    4
};

/// Note: all the fields are already sealed except length
pub fn seal_template_obj(ctx: &ContextImpl, mut obj: JSValue) -> i32 {
    let p = unsafe { obj.as_object_mut_unchecked() };

    // let (_, cur_flags) = p
    //     .find_shape_property(Atom::CONST_length)
    //     .expect("length property must exist");

    if p.modify_shape_property_flags(ctx, Atom::CONST_length, |f| {
        f & !(JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE)
    })
    .is_err()
    {
        return -1;
    }

    p.set_flag_extensible(false);
    0
}

pub fn JS_SpeciesConstructor(
    ctx: &ContextImpl,
    obj: JSValue,
    default_constructor: JSValue,
) -> JSResult {
    if let Some(o) = obj.as_object() {
        let ctor = o.get_property(ctx, Atom::CONST_constructor)?;

        if unlikely(ctor.is_undefined()) {
            Ok(default_constructor)
        } else if let Some(o1) = ctor.as_object() {
            let species: JSValue = {
                #[cfg(feature = "builtin_symbol")]
                {
                    o1.get_property(ctx, Atom::CONST_Symbol_species)?
                }
                #[cfg(not(feature = "builtin_symbol"))]
                {
                    JSValue::UNDEFINED
                }
            };

            if species.is_undefined_or_null() {
                Ok(default_constructor)
            } else if unlikely(!species.is_constructor()) {
                ctx.throw_type_error("not a constructor");
                Err(-1)
            } else {
                Ok(species)
            }
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}
