use std::{
    cell::{Cell, RefCell},
    fmt::Formatter,
    ops::Deref,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION,
    JS_CLASS_DATAVIEW, JS_CLASS_DATE, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_GENERATOR_FUNCTION,
    JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_REGEXP, JS_CLASS_STRING, JS_CLASS_SYMBOL,
    JS_CLASS_UINT8C_ARRAY,
    atom::{Atom, consts::*},
    branch_hints::unlikely,
    class::ClassID,
    context::JSContext,
    error::JSResult,
    object::GCObjectWeakref,
    runtime::JSRuntime,
    value::JSValue,
};

use super::{
    GCObject, JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE,
    JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_THROW,
    JS_PROP_WRITABLE, JSProperty, JSShape, ObjectPayload,
};

/// Bit flags and metadata for JavaScript objects
/// Encodes object state and class information in a compact bitfield
#[derive(Debug, Clone, Copy, Default)]
#[repr(transparent)]
pub struct ObjectFlags {
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

impl ObjectFlags {
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

pub(crate) struct ObjectInner {
    rt: NonNull<JSRuntime>,
    pub(crate) gc_mark: Cell<bool>,

    /// object flags
    obj_flags: Cell<ObjectFlags>,

    /// Shape of object, to denote prototype and property names + flag
    /// could be NULL while freeing an object
    pub(crate) shape: RefCell<Option<JSShape>>,

    /// property value slots
    pub(crate) prop_values: RefCell<Vec<JSProperty>>,

    /// this object is weak referenced in state.
    pub(crate) weak_refs: RefCell<Vec<JSObjectWeakref>>,

    /// object payload.
    payload: ObjectPayload,
}

impl Drop for ObjectInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl std::fmt::Display for ObjectInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let rt = self.rt();
        let class_id = self.class_id();
        let name = rt.class_registry[class_id as usize].class_name.to_utf8();

        #[cfg(debug_assertions)]
        {
            let sh = self.shape.borrow();
            write!(
                f,
                "[{name} {:p} shape={:p} proto={}]",
                self.id() as *const std::ffi::c_void,
                sh.as_ref().map_or(0, |s| s.id()) as *const std::ffi::c_void,
                sh.as_ref()
                    .and_then(|s| s.0.borrow().proto.clone())
                    .map_or("<null>".to_string(), |x| x.to_string())
            )
        }
        #[cfg(not(debug_assertions))]
        {
            write!(f, "[{name}]")
        }
    }
}

