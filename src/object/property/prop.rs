// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::Atom,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::VarRef,
    object::{LazyInit, Object, ObjectImpl},
    utils::unlikely,
    value::{JS_GetPrototypePrimitive, JSValue},
};

// Property flags
/// Indicates if a property can be modified or deleted
pub const JS_PROP_CONFIGURABLE: u32 = 1;
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
/// Force to set/delete an existing property regardless of writable/configurable setting.
/// Be cautious, this is for experts only
pub const JS_PROP_FORCE: u32 = 1 << 18;

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
pub const JS_GPN_INDEX_MASK: i32 = 1 << 0; // a.k.a. JS_GPN_STRING_MASK
/// Include symbol properties
pub const JS_GPN_SYMBOL_MASK: i32 = 1 << 1;
/// Include private properties
pub const JS_GPN_PRIVATE_MASK: i32 = 1 << 2;
/// Only include enumerable properties
pub const JS_GPN_ENUM_ONLY: i32 = 1 << 4;
/// set theJSPropertyEnum.is_enumerable field
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
    pub(crate) fn parse(ctx: &mut ScopedContext, desc: Object) -> Result<Self, i32> {
        let mut flags: u32 = 0;
        let mut value: JSValue = JSValue::UNDEFINED;
        let mut getter: JSValue = JSValue::UNDEFINED;
        let mut setter: JSValue = JSValue::UNDEFINED;

        if desc.has_property(ctx, Atom::CONST_configurable)? {
            let conf = desc.get_property(ctx, Atom::CONST_configurable)?;
            flags |= JS_PROP_HAS_CONFIGURABLE;
            if conf.to_bool()? {
                flags |= JS_PROP_CONFIGURABLE;
            }
        }

        if desc.has_property(ctx, Atom::CONST_writable)? {
            let conf = desc.get_property(ctx, Atom::CONST_writable)?;
            flags |= JS_PROP_HAS_WRITABLE;
            if conf.to_bool()? {
                flags |= JS_PROP_WRITABLE;
            }
        }

        if desc.has_property(ctx, Atom::CONST_enumerable)? {
            let conf = desc.get_property(ctx, Atom::CONST_enumerable)?;
            flags |= JS_PROP_HAS_ENUMERABLE;
            if conf.to_bool()? {
                flags |= JS_PROP_ENUMERABLE;
            }
        }

        if desc.has_property(ctx, Atom::CONST_value)? {
            flags |= JS_PROP_HAS_VALUE;
            value = desc.get_property(ctx, Atom::CONST_value)?;
        }

        if desc.has_property(ctx, Atom::CONST_get)? {
            flags |= JS_PROP_HAS_GET;
            getter = desc.get_property(ctx, Atom::CONST_get)?;
            if unlikely(!(getter.is_undefined() || getter.is_function())) {
                ctx.throw_type_error("invalid getter");
                return Err(-1);
            }
        }

        if desc.has_property(ctx, Atom::CONST_set)? {
            flags |= JS_PROP_HAS_SET;
            setter = desc.get_property(ctx, Atom::CONST_set)?;
            if unlikely(!(setter.is_undefined() || setter.is_function())) {
                ctx.throw_type_error("invalid setter");
                return Err(-1);
            }
        }

        if unlikely(
            flags & (JS_PROP_HAS_SET | JS_PROP_HAS_GET) != 0
                && flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE) != 0,
        ) {
            ctx.throw_type_error("cannot have setter/getter and value or writable");
            Err(-1)
        } else {
            Ok(Self {
                flags,
                value,
                getter,
                setter,
            })
        }
    }

    /// Parse a value to JSPropertyDescriptor.
    #[inline]
    pub(crate) fn from_value(ctx: &mut ScopedContext, desc: JSValue) -> Result<Self, i32> {
        if let Some(o) = desc.as_object() {
            Self::parse(ctx, *o)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// build object from descriptor
    pub fn make_object(&self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        let mut ret = ctx.create_base_object()?;

        if self.flags & JS_PROP_HAS_GET != 0 {
            ret.define_property_value(ctx, Atom::CONST_get, self.getter, JS_PROP_C_W_E)?;
        }
        if self.flags & JS_PROP_HAS_SET != 0 {
            ret.define_property_value(ctx, Atom::CONST_set, self.setter, JS_PROP_C_W_E)?;
        }
        if self.flags & JS_PROP_HAS_VALUE != 0 {
            ret.define_property_value(ctx, Atom::CONST_value, self.value, JS_PROP_C_W_E)?;
        }
        if self.flags & JS_PROP_HAS_WRITABLE != 0 {
            ret.define_property_value(
                ctx,
                Atom::CONST_writable,
                (self.flags & JS_PROP_WRITABLE != 0).into(),
                JS_PROP_C_W_E,
            )?;
        }
        if self.flags & JS_PROP_HAS_ENUMERABLE != 0 {
            ret.define_property_value(
                ctx,
                Atom::CONST_enumerable,
                (self.flags & JS_PROP_ENUMERABLE != 0).into(),
                JS_PROP_C_W_E,
            )?;
        }
        if self.flags & JS_PROP_HAS_CONFIGURABLE != 0 {
            ret.define_property_value(
                ctx,
                Atom::CONST_configurable,
                (self.flags & JS_PROP_CONFIGURABLE != 0).into(),
                JS_PROP_C_W_E,
            )?;
        }

        Ok(ret)
    }
}

/// Property content
#[repr(u8)]
#[derive(Debug)]
pub enum JSProperty {
    Normal(JSValue),
    VarRef(VarRef),

    /// TODO: use Box<PropertyGetSet>
    GetSet {
        getter: Option<Object>,
        setter: Option<Object>,
    },

    /// TODO: use Box<PropertyLazy>
    LazyInit {
        realm: ContextPtr,
        init_type: LazyInit,
    },

    Deleted,
}

impl GcTrace for JSProperty {
    fn trace(&self, tr: &mut GcTraceCtx) {
        match self {
            Self::Normal(v) => {
                v.trace(tr);
            }
            Self::GetSet { getter, setter } => {
                if let Some(o) = getter {
                    o.trace(tr);
                }
                if let Some(o) = setter {
                    o.trace(tr);
                }
            }
            Self::VarRef(vr) => {
                vr.trace(tr);
            }
            Self::LazyInit { realm, init_type } => {
                realm.trace(tr);
                init_type.trace(tr);
            }
            Self::Deleted => {}
        }
    }
}

impl Default for JSProperty {
    fn default() -> Self {
        Self::Normal(JSValue::UNDEFINED)
    }
}

impl JSProperty {
    /// retrive property value
    pub(super) fn value(&self, ctx: &mut ScopedContext, this_obj: JSValue) -> JSResult {
        match self {
            Self::Normal(v) => {
                debug_assert!(!v.is_exception());
                Ok(*v)
            }
            Self::GetSet { getter, .. } => {
                if let Some(fun) = getter {
                    fun.call(ctx, this_obj, &[])
                } else {
                    Ok(JSValue::UNDEFINED)
                }
            }
            Self::VarRef(vref) => Ok(vref.value()),
            Self::LazyInit { .. } => Ok(JSValue::UNDEFINED),
            Self::Deleted => {
                unreachable!()
            }
        }
    }
}

impl ObjectImpl {
    /// check if object is extensible.
    /// in case `self` is a Proxy object, may return Err.
    pub fn is_extensible_obj(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        #[cfg(feature = "builtin_proxy")]
        {
            Object::from_ref(self).as_Proxy().map_or_else(
                || Ok(self.flags().extensible()),
                |p| p.is_extensible_obj(ctx),
            )
        }
        #[cfg(not(feature = "builtin_proxy"))]
        {
            Ok(self.flags().extensible())
        }
    }

    /// test if property `prop` exists, check along proto chain.
    pub fn has_property(&self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        for cur_obj in self.shape_proto_iter(true) {
            if cur_obj.flags().is_exotic()
                && let Some(cb) = ctx.rt_ptr().class_registry[cur_obj.class_id() as usize]
                    .exotic
                    .as_ref()
                    .and_then(|x| x.has_property)
            {
                return cb(ctx, cur_obj, prop);
            }

            if cur_obj.get_own_property(ctx, prop)?.is_some() {
                return Ok(true);
            }

            if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&cur_obj.class_id())
                && prop.is_numeric_index(ctx)?
            {
                return Ok(false);
            }
        }

        Ok(false)
    }

    /// copy and assign enumerable properties from `source` to `self`.
    /// equivelent to js `Object::assign(self, source)`
    pub(crate) fn copy_data_properties_from(
        &mut self,
        ctx: &mut ScopedContext,
        source: Object,
        excluded: Option<Object>,
        setprop: bool,
    ) -> Result<(), i32> {
        let mut gpn_flags = JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_ENUM_ONLY;
        if source.flags().is_exotic()
            && ctx.rt_ptr().class_registry[source.class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|x| x.get_own_property_names)
                .is_some()
        {
            // cannot use JS_GPN_ENUM_ONLY with e.g. proxies because it
            // introduces a visible change
            gpn_flags &= !JS_GPN_ENUM_ONLY;
        }

        for k in source.get_own_property_names(ctx, gpn_flags)? {
            if let Some(excl) = excluded {
                let skip = excl.get_own_property(ctx, k.atom).map(|x| x.is_some())?;
                if skip {
                    continue; // excluded prop, skip
                }
            }

            if gpn_flags & JS_GPN_ENUM_ONLY == 0 {
                if let Some(desc) = source.get_own_property(ctx, k.atom)? {
                    if desc.flags & JS_PROP_ENUMERABLE == 0 {
                        continue; // skip if non enumerable
                    }
                } else {
                    continue; // skip if prop does not exist
                }
            }

            let val = source.get_property(ctx, k.atom)?;
            if setprop {
                self.set_property(ctx, k.atom, val)?;
            } else {
                self.define_property_value(ctx, k.atom, val, JS_PROP_C_W_E)?;
            }
        }

        Ok(())
    }

    /// Read _length_ property defined on `self`.
    /// if _length_ property is null or undefined, returns 0.
    ///
    /// Returned length is in range \[0, MAX_SAFE_INTEGER\]
    pub fn get_length_prop(&self, ctx: &mut ScopedContext) -> Result<i64, i32> {
        self.get_property(ctx, Atom::CONST_length)?.to_length(ctx)
    }
}

