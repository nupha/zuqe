use std::ffi::c_void;

use crate::{
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_PROXY, JS_CLASS_UINT8C_ARRAY,
    atom::{__JS_AtomToUInt32, JS_AtomIsNumericIndex, JS_FreeAtom, JSAtom, consts::*},
    context::{JSContext, JSContextWeakref},
    error::JS_ThrowReferenceErrorUninitialized,
    function::{JS_Call, JSVarRef},
    proxy::js_proxy_isExtensible,
    runtime::JSRuntime,
    value::{
        JS_ToLengthFree, JS_ToUint32Free, JSValue,
        object::{JSObject, JSShapeProperty, LazyInit},
    },
};

// Property flags
/// Indicates if a property can be modified or deleted
pub const JS_PROP_CONFIGURABLE: u32 = 1 << 0;
/// Indicates if a property's value can be changed
pub const JS_PROP_WRITABLE: u32 = 1 << 1;
/// Indicates if a property appears in enumeration of object's properties
pub const JS_PROP_ENUMERABLE: u32 = 1 << 2;
/// Combined flag for configurable, writable and enumerable properties
pub const JS_PROP_C_W_E: u32 = JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE | JS_PROP_ENUMERABLE;
/// Special flag for length property
pub const JS_PROP_LENGTH: u32 = 1 << 3;
/// Mask for property type
pub const JS_PROP_TMASK: u32 = 3 << 4;
/// Normal property type
pub const JS_PROP_NORMAL: u32 = 0 << 4;
/// Getter/setter property type
pub const JS_PROP_GETSET: u32 = 1 << 4;
/// Variable reference property type
pub const JS_PROP_VARREF: u32 = 2 << 4;
/// Auto-initialized property type
pub const JS_PROP_AUTOINIT: u32 = 3 << 4;

// Property definition flags
/// Base shift for property definition flags
pub const JS_PROP_HAS_SHIFT: u32 = 8;
/// Property has configurable attribute
pub const JS_PROP_HAS_CONFIGURABLE: u32 = 1 << 8;
/// Property has writable attribute
pub const JS_PROP_HAS_WRITABLE: u32 = 1 << 9;
/// Property has enumerable attribute
pub const JS_PROP_HAS_ENUMERABLE: u32 = 1 << 10;
/// Property has getter function
pub const JS_PROP_HAS_GET: u32 = 1 << 11;
/// Property has setter function
pub const JS_PROP_HAS_SET: u32 = 1 << 12;
/// Property has value
pub const JS_PROP_HAS_VALUE: u32 = 1 << 13;
/// Throw error on failed operation
pub const JS_PROP_THROW: u32 = 1 << 14;
/// Throw error in strict mode
pub const JS_PROP_THROW_STRICT: u32 = 1 << 15;
/// Prevent adding new properties
pub const JS_PROP_NO_ADD: u32 = 1 << 16;
/// Prevent exotic object behavior
pub const JS_PROP_NO_EXOTIC: u32 = 1 << 17;

// Property types
/// Regular identifier property
pub const PROP_TYPE_IDENT: i32 = 0;
/// Variable property
pub const PROP_TYPE_VAR: i32 = 1;
/// Getter property
pub const PROP_TYPE_GET: i32 = 2;
/// Setter property
pub const PROP_TYPE_SET: i32 = 3;
/// Generator property
pub const PROP_TYPE_STAR: i32 = 4;
/// Async property
pub const PROP_TYPE_ASYNC: i32 = 5;
/// Async generator property
pub const PROP_TYPE_ASYNC_STAR: i32 = 6;
/// Private property
pub const PROP_TYPE_PRIVATE: i32 = 1 << 4;

// Get property names flags
/// Include string properties
pub const JS_GPN_STRING_MASK: i32 = 1 << 0;
/// Include symbol properties
pub const JS_GPN_SYMBOL_MASK: i32 = 1 << 1;
/// Include private properties
pub const JS_GPN_PRIVATE_MASK: i32 = 1 << 2;
/// Only include enumerable properties
pub const JS_GPN_ENUM_ONLY: i32 = 1 << 4;
/// Set the is_enumerable field in JSPropertyEnum
pub const JS_GPN_SET_ENUM: i32 = 1 << 5;

#[derive(Debug, Clone)]
pub struct JSPropertyDescriptor {
    pub flags: u32,
    pub value: JSValue,
    pub getter: JSValue,
    pub setter: JSValue,
}

impl Default for JSPropertyDescriptor {
    fn default() -> Self {
        Self {
            flags: 0,
            value: JSValue::UNDEFINED,
            getter: JSValue::UNDEFINED,
            setter: JSValue::UNDEFINED,
        }
    }
}