impl ObjectInner {
    pub(crate) fn new(
        ctx: &JSContext,
        shape: Option<JSShape>,
        payload: ObjectPayload,
    ) -> Result<Self, ObjectPayload> {
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

                if shape_id.unwrap()
                    == ctx
                        .0
                        .borrow()
                        .array_shape
                        .as_ref()
                        .map_or(usize::MAX, |sh| sh.id())
                {
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

            class_id @ (JS_CLASS_NUMBER | JS_CLASS_STRING | JS_CLASS_BOOLEAN | JS_CLASS_SYMBOL
            | JS_CLASS_DATE | JS_CLASS_BIG_INT) => {
                if ctx.rt().class_registry[class_id as usize].exotic.is_some() {
                    flags.set_exotic(true);
                }
            }

            JS_CLASS_C_FUNCTION => {}

            JS_CLASS_REGEXP => {
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
                if ctx.rt().class_registry[class_id as usize].exotic.is_some() {
                    flags.set_exotic(true);
                }
            }
        }

        let obj = Self {
            rt: ctx.rt_ptr(),
            obj_flags: Cell::new(flags),
            shape: RefCell::new(shape),
            prop_values: RefCell::new(prop_values),
            payload,
            weak_refs: RefCell::new(Vec::new()),
            gc_mark: Cell::new(false),
        };

        if add_length_prop {
            // only used for the Array prototype object
            obj.add_own_property(
                ctx,
                JS_ATOM_length.deref().clone(),
                JS_PROP_WRITABLE | JS_PROP_LENGTH,
                JSProperty::Normal(JSValue::Int(0)),
            )
            .unwrap()
        }

        Ok(obj)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub const fn rt_ptr(&self) -> NonNull<JSRuntime> {
        self.rt
    }

    #[inline]
    pub const fn rt(&self) -> &JSRuntime {
        unsafe { self.rt.as_ref() }
    }

    #[inline(always)]
    pub fn class_id(&self) -> ClassID {
        self.payload.class_id()
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

    fn finalize(&mut self) {
        // free shape props and props
        // self.shape.take();
        // self.prop_values.borrow_mut().clear();

        while let Some(w) = self.weak_refs.borrow_mut().pop() {
            if let Some(o) = w.upgrade() {
                o.0.payload
                    .mapset()
                    .unwrap()
                    .borrow_mut()
                    .remove_weak_obj(self)
            }
        }

        // call class finalize.
        // object won't be accessed any more, so we can take payload out
        let rt = unsafe { self.rt_ptr().as_mut() };
        if let Some(f) = rt.class_registry[self.class_id() as usize].finalize {
            let id = self.id();
            f(rt, &self.payload, id);
        }

        // untrack from gc
        rt.gc_obj_list.remove(&self.id());
    }
}

#[derive(Clone)]
pub struct JSObject(pub(crate) Rc<ObjectInner>);

impl PartialEq for JSObject {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl From<JSObject> for JSValue {
    #[inline(always)]
    fn from(value: JSObject) -> Self {
        Self::Object(value)
    }
}

impl TryFrom<JSValue> for JSObject {
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

impl From<JSObject> for GCObject {
    #[inline(always)]
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
    pub fn id(&self) -> usize {
        self.0.id()
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        self.0.rt()
    }

    /// get object payload
    #[inline(always)]
    pub fn payload(&self) -> &ObjectPayload {
        &self.0.payload
    }

    /// Get object class id
    #[inline(always)]
    pub fn class_id(&self) -> ClassID {
        self.payload().class_id()
    }

    /// Get object class name atom.
    #[inline]
    pub fn class_name(&self) -> &Atom {
        &self.rt().class_registry[self.class_id() as usize].class_name
    }

    /// Get object flags
    #[inline]
    pub fn flags(&self) -> ObjectFlags {
        self.0.obj_flags.get()
    }
    #[inline]
    pub fn set_flag_fast_array(&self, b: bool) {
        unsafe {
            (&mut *self.0.obj_flags.as_ptr()).set_fast_array(b);
        }
        // equivelent but slower solution:
        // let mut f = self.flags();
        // f.set_fast_array(b);
        // self.0.obj_flags.set(f);
    }
    #[inline]
    pub fn set_flag_constructor(&self, b: bool) {
        unsafe {
            (&mut *self.0.obj_flags.as_ptr()).set_constructor(b);
        }
        // for equivelent but slower solution: see above
    }
    #[inline]
    pub fn set_flag_extensible(&self, b: bool) {
        unsafe {
            (&mut *self.0.obj_flags.as_ptr()).set_extensible(b);
        }
        // for equivelent but slower solution: see above
    }
    #[inline]
    pub fn set_flag_uncatchable_error(&self, b: bool) {
        unsafe {
            (&mut *self.0.obj_flags.as_ptr()).set_uncatchable_error(b);
        }
        // for equivelent but slower solution: see above
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
    pub fn is_function(&self) -> bool {
        self.0.is_function()
    }

    #[inline(always)]
    pub fn is_constructor(&self) -> bool {
        self.flags().is_constructor()
    }

    /// return Err(-1) for Proxy only
    pub fn prevent_extensions(&self, ctx: &JSContext) -> Result<bool, i32> {
        self.as_Proxy().map_or_else(
            || {
                self.set_flag_extensible(false);
                Ok(true)
            },
            |p| p.prevent_extensions(ctx),
        )
    }

    pub fn method_set_home_object(&self, home_obj: Option<&JSObject>) {
        if let Some(fo) = self.payload().bc_function()
            && fo.bytecode().need_home_object()
        {
            fo.set_home_object(home_obj.cloned());
        }
    }

    /// Seal the object.
    pub fn seal(&self, ctx: &JSContext, freeze: bool) -> Result<(), i32> {
        if unlikely(!self.prevent_extensions(ctx)?) {
            ctx.throw_type_error("proxy preventExtensions handler returned false");
            Err(-1)
        } else {
            let _ = self.own_property_names_foreach(
                ctx,
                JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK,
                |_, pkey| {
                    let mut flags = JS_PROP_THROW | JS_PROP_HAS_CONFIGURABLE;

                    if freeze
                        && let Some(desc) = self.get_own_property(ctx, &pkey.atom)?
                        && desc.flags & JS_PROP_WRITABLE != 0
                    {
                        flags |= JS_PROP_HAS_WRITABLE;
                    }

                    // equivelent to set {configurable: false},
                    // or {configurable:false, writable: false} if freeze=true
                    self.define_property(
                        ctx,
                        &pkey.atom,
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
    pub fn is_sealed(&self, ctx: &JSContext, is_frozen: bool) -> Result<bool, i32> {
        const NOT_SEALED_ERR: i32 = i32::MAX;

        if let Err(e) =
            self.own_property_names_foreach(ctx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK, |_, pk| {
                if let Some(desc) = self.get_own_property(ctx, &pk.atom)?
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

    #[inline]
    pub fn downgrade(&self) -> JSObjectWeakref {
        JSObjectWeakref(Rc::downgrade(&self.0))
    }

    #[cfg(debug_assertions)]
    pub fn ref_count(&self) -> usize {
        Rc::strong_count(&self.0)
    }

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
pub struct JSObjectWeakref(pub(crate) Weak<ObjectInner>);

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
    pub const fn as_object(&self) -> Option<&JSObject> {
        if let Self::Object(ob) = self {
            Some(ob)
        } else {
            None
        }
    }

    #[inline]
    pub unsafe fn as_object_unchecked(&self) -> &JSObject {
        if let Self::Object(ob) = self {
            ob
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
    pub fn objectify(self, ctx: &JSContext) -> Result<JSObject, i32> {
        match self {
            Self::Object(o) => Ok(o),
            Self::String(s) => s.objectify(ctx, &JSValue::UNDEFINED),

            Self::Int(_) | Self::Float64(_) => ctx
                .create_object(ObjectPayload::NUMBER(self))
                .map_err(|_| -1),
            Self::Bool(_) => ctx
                .create_object(ObjectPayload::BOOLEAN(self))
                .map_err(|_| -1),
            Self::Symbol(_) => ctx
                .create_object(ObjectPayload::SYMBOL(self))
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
        if let Self::Object(ob) = self {
            ob.flags().is_constructor()
        } else {
            false
        }
    }

    #[inline]
    pub fn prevent_extensions(&self, ctx: &JSContext) -> Result<bool, i32> {
        self.as_object()
            .map_or(Ok(false), |o| o.prevent_extensions(ctx))
    }

    /// Seal `self` if it is an object.
    pub fn seal(&self, ctx: &JSContext, freeze: bool) -> Result<(), i32> {
        if let JSValue::Object(p) = &self {
            p.seal(ctx, freeze)
        } else {
            Ok(())
        }
    }

    /// check if value is sealed. Object value will check property, Non-object value is always treated as sealed.
    pub fn is_sealed(&self, ctx: &JSContext, is_frozen: bool) -> Result<bool, i32> {
        if let Some(o) = self.as_object() {
            o.is_sealed(ctx, is_frozen)
        } else {
            Ok(true)
        }
    }
}

pub const JS_PROP_INITIAL_SIZE: u32 = 2;
pub const JS_ARRAY_INITIAL_SIZE: u32 = 2;

#[cfg(target_pointer_width = "64")]
pub const JS_PROP_INITIAL_HASH_SIZE: usize = 8;
#[cfg(not(target_pointer_width = "64"))]
pub const JS_PROP_INITIAL_HASH_SIZE: usize = 4;

/// Note: all the fields are already sealed except length
pub fn seal_template_obj(ctx: &JSContext, obj: JSValue) -> i32 {
    let p = unsafe { obj.as_object_unchecked() };

    let (_, cur_flags) = p
        .find_shape_property(JS_ATOM_length.deref())
        .expect("length property must exist");

    if p.update_shape_property_flags(
        ctx,
        JS_ATOM_length.deref(),
        cur_flags & !(JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE),
    )
    .is_err()
    {
        return -1;
    }

    p.set_flag_extensible(false);
    0
}

pub fn JS_SpeciesConstructor(
    ctx: &JSContext,
    obj: &JSValue,
    default_constructor: JSValue,
) -> JSResult {
    if let Some(o) = obj.as_object() {
        let ctor = o.get_property(ctx, JS_ATOM_constructor.deref())?;

        if unlikely(ctor.is_undefined()) {
            Ok(default_constructor)
        } else if let Some(o1) = ctor.as_object() {
            let species = o1.get_property(ctx, JS_ATOM_Symbol_species.deref())?;

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
