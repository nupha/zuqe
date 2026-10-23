// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::Atom,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{VarRef, prop_str_to_atom},
    object::{LazyInit, Object, ObjectImpl},
    utils::unlikely,
    value::JSValue,
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
/// Throw error on failed operation, regardless of strict mode.
///
/// Unified rule: use ONLY in the define/create/delete family, which per ECMA
/// are mode-agnostic always-throw operations (`DefinePropertyOrThrow`,
/// `DeletePropertyOrThrow`, `Set(..., true)`). Never on generic set/assignment.
pub const JS_PROP_THROW: u32 = 1 << 14;
/// Throw error only if the current frame is strict.
///
/// Unified rule: use for every generic set/assignment (PutValue / `[[Set]]`),
/// where ECMA leaves the throw decision to the assignment expression, i.e.
/// sloppy mode silently fails while strict mode throws.
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
/// Include index(int) or string properties
pub const JS_GPN_STR_OR_INT_MASK: i32 = 1 << 0;
/// Include symbol properties
pub const JS_GPN_SYMBOL_MASK: i32 = 1 << 1;
/// Include private properties
pub const JS_GPN_PRIVATE_MASK: i32 = 1 << 2;
/// Only include enumerable properties
pub const JS_GPN_ENUM_ONLY: i32 = 1 << 4;
/// set theJSPropertyEnum.is_enumerable field
pub const JS_GPN_SET_ENUM: i32 = 1 << 5;

/// Typed throw policy for the set family, replacing the opaque
/// `flags & JS_PROP_THROW` / `flags & JS_PROP_THROW_STRICT` tests.
/// One of the two knobs on every set-family entry point (the other
/// being `no_add: bool`).  See `JS_PROP_THROW` / `JS_PROP_THROW_STRICT`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThrowPolicy {
    /// Always throw on failure (`Set(..., true)` — mode-agnostic).
    Always,
    /// Throw only in strict mode (`PutValue` — mode-faithful).
    StrictOnly,
    /// Never throw (pure `Reflect.set` semantics).
    Never,
}

impl ThrowPolicy {
    /// Whether a failed operation should raise, given the current strictness.
    #[inline(always)]
    pub(crate) const fn can_throw(self, is_strict: bool) -> bool {
        matches!(self, Self::Always) || (matches!(self, Self::StrictOnly) && is_strict)
    }