impl JSPropertyDescriptor {
    /// Parse an object to JSPropertyDescriptor.
    pub(crate) fn parse(ctx: &JSContext, desc: &JSObject) -> Result<Self, i32> {
        let mut flags: u32 = 0;
        let mut value: JSValue = JSValue::UNDEFINED;
        let mut getter: JSValue = JSValue::UNDEFINED;
        let mut setter: JSValue = JSValue::UNDEFINED;

        if desc.has_property(ctx, JS_ATOM_configurable)? {
            let conf = desc
                .get_property(ctx, JS_ATOM_configurable)
                .check_exception()?;
            flags |= JS_PROP_HAS_CONFIGURABLE;
            if conf.coerce_to_bool(ctx)? {
                flags |= JS_PROP_CONFIGURABLE;
            }
        }

        if desc.has_property(ctx, JS_ATOM_writable)? {
            let conf = desc.get_property(ctx, JS_ATOM_writable).check_exception()?;
            flags |= JS_PROP_HAS_WRITABLE;
            if conf.coerce_to_bool(ctx)? {
                flags |= JS_PROP_WRITABLE;
            }
        }

        if desc.has_property(ctx, JS_ATOM_enumerable)? {
            let conf = desc
                .get_property(ctx, JS_ATOM_enumerable)
                .check_exception()?;
            flags |= JS_PROP_HAS_ENUMERABLE;
            if conf.coerce_to_bool(ctx)? {
                flags |= JS_PROP_ENUMERABLE;
            }
        }

        if desc.has_property(ctx, JS_ATOM_value)? {
            flags |= JS_PROP_HAS_VALUE;
            value = desc.get_property(ctx, JS_ATOM_value).check_exception()?;
        }

        if desc.has_property(ctx, JS_ATOM_get)? {
            flags |= JS_PROP_HAS_GET;
            getter = desc.get_property(ctx, JS_ATOM_get);
            if getter.is_exception() || !(getter.is_undefined() || getter.is_callable()) {
                ctx.JS_ThrowTypeError("invalid getter");
                return Err(-1);
            }
        }

        if desc.has_property(ctx, JS_ATOM_set)? {
            flags |= JS_PROP_HAS_SET;
            setter = desc.get_property(ctx, JS_ATOM_set);
            if setter.is_exception() || !(setter.is_undefined() || setter.is_callable()) {
                ctx.JS_ThrowTypeError("invalid setter");
                return Err(-1);
            }
        }

        if flags & (JS_PROP_HAS_SET | JS_PROP_HAS_GET) != 0
            && flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE) != 0
        {
            ctx.JS_ThrowTypeError("cannot have setter/getter and value or writable");
            return Err(-1);
        }

        Ok(Self {
            flags,
            value,
            getter,
            setter,
        })
    }
}

/// the property content
#[repr(u8)]
#[derive(Debug, Clone)]
pub enum JSProperty {
    Normal(JSValue),
    GetSet {
        getter: Option<JSObject>,
        setter: Option<JSObject>,
    },
    VarRef(JSVarRef),
    /// TODO: use Box<PropertyInit> instead
    AutoInit {
        realm: JSContextWeakref,
        init_type: LazyInit,
        opaque: *mut c_void,
    },
}

impl Default for JSProperty {
    fn default() -> Self {
        Self::Normal(JSValue::UNDEFINED)
    }
}

impl JSProperty {
    /// get property value
    pub fn value(&self, ctx: &JSContext, this_obj: &JSValue) -> JSValue {
        match self {
            Self::Normal(val) => val.clone(),
            Self::GetSet { getter, .. } => {
                if let Some(fun) = getter {
                    JS_Call(ctx, &JSValue::Object(fun.clone()), this_obj, &[])
                } else {
                    JSValue::UNDEFINED
                }
            }
            Self::VarRef(vref) => vref.value(),
            Self::AutoInit { .. } => JSValue::UNDEFINED,
        }
    }

    /// get realm of an auto init property.
    /// for other property types, returns None.
    pub(crate) fn auto_init_realm(&self) -> Option<JSContext> {
        if let JSProperty::AutoInit { realm, .. } = self {
            realm.upgrade()
        } else {
            None
        }
    }
}

#[deprecated(note = "use JSValue::has_property() instead")]
pub unsafe fn JS_HasProperty(ctx: &JSContext, obj: JSValue, prop: JSAtom) -> Result<bool, i32> {
    obj.has_property(ctx, prop)
}

impl JSObject {
    pub fn is_extensible(&self, ctx: &JSContext) -> Result<bool, i32> {
        if self.class_id() == JS_CLASS_PROXY {
            js_proxy_isExtensible(ctx, &JSValue::Object(self.clone()))
        } else {
            Ok(self.flags().extensible())
        }
    }

    pub fn has_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
        let mut p = self.clone();

