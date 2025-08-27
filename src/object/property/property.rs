use std::ffi::c_void;

use crate::{
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_PROXY, JS_CLASS_UINT8C_ARRAY,
    atom::{JS_AtomIsNumericIndex, JSAtom, consts::*},
    context::{JSContext, JSContextWeakref},
    error::JSResult,
    function::JSVarRef,
    object::{JSObject, LazyInit},
    proxy::js_proxy_isExtensible,
    value::{JS_GetPrototypePrimitive, JSValue},
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
pub const JS_PROP_LAZYINIT: u32 = 3 << 4;

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
/// Include array or string index properties
pub const JS_GPN_INDEX_MASK: i32 = 1 << 0;
#[deprecated(note = "renamed to JS_GPN_INDEX_MASK")]
pub const JS_GPN_STRING_MASK: i32 = 1 << 0;
/// Include symbol properties
pub const JS_GPN_SYMBOL_MASK: i32 = 1 << 1;
/// Include private properties
pub const JS_GPN_PRIVATE_MASK: i32 = 1 << 2;
/// Only include enumerable properties
pub const JS_GPN_ENUM_ONLY: i32 = 1 << 4;
/// set theJSPropertyEnum.is_enumerable field
#[deprecated(note = "has no effect")]
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
            let conf = desc.get_property(ctx, JS_ATOM_configurable)?;
            flags |= JS_PROP_HAS_CONFIGURABLE;
            if conf.to_bool()? {
                flags |= JS_PROP_CONFIGURABLE;
            }
        }

        if desc.has_property(ctx, JS_ATOM_writable)? {
            let conf = desc.get_property(ctx, JS_ATOM_writable)?;
            flags |= JS_PROP_HAS_WRITABLE;
            if conf.to_bool()? {
                flags |= JS_PROP_WRITABLE;
            }
        }

        if desc.has_property(ctx, JS_ATOM_enumerable)? {
            let conf = desc.get_property(ctx, JS_ATOM_enumerable)?;
            flags |= JS_PROP_HAS_ENUMERABLE;
            if conf.to_bool()? {
                flags |= JS_PROP_ENUMERABLE;
            }
        }

        if desc.has_property(ctx, JS_ATOM_value)? {
            flags |= JS_PROP_HAS_VALUE;
            value = desc.get_property(ctx, JS_ATOM_value)?;
        }

        if desc.has_property(ctx, JS_ATOM_get)? {
            flags |= JS_PROP_HAS_GET;
            getter = desc.get_property(ctx, JS_ATOM_get)?;
            if !(getter.is_undefined() || getter.is_callable()) {
                ctx.throw_type_error("invalid getter");
                return Err(-1);
            }
        }

        if desc.has_property(ctx, JS_ATOM_set)? {
            flags |= JS_PROP_HAS_SET;
            setter = desc.get_property(ctx, JS_ATOM_set)?;
            if !(setter.is_undefined() || setter.is_callable()) {
                ctx.throw_type_error("invalid setter");
                return Err(-1);
            }
        }

        if flags & (JS_PROP_HAS_SET | JS_PROP_HAS_GET) != 0
            && flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE) != 0
        {
            ctx.throw_type_error("cannot have setter/getter and value or writable");
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
    LazyInit {
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
    /// retrive property value
    pub fn value(&self, ctx: &JSContext, this_obj: &JSValue) -> JSResult {
        match self {
            Self::Normal(v) => {
                debug_assert!(!v.is_exception());
                Ok(v.clone())
            }
            Self::GetSet { getter, .. } => {
                if let Some(fun) = getter {
                    JSValue::Object(fun.clone()).call(ctx, this_obj, &[])
                } else {
                    Ok(JSValue::UNDEFINED)
                }
            }
            Self::VarRef(vref) => Ok(vref.value()),
            Self::LazyInit { .. } => Ok(JSValue::UNDEFINED),
        }
    }

    // /// get realm of a lazy init property.
    // /// for other property types, returns None.
    // pub(crate) fn auto_init_realm(&self) -> Option<JSContext> {
    //     if let JSProperty::LazyInit { realm, .. } = self {
    //         realm.upgrade()
    //     } else {
    //         None
    //     }
    // }
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

                    // walk up proto chain
                    if let Some(up) = p.shape().unwrap().0.borrow().proto.clone() {
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

    /// copy and assign enumerable properties from `source` value to `self`.
    /// equivelent to js `Object::assign(self, source)`
    pub fn copy_properties_from(
        &self,
        ctx: &JSContext,
        source: &Self,
        excluded: Option<&Self>,
        setprop: bool,
    ) -> Result<(), i32> {
        let mut gpn_flags = JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_ENUM_ONLY;

        if source.flags().is_exotic()
            && unsafe {
                ctx.rt().class_defs[source.class_id() as usize]
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

        source.own_property_names_foreach(ctx, gpn_flags, |cx, name| -> Result<(), i32> {
            if let Some(excl) = excluded {
                if excl.get_own_property(ctx, name.atom)?.is_some() {
                    return Ok(()); // excluded prop, skip
                }
            }

            if gpn_flags & JS_GPN_ENUM_ONLY == 0 {
                if let Some(desc) = source.get_own_property(ctx, name.atom)? {
                    if desc.flags & JS_PROP_ENUMERABLE == 0 {
                        return Ok(()); // skip if non enumerable
                    }
                } else {
                    return Ok(()); // skip if prop does not exsit
                }
            }

            let val = source.get_property(cx, name.atom)?;
            if setprop {
                self.set_property(cx, name.atom, val)?;
            } else {
                self.define_property_value(cx, name.atom, val, JS_PROP_C_W_E)?;
            }

            Ok(())
        })
    }

    /// Read _length_ property defined on `self`.
    /// if _length_ property is null or undefined, returns 0.
    ///
    /// Returned length is in range \[0, MAX_SAFE_INTEGER\]
    pub fn get_length_prop(&self, ctx: &JSContext) -> Result<i64, i32> {
        self.get_property(ctx, JS_ATOM_length)?.to_length(ctx)
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

    /// copy and assign enumerable properties from `source` value to `self`.
    /// equivelent to js `Object::assign(self, source)`
    pub fn copy_properties_from(
        &self,
        ctx: &JSContext,
        source: &Self,
        excluded: &Self,
        setprop: bool,
    ) -> Result<(), i32> {
        if let Self::Object(src_obj) = source {
            if let Self::Object(o) = self {
                o.copy_properties_from(ctx, src_obj, excluded.object(), setprop)
            } else {
                if let JSValue::Object(p) = JS_GetPrototypePrimitive(ctx, self) {
                    p.copy_properties_from(ctx, src_obj, excluded.object(), setprop)
                } else {
                    ctx.throw_type_error_not_an_object();
                    Err(-1)
                }
            }
        } else {
            Ok(())
        }
    }

    /// Read _length_ property defined on `self`.
    /// if _length_ property is null or undefined, returns 0.
    ///
    /// Returned length is in range \[0, MAX_SAFE_INTEGER\]
    pub fn get_length_prop(&self, ctx: &JSContext) -> Result<i64, i32> {
        self.get_property(ctx, JS_ATOM_length)?.to_length(ctx)
    }
}

/// Computes the effective property flags by combining input flags with default flags
/// If a flag is specified in JS_PROP_HAS_x, it takes precedence over def_flags
#[inline(always)]
pub const fn get_prop_flags(flags: u32, def_flags: u32) -> u32 {
    let mask = (flags >> JS_PROP_HAS_SHIFT) & JS_PROP_C_W_E;
    (flags & mask) | (def_flags & !mask)
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
#[deprecated(note = "use JSValue::get_length_prop() instead")]
pub fn js_get_length32(ctx: &JSContext, obj: &JSValue) -> Result<u32, i32> {
    obj.get_property(ctx, JS_ATOM_length)
        .and_then(|v| v.to_u32(ctx))
}

/// Get object `length` property as i64
#[deprecated(note = "use JSObject::get_length_prop() or JSValue::get_length_prop() instead")]
pub fn js_get_length64(ctx: &JSContext, obj: &JSValue) -> Result<i64, i32> {
    obj.get_length_prop(ctx)
}