impl JSValue {
    /// return Err() if exception (Proxy object only) or TRUE/FALSE
    pub fn is_extensible(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        if let Self::Object(p) = self {
            p.is_extensible_obj(ctx)
        } else {
            Ok(false)
        }
    }

    pub fn has_property(&self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        if let Self::Object(p) = self {
            p.has_property(ctx, prop)
        } else {
            Ok(false)
        }
    }

    /// copy and assign enumerable properties from `source` value to `self`.
    /// equivelent to js `Object::assign(self, source)`
    pub(crate) fn copy_data_properties_from(
        &mut self,
        ctx: &mut ScopedContext,
        source: &Self,
        excluded: &Self,
        setprop: bool,
    ) -> Result<(), i32> {
        if let Self::Object(src_obj) = source {
            if let Self::Object(o) = self {
                o.copy_data_properties_from(ctx, *src_obj, excluded.as_object().copied(), setprop)
            } else if let JSValue::Object(mut p) = JS_GetPrototypePrimitive(ctx, self) {
                p.copy_data_properties_from(ctx, *src_obj, excluded.as_object().copied(), setprop)
            } else {
                ctx.throw_type_error_not_an_object();
                Err(-1)
            }
        } else {
            Ok(())
        }
    }

    /// Read _length_ property defined on `self`.
    /// if _length_ property is null or undefined, returns 0.
    ///
    /// Returned length is in range \[0, MAX_SAFE_INTEGER\]
    pub fn get_length_prop(&self, ctx: &mut ScopedContext) -> Result<i64, i32> {
        self.get_property(ctx, Atom::CONST_length)?.to_length(ctx)
    }
}

/// Computes the effective property flags by combining input flags with default flags
/// If a flag is specified in JS_PROP_HAS_x, it takes precedence over def_flags
#[inline(always)]
pub(super) const fn get_prop_flags(flags: u32, def_flags: u32) -> u32 {
    let mask = (flags >> JS_PROP_HAS_SHIFT) & JS_PROP_C_W_E;
    (flags & mask) | (def_flags & !mask)
}

/// Checks if property flags are valid for property definition
/// Returns false if the flags are invalid
pub(crate) const fn check_define_prop_flags(prop_flags: u32, flags: u32) -> bool {
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