        loop {
            if p.flags().is_exotic() {
                if let Some(cb) = unsafe {
                    ctx.rt().class_defs[p.class_id() as usize]
                        .exotic
                        .as_ref()
                        .and_then(|x| x.has_property)
                } {
                    return match cb(ctx, &p, prop) {
                        -1 => Err(-1),
                        n => Ok(n != 0),
                    };
                }
            }

            match p.get_own_property(ctx, prop)? {
                None => {
                    if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&p.class_id()) {
                        match unsafe { JS_AtomIsNumericIndex(ctx, prop) } {
                            0 => {}
                            -1 => {
                                return Err(-1);
                            }
                            _ => {
                                return Ok(false);
                            }
                        }
                    }

                    // walk up proto
                    let up = p.shape().unwrap().0.borrow().proto.clone();
                    if let Some(up) = up {
                        p = up;
                    } else {
                        break;
                    }
                }
                Some(_) => return Ok(true),
            }
        }

        Ok(false)
    }

    /// get description of own property.
    /// Returns None if own property does not exist.
    pub fn get_own_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
            let mut desc = JSPropertyDescriptor {
                flags: prs_flags & JS_PROP_C_W_E,
                ..Default::default()
            };

            match prs_flags & JS_PROP_TMASK {
                JS_PROP_NORMAL => {
                    desc.value = self.0.prop_values.borrow()[idx].value(ctx, &JSValue::UNDEFINED);
                }
                JS_PROP_GETSET => {
                    desc.flags |= JS_PROP_GETSET;
                    if let JSProperty::GetSet { getter, setter } =
                        self.0.prop_values.borrow()[idx].clone()
                    {
                        desc.getter = getter.map_or(JSValue::UNDEFINED, JSValue::Object);
                        desc.setter = setter.map_or(JSValue::UNDEFINED, JSValue::Object);
                    } else {
                        unreachable!()
                    }
                }
                JS_PROP_VARREF => {
                    desc.value = self.0.prop_values.borrow()[idx].value(ctx, &JSValue::UNDEFINED);
                    if desc.value.is_uninitialized() {
                        JS_ThrowReferenceErrorUninitialized(ctx, prop);
                        return Err(-1);
                    }
                }
                JS_PROP_AUTOINIT => {
                    // Instantiate property
                    let mut pr = std::mem::take(&mut self.0.prop_values.borrow_mut()[idx]);
                    desc.value = pr.instantiate(self, prop, self)?;
                    debug_assert!(matches!(pr, JSProperty::Normal(_)));
                    debug_assert_eq!(
                        self.find_shape_property(prop).unwrap().1 & JS_PROP_TMASK,
                        JS_PROP_NORMAL
                    );
                    self.0.prop_values.borrow_mut()[idx] = pr;
                }
                _ => {}
            }

            return Ok(Some(desc));
        }

        let flags = self.flags();
        if flags.is_exotic() {
            if flags.fast_array() {
                // specific case for fast arrays
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < self.array_length_unchecked() as u32 {
                        return Ok(Some(JSPropertyDescriptor {
                            flags: JS_PROP_WRITABLE | JS_PROP_ENUMERABLE | JS_PROP_CONFIGURABLE,
                            value: JSValue::Object(self.clone()).get_property_u32(ctx, idx),
                            getter: JSValue::UNDEFINED,
                            setter: JSValue::UNDEFINED,
                        }));
                    }
                }
            } else {
                let em = ctx.rt().class_defs[self.class_id() as usize].exotic;
                if !em.is_null() {
                    if let Some(cb) = unsafe { (*em).get_own_property } {
                        return cb(ctx, JSValue::Object(self.clone()), prop);
                    }
                }
            }
        }

        Ok(None)
    }
}

impl JSValue {
    /// return Err() if exception (Proxy object only) or TRUE/FALSE
    pub fn is_extensible(&self, ctx: &JSContext) -> Result<bool, i32> {
        if let Self::Object(p) = self {
            p.is_extensible(ctx)
        } else {
            Ok(false)
        }
    }