    /// Reconstruct from a legacy flags bitmask (exotic-callback ABI).
    pub(crate) const fn from_flags(flags: u32) -> (Self, bool) {
        let throw = if flags & crate::object::JS_PROP_THROW != 0 {
            Self::Always
        } else if flags & crate::object::JS_PROP_THROW_STRICT != 0 {
            Self::StrictOnly
        } else {
            Self::Never
        };
        (throw, flags & crate::object::JS_PROP_NO_ADD != 0)
    }
}

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
            if unlikely(!(getter.is_undefined() || getter.is_callable())) {
                return Err(ctx.throw_type_error("invalid getter"));
            }
        }

        if desc.has_property(ctx, Atom::CONST_set)? {
            flags |= JS_PROP_HAS_SET;
            setter = desc.get_property(ctx, Atom::CONST_set)?;
            if unlikely(!(setter.is_undefined() || setter.is_callable())) {
                return Err(ctx.throw_type_error("invalid setter"));
            }
        }

        if unlikely(
            flags & (JS_PROP_HAS_SET | JS_PROP_HAS_GET) != 0
                && flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE) != 0,
        ) {
            Err(ctx.throw_type_error("cannot have setter/getter and value or writable"))
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
            Self::parse(ctx, o)
        } else {
            Err(ctx.throw_type_error_not_an_object())
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

/// The variant tags of [`JSProperty`] — the contract with the JIT (the property inline guard).
///
/// The inline property guard reads the tag byte of a slot and compares it
/// with [`PROP_TAG_NORMAL`]: a slot that carries anything else is not a data
/// property (it is an accessor, a variable reference, a lazy initializer or a
/// tombstone) and the guard must decline it. The enum's discriminants *are*
/// these constants, so the two cannot drift apart.
pub(crate) const PROP_TAG_NORMAL: u8 = 0;
pub(crate) const PROP_TAG_VARREF: u8 = 1;
pub(crate) const PROP_TAG_GETSET: u8 = 2;
pub(crate) const PROP_TAG_LAZYINIT: u8 = 3;
pub(crate) const PROP_TAG_DELETED: u8 = 4;

/// Property content
///
/// The layout is a contract with the JIT: the inline property guard
/// reads the variant's tag and, for a `Normal` slot, the value — at fixed
/// offsets. `#[repr(C, u8)]` puts the tag in a byte at offset 0 and the
/// variant's payload right after it (at the union's alignment), so the
/// offsets are the same on every target. The offsets are published by
/// [`JSProperty::tag_offset`], [`JSProperty::normal_value_offset`] and
/// [`JSProperty::slot_size`], and pinned by this module's tests.
#[repr(C, u8)]
#[derive(Debug)]
pub enum JSProperty {
    Normal(JSValue) = PROP_TAG_NORMAL,
    VarRef(VarRef) = PROP_TAG_VARREF,

    /// TODO: use Box<PropertyGetSet>
    GetSet {
        getter: Option<Object>,
        setter: Option<Object>,
    } = PROP_TAG_GETSET,

    /// TODO: use Box<PropertyLazy>
    LazyInit {
        realm: ContextPtr,
        init: LazyInit,
    } = PROP_TAG_LAZYINIT,

    Deleted = PROP_TAG_DELETED,
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
            Self::LazyInit {
                realm,
                init: init_type,
            } => {
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
    /// The offset of the variant's tag; the JIT's inline property guard
    /// reads it to tell a data property from an accessor or a tombstone.
    pub(crate) const fn tag_offset() -> i32 {
        0
    }

    /// The offset of the `JSValue` inside a `Normal` slot.
    ///
    /// The tag is a byte, the payload follows at the union's alignment (8).
    pub(crate) const fn normal_value_offset() -> i32 {
        8
    }

    /// The size of one slot.
    pub(crate) const fn slot_size() -> i64 {
        std::mem::size_of::<Self>() as i64
    }

    /// retrive property value
    pub(crate) fn value(&self, ctx: &mut ScopedContext, this_obj: JSValue) -> JSResult {
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
    pub fn is_extensible(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
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

    /// Test if property `prop` exists (UTF-8 key), checking the proto chain.
    ///
    /// The key is resolved with `prop_str_to_atom`, which takes the
    /// allocation-free `Atom::lookup_str` fast path for interned keys.
    #[inline]
    pub fn has_property_str(&self, ctx: &mut ScopedContext, prop: &str) -> Result<bool, i32> {
        let atom = prop_str_to_atom(ctx, prop)?;
        self.has_property(ctx, atom)
    }

    /// test if property `prop` exists, check along proto chain.
    pub fn has_property(&self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        for mut cur_obj in self.shape_proto_iter(true) {
            if cur_obj.flags().is_exotic()
                && let Some(cb) = ctx
                    .rt()
                    .class_def(cur_obj.class_id())
                    .unwrap()
                    .exotic
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
        mut source: Object,
        excluded: Option<Object>,
        setprop: bool,
    ) -> Result<(), i32> {
        let mut gpn_flags = JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_ENUM_ONLY;
        if source.flags().is_exotic()
            && ctx
                .rt()
                .class_def(source.class_id())
                .unwrap()
                .exotic
                .and_then(|x| x.get_own_property_names)
                .is_some()
        {
            // cannot use JS_GPN_ENUM_ONLY with e.g. proxies because it
            // introduces a visible change
            gpn_flags &= !JS_GPN_ENUM_ONLY;
        }

        for k in source.get_own_property_names(ctx, gpn_flags)? {
            if let Some(mut excl) = excluded {
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
                // Object.assign: `Perform ? Set(to, nextKey, propValue, true)`.
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
        if let Some(p) = self.as_object() {
            p.is_extensible(ctx)
        } else {
            Ok(false)
        }
    }

    pub fn has_property(&self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        if let Some(p) = self.as_object() {
            p.has_property(ctx, prop)
        } else {
            Ok(false)
        }
    }

    /// Check whether a property exists by UTF-8 key (no intermediate JS string).
    #[inline]
    pub fn has_property_str(&self, ctx: &mut ScopedContext, prop: &str) -> Result<bool, i32> {
        let atom = prop_str_to_atom(ctx, prop)?;
        self.has_property(ctx, atom)
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
        if let Some(src_obj) = source.as_object() {
            if let Some(mut o) = self.as_object() {
                o.copy_data_properties_from(ctx, src_obj, excluded.as_object(), setprop)
            } else if let Some(mut p) = self.get_prototype_primitive(ctx).as_object() {
                p.copy_data_properties_from(ctx, src_obj, excluded.as_object(), setprop)
            } else {
                Err(ctx.throw_type_error_not_an_object())
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
            // not writable: cannot set the writable bit
            if (flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE))
                == (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
            {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout the JIT's inline property guard reads: the variant's
    /// tag, and the value inside a `Normal` slot.
    ///
    /// The offsets are fixed by `#[repr(C, u8)]`, but *which* variant carries
    /// which tag is the engine's business — so it is pinned here, for every
    /// variant: a guard that reads the wrong tag would treat an accessor as a
    /// data property (and return the getter's slot instead of calling it).
    #[test]
    fn the_property_slot_layout_is_stable() {
        // The tag is a byte at the slot's start.
        assert_eq!(JSProperty::tag_offset(), 0);

        // The published tags: the guard compares the tag byte against
        // `PROP_TAG_NORMAL`, so the values are part of the contract.
        assert_eq!(PROP_TAG_NORMAL, 0);
        assert_eq!(PROP_TAG_VARREF, 1);
        assert_eq!(PROP_TAG_GETSET, 2);
        assert_eq!(PROP_TAG_LAZYINIT, 3);
        assert_eq!(PROP_TAG_DELETED, 4);

        // The tag byte of a slot, read the way the guard reads it.
        let tag_of = |prop: &JSProperty| unsafe {
            *((prop as *const JSProperty).cast::<u8>()).add(JSProperty::tag_offset() as usize)
        };

        // A `Normal` slot's value: the `JSValue`, 8 bytes in, 8 bytes wide.
        let value = JSValue::from(42i32);
        let prop = JSProperty::Normal(value);
        let raw = unsafe {
            *((&prop as *const JSProperty).cast::<u8>())
                .add(JSProperty::normal_value_offset() as usize)
                .cast::<u64>()
        };
        assert_eq!(raw, value.0, "the value is not where the JIT reads it");

        // The widths the generated code assumes: a byte of tag, the value
        // 8 bytes in, and a slot the size the guard's stride assumes.
        assert_eq!(JSProperty::normal_value_offset(), 8);
        assert_eq!(std::mem::size_of::<JSValue>(), 8);
        assert_eq!(std::mem::size_of::<JSProperty>(), 48);
        assert_eq!(std::mem::align_of::<JSProperty>(), 8);
        assert_eq!(JSProperty::slot_size(), 48);
        assert!(
            (JSProperty::normal_value_offset() as usize + std::mem::size_of::<JSValue>())
                <= std::mem::size_of::<JSProperty>(),
            "the value does not fit in the slot"
        );

        // Every variant's tag, read from a real slot. The `VarRef` and the
        // `LazyInit` need a runtime to exist, so one is built here (and the
        // checks stay inside the scope: a `VarRef` is a GC node and may not
        // leave it).
        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(crate::ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        ctx.with_new_scope(|scope| {
            let var_ref = VarRef::create(scope, JSValue::from(1i32)).unwrap();
            let lazy_init = JSProperty::LazyInit {
                realm: scope.ctx_ptr,
                init: LazyInit::Prototype,
            };
            assert_eq!(
                tag_of(&JSProperty::VarRef(var_ref)),
                PROP_TAG_VARREF,
                "a variable reference must not look like a data property"
            );
            assert_eq!(
                tag_of(&lazy_init),
                PROP_TAG_LAZYINIT,
                "a lazy initializer must not look like a data property"
            );
        });

        assert_eq!(
            tag_of(&JSProperty::Normal(JSValue::UNDEFINED)),
            PROP_TAG_NORMAL,
            "the data property's tag is not where the JIT expects it"
        );
        assert_eq!(
            tag_of(&JSProperty::GetSet {
                getter: None,
                setter: None,
            }),
            PROP_TAG_GETSET,
            "an accessor must not look like a data property"
        );
        assert_eq!(
            tag_of(&JSProperty::Deleted),
            PROP_TAG_DELETED,
            "a tombstone must not look like a data property"
        );
    }
}