    pub fn has_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
        if let Self::Object(p) = self {
            p.has_property(ctx, prop)
        } else {
            Ok(false)
        }
    }

    /// get description of own property.
    pub fn get_own_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        if let JSValue::Object(p) = self {
            p.get_own_property(ctx, prop)
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }

    /// copy and assign enumerable properties from `source` value to `self`.
    /// equivelent to js `Object::assign(self, source)`
    pub fn copy_properties_from(
        &self,
        ctx: &JSContext,
        source: &Self,
        excluded: &Self,
        setprop: bool,
    ) -> Result<(), i32> {
        if let JSValue::Object(src_obj) = source {
            let pexcl = if let JSValue::Object(p) = excluded {
                Some(p)
            } else {
                None
            };

            let mut gpn_flags = JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_ENUM_ONLY;
            if src_obj.flags().is_exotic()
                && unsafe {
                    ctx.rt().class_defs[src_obj.class_id() as usize]
                        .exotic
                        .as_ref()
                }
                .and_then(|x| x.get_own_property_names)
                .is_some()
            {
                // cannot use JS_GPN_ENUM_ONLY with e.g. proxies because it
                // introduces a visible change
                gpn_flags &= !JS_GPN_ENUM_ONLY;
            }

            src_obj.own_property_names_foreach(ctx, gpn_flags, |cx, name| -> Result<(), i32> {
                if let Some(excl) = pexcl {
                    if excl.get_own_property(ctx, name.atom)?.is_some() {
                        return Ok(()); // excluded prop, skip
                    }
                }

                if gpn_flags & JS_GPN_ENUM_ONLY == 0 {
                    if let Some(desc) = src_obj.get_own_property(ctx, name.atom)? {
                        if desc.flags & JS_PROP_ENUMERABLE == 0 {
                            return Ok(()); // skip if non enumerable
                        }
                    } else {
                        return Ok(()); // skip if prop does not exsit
                    }
                }

                let val = source.get_property(cx, name.atom).check_exception()?;
                if setprop {
                    self.set_property(cx, name.atom, val)?;
                } else {
                    self.define_property_value(cx, name.atom, val, JS_PROP_C_W_E)?;
                }

                Ok(())
            })
        } else {
            Ok(())
        }
    }
}

#[deprecated(note = "use JSProperty::free() instead")]
pub unsafe fn free_property(rt: *mut JSRuntime, pr: *mut JSProperty, prop_flags: u32) {
    unreachable!("use JSProperty::free() instead");
}

/// Computes the effective property flags by combining input flags with default flags
/// If a flag is specified in JS_PROP_HAS_x, it takes precedence over def_flags
#[inline(always)]
pub const fn get_prop_flags(flags: u32, def_flags: u32) -> u32 {
    let mask = (flags >> JS_PROP_HAS_SHIFT) & JS_PROP_C_W_E;
    (flags & mask) | (def_flags & !mask)
}

/// Finds a property in an object by its atom name
#[deprecated(note = "use JSObject::find_own_property1() instead")]
pub unsafe fn find_own_property1(p: *mut JSValue, atom: JSAtom) -> *mut JSShapeProperty {
    unreachable!("unused");
}

/// Finds a property and its value in an object by atom name
/// Sets ppr to the property value pointer if found, null otherwise
/// Returns the property shape pointer if found, null otherwise
#[inline(always)]
#[deprecated(note = "use JSObject::find_own_property() instead")]
pub unsafe fn find_own_property(
    ppr: *mut *mut JSProperty,
    p: *mut JSValue,
    atom: JSAtom,
) -> *mut JSShapeProperty {
    unreachable!("unused");
}

/// Checks if property flags are valid for property definition
/// Returns false if the flags are invalid
pub const unsafe fn check_define_prop_flags(prop_flags: u32, flags: u32) -> bool {
    let has_accessor;
    let is_getset;

    if (prop_flags & JS_PROP_CONFIGURABLE) == 0 {
        if (flags & (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE))
            == (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE)
        {
            return false;
        }
        if (flags & JS_PROP_HAS_ENUMERABLE) != 0
            && (flags & JS_PROP_ENUMERABLE) != (prop_flags & JS_PROP_ENUMERABLE)
        {
            return false;
        }
    }
    if (flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)) != 0
        && prop_flags & JS_PROP_CONFIGURABLE == 0
    {
        has_accessor = (flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET)) != 0;
        is_getset = (prop_flags & JS_PROP_TMASK) == JS_PROP_GETSET;
        if has_accessor != is_getset {
            return false;
        }
        if !has_accessor && !is_getset && (prop_flags & JS_PROP_WRITABLE) == 0 {
            /* not writable: cannot set the writable bit */
            if (flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE))
                == (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
            {
                return false;
            }
        }
    }
    true
}

/// Get object `length` property as u32
pub fn js_get_length32(ctx: &JSContext, obj: &JSValue) -> Result<u32, i32> {
    unsafe {
        JS_ToUint32Free(
            ctx,
            obj.get_property(ctx, JS_ATOM_length).check_exception()?,
        )
    }
}

/// Get object `length` property as i64
pub fn js_get_length64(ctx: &JSContext, obj: &JSValue) -> Result<i64, i32> {
    let len_val = obj.get_property(ctx, JS_ATOM_length);
    if len_val.is_exception() {
        Err(-1)
    } else {
        unsafe { JS_ToLengthFree(ctx, len_val) }
    }
}

/* Note: the property value is not initialized. Return NULL if memory
error. */
#[deprecated(note = "use JSObject::add_property() instead")]
pub unsafe fn add_property(
    ctx: &JSContext,
    p: *mut JSValue,
    prop: JSAtom,
    prop_flags: u32,
) -> *mut JSProperty {
    unreachable!("unused")
}
