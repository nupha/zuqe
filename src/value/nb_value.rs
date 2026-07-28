// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::{GcHead, GcNode, GcRef};

use crate::{
    Atom, IntOrFloat, JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING, JS_CLASS_SYMBOL,
    ModuleDef, ScopedContext,
    atom::AtomKind,
    function::{Bytecode, BytecodeImpl},
    module::ModuleDefImpl,
    number::{Float64Union, JS_DTOA_VAR_FORMAT, f64_stringify},
    object::{Object, ObjectImpl, ObjectPayload},
    string::{RopeImpl, StringImpl, StringRef},
    utils::hash_usize,
    value::HINT_STRING,
};

use super::tag::{JS_NAN_U64, JSValueTag, js_new_float64, js_value_get_float64, js_value_get_tag};

/// JSValue: a 64-bit value with the tag in the high 16 bits (bits 48..64) and
/// the payload (pointer or small value) in the low 48 bits.
///
/// On 64-bit the GC-managed pointer is stored in the low 48 bits, which covers
/// the full canonical x86-64 user address space (< 2^48). The tag is cleared
/// when reconstructing a pointer;
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct JSValue(pub(crate) u64);

#[allow(non_upper_case_globals)]
impl JSValue {
    //
    // Constant values
    //
    pub const NULL: Self = Self((JSValueTag::Null as u64) << 48);
    pub const UNDEFINED: Self = Self((JSValueTag::Undefined as u64) << 48);
    pub const EXCEPTION: Self = Self((JSValueTag::Exception as u64) << 48);
    pub const UNINITIALIZED: Self = Self((JSValueTag::Uninitialized as u64) << 48);
    pub const ZERO: Self = Self(JSValueTag::Int.mkval(0));
    pub const FALSE: Self = Self(JSValueTag::Bool.mkval(0));
    pub const TRUE: Self = Self(JSValueTag::Bool.mkval(1));
    pub const NAN: Self = Self(JS_NAN_U64);

    const fn Bool(v: bool) -> Self {
        Self(JSValueTag::Bool.mkval(v as u32))
    }
    const fn Int(v: i32) -> Self {
        Self(JSValueTag::Int.mkval(v as u32))
    }
    const fn Float64(v: f64) -> Self {
        Self(js_new_float64(v))
    }
    const fn CatchOffset(v: i32) -> Self {
        Self(JSValueTag::CatchOffset.mkval(v as u32))
    }
}

impl PartialEq for JSValue {
    fn eq(&self, other: &Self) -> bool {
        let ta = js_value_get_tag(self.0);
        let tb = js_value_get_tag(other.0);

        if ta != tb {
            return false;
        }

        // Bit-identical values are equal, except Float64: all NaN payloads
        // canonicalize to the same u64, yet JS semantics require NaN != NaN.
        if self.0 == other.0 && JSValueTag::norm_tag(ta) != JSValueTag::Float64 {
            return true;
        }

        match JSValueTag::norm_tag(ta) {
            JSValueTag::Int | JSValueTag::Bool | JSValueTag::CatchOffset => self.0 == other.0,
            JSValueTag::Float64 => js_value_get_float64(self.0).eq(&js_value_get_float64(other.0)),

            JSValueTag::String => unsafe {
                // string: content equality (matches non-nan-boxing StringRef::eq).
                // runtime-built strings (e.g. ropes from concat) are distinct nodes
                // with equal content, so raw-pointer compare would wrongly reject them.
                self.as_string_unchecked() == other.as_string_unchecked()
            },

            JSValueTag::Object | JSValueTag::FunctionBytecode | JSValueTag::Module => {
                self.0 == other.0 // raw pointer eq
            }

            JSValueTag::Symbol => unsafe {
                match (
                    self.as_string_unchecked().atom_info(),
                    other.as_string_unchecked().atom_info(),
                ) {
                    (Some(a0), Some(a1)) => a0.id() == a1.id(),
                    _ => false,
                }
            },

            JSValueTag::StringRope => unreachable!(),

            JSValueTag::Null
            | JSValueTag::Undefined
            | JSValueTag::Uninitialized
            | JSValueTag::Exception => true,
        }
    }
}

impl Default for JSValue {
    #[inline(always)]
    fn default() -> Self {
        JSValue::UNDEFINED
    }
}

impl From<i32> for JSValue {
    #[inline(always)]
    fn from(val: i32) -> Self {
        Self::Int(val)
    }
}

impl From<bool> for JSValue {
    #[inline(always)]
    fn from(value: bool) -> Self {
        JSValue::Bool(value)
    }
}

impl From<Object> for JSValue {
    #[inline(always)]
    fn from(value: Object) -> Self {
        let ptr = value.0.node_ptr();
        Self(JSValueTag::Object.mknode(ptr))
    }
}

impl JSValue {
    pub(crate) const fn tag(self) -> JSValueTag {
        let t = js_value_get_tag(self.0);
        if JSValueTag::is_float64(t) {
            JSValueTag::Float64
        } else {
            JSValueTag::from_raw(t).unwrap()
        }
    }

    #[inline(always)]
    pub(crate) const fn payload_i32(self) -> i32 {
        self.0 as i32
    }

    #[inline(always)]
    const unsafe fn as_ptr<T>(&self) -> *mut T {
        let p = (self.0 & 0x0000_FFFF_FFFF_FFFF) as *mut T;
        debug_assert!(!p.is_null());
        p
    }

    #[inline(always)]
    const unsafe fn as_node_ptr(&self) -> NonNull<GcHead> {
        let p = unsafe { self.as_ptr::<GcHead>() };
        unsafe { NonNull::new_unchecked(p) }
    }

    pub(crate) fn as_gc_node(&self) -> Option<NonNull<GcHead>> {
        match JSValueTag::norm_tag(js_value_get_tag(self.0)) {
            JSValueTag::Object
            | JSValueTag::String
            | JSValueTag::FunctionBytecode
            | JSValueTag::Module => Some(unsafe { self.as_node_ptr() }),

            JSValueTag::Symbol => unsafe {
                let sr = self.as_string_unchecked();
                let aid = sr.atom_info().unwrap().id();
                if !crate::atom::is_const_atom(aid) && !crate::atom::is_small_int_atom(aid) {
                    Some(sr.gc_node_ptr())
                } else {
                    None
                }
            },

            _ => None,
        }
    }

    #[inline(always)]
    pub const fn is_exception(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Exception as i32)
    }

    #[inline(always)]
    pub const fn is_null(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Null as i32)
    }

    #[inline(always)]
    pub const fn is_undefined(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Undefined as i32)
    }

    #[inline(always)]
    pub const fn is_undefined_or_null(&self) -> bool {
        let t = js_value_get_tag(self.0);
        t == (JSValueTag::Undefined as i32) || t == (JSValueTag::Null as i32)
    }

    #[inline(always)]
    pub const fn is_object(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Object as i32)
    }

    #[inline(always)]
    pub const fn is_string(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::String as i32)
    }

    #[inline(always)]
    pub const fn is_bool(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Bool as i32)
    }

    #[inline(always)]
    pub const fn is_int(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Int as i32)
    }

    #[inline(always)]
    pub const fn is_float64(&self) -> bool {
        JSValueTag::is_float64(js_value_get_tag(self.0))
    }

    pub const fn is_int_or_bool(&self) -> bool {
        let t = js_value_get_tag(self.0);
        t == (JSValueTag::Int as i32) || t == (JSValueTag::Bool as i32)
    }

    #[inline(always)]
    pub const fn is_number(&self) -> bool {
        let t = js_value_get_tag(self.0);
        t == (JSValueTag::Int as i32) || JSValueTag::is_float64(t)
    }

    #[inline(always)]
    pub const fn is_module(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Module as i32)
    }

    #[inline(always)]
    pub const fn is_uninitialized(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Uninitialized as i32)
    }

    #[inline(always)]
    pub const fn is_symbol(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Symbol as i32)
    }

    pub const fn is_string_or_symbol(&self) -> bool {
        let t = js_value_get_tag(self.0);
        t == (JSValueTag::String as i32) || t == (JSValueTag::Symbol as i32)
    }

    #[inline(always)]
    pub(crate) const fn is_catch_offset(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::CatchOffset as i32)
    }

    pub fn is_gc(&self) -> bool {
        match self.tag() {
            JSValueTag::Object
            | JSValueTag::String
            | JSValueTag::FunctionBytecode
            | JSValueTag::Module => true,
            JSValueTag::Symbol => unsafe {
                self.as_string_unchecked().atom_info().is_some_and(|a| {
                    !crate::atom::is_const_atom(a.id()) && !crate::atom::is_small_int_atom(a.id())
                })
            },
            _ => false,
        }
    }

    pub const fn as_int(&self) -> Option<i32> {
        if js_value_get_tag(self.0) == JSValueTag::Int as i32 {
            Some(self.payload_i32())
        } else {
            None
        }
    }

    // for Boolean value or Boolean object, returns bool
    pub fn as_bool(&self) -> Option<bool> {
        if js_value_get_tag(self.0) == (JSValueTag::Bool as i32) {
            Some(self.0 as u32 != 0)
        } else if self.tag() == JSValueTag::Object {
            // Boolean objects (e.g. `new Boolean(true)`) must unwrap their
            // stored primitive, not be rejected as a non-boolean.
            unsafe { self.as_object_unchecked().payload().bool().copied() }
        } else {
            None
        }
    }

    #[inline(always)]
    pub(crate) const unsafe fn as_float_unchecked(&self) -> f64 {
        js_value_get_float64(self.0)
    }

    pub const fn as_float(&self) -> Option<f64> {
        if JSValueTag::is_float64(js_value_get_tag(self.0)) {
            Some(js_value_get_float64(self.0))
        } else {
            None
        }
    }

    pub(crate) fn as_number(&self) -> Option<IntOrFloat> {
        let t = js_value_get_tag(self.0);
        if t == JSValueTag::Int as i32 {
            Some(IntOrFloat::from(self.payload_i32()))
        } else if JSValueTag::is_float64(t) {
            Some(IntOrFloat::from(js_value_get_float64(self.0)))
        } else {
            None
        }
    }

    pub(crate) unsafe fn as_object_unchecked(&self) -> Object {
        unsafe {
            let head = self.as_node_ptr();
            debug_assert_eq!(head.as_ref().dtype(), ObjectImpl::GC_TYPE_ID);
            Object(GcRef::<ObjectImpl>::from_node_ptr(head))
        }
    }

    pub(crate) unsafe fn as_string_unchecked(&self) -> StringRef {
        unsafe {
            let head = self.as_node_ptr();
            match head.as_ref().dtype() {
                StringImpl::GC_TYPE_ID => {
                    StringRef::from_flat(GcRef::<StringImpl>::from_node_ptr(head))
                }
                RopeImpl::GC_TYPE_ID => {
                    StringRef::from_rope(GcRef::<RopeImpl>::from_node_ptr(head))
                }
                _ => {
                    unreachable!()
                }
            }
        }
    }

    pub(crate) unsafe fn as_bytecode_unchecked(&self) -> Bytecode {
        unsafe {
            let head = self.as_node_ptr();
            debug_assert_eq!(head.as_ref().dtype(), BytecodeImpl::GC_TYPE_ID);
            Bytecode::from(GcRef::<BytecodeImpl>::from_node_ptr(head))
        }
    }

    pub(crate) unsafe fn as_module_unchecked(&self) -> ModuleDef {
        unsafe {
            let head = self.as_node_ptr();
            debug_assert_eq!(head.as_ref().dtype(), ModuleDefImpl::GC_TYPE_ID);
            ModuleDef::from(GcRef::<ModuleDefImpl>::from_node_ptr(head))
        }
    }

    pub fn as_object(&self) -> Option<Object> {
        if js_value_get_tag(self.0) == (JSValueTag::Object as i32) {
            Some(unsafe { self.as_object_unchecked() })
        } else {
            None
        }
    }

    /// Returns the `StringRef` if this is a primitive string.
    /// When `allow_string_object` is `true`, also unwraps `String` wrapper objects.
    /// With `false`, only a string *primitive* matches (equivalent to
    /// `is_string() && as_string()`).
    pub fn as_string(&self, allow_string_object: bool) -> Option<StringRef> {
        let tag = js_value_get_tag(self.0);
        if tag == (JSValueTag::String as i32) {
            Some(unsafe { self.as_string_unchecked() })
        } else if allow_string_object && tag == (JSValueTag::Object as i32) {
            // unwrap String object
            let o = unsafe { self.as_object_unchecked() };
            if o.class_id() == JS_CLASS_STRING {
                o.payload().string().copied()
            } else {
                None
            }
        } else {
            None
        }
    }

    /// for Symbol value or Symbol object, returns its StringRef descriptor.
    /// When `allow_symbol_object` is `true`, also unwraps `Symbol` wrapper objects.
    /// With `false`, only a symbol *primitive* matches (equivalent to
    /// `is_symbol() && as_symbol()`).
    pub fn as_symbol(&self, allow_symbol_object: bool) -> Option<StringRef> {
        let tag = js_value_get_tag(self.0);
        if tag == (JSValueTag::Symbol as i32) {
            return Some(unsafe { self.as_string_unchecked() });
        } else if allow_symbol_object && tag == (JSValueTag::Object as i32) {
            let o = unsafe { self.as_object_unchecked() };
            if o.class_id() == JS_CLASS_SYMBOL {
                return o.payload().string().copied();
            }
        }

        None
    }

    pub fn as_bytecode(&self) -> Option<Bytecode> {
        if js_value_get_tag(self.0) == (JSValueTag::FunctionBytecode as i32) {
            Some(unsafe { self.as_bytecode_unchecked() })
        } else {
            None
        }
    }

    pub fn as_module(&self) -> Option<ModuleDef> {
        if js_value_get_tag(self.0) == (JSValueTag::Module as i32) {
            Some(unsafe { self.as_module_unchecked() })
        } else {
            None
        }
    }

    pub(crate) const fn as_catch_offset(&self) -> Option<i32> {
        if js_value_get_tag(self.0) == (JSValueTag::CatchOffset as i32) {
            Some(self.payload_i32())
        } else {
            None
        }
    }
}

impl JSValue {
    pub(crate) fn hash(&self) -> u32 {
        let h: u32 = match self.tag() {
            JSValueTag::Object | JSValueTag::FunctionBytecode | JSValueTag::Module => unsafe {
                hash_usize(self.as_ptr::<GcHead>() as usize)
            },
            JSValueTag::Int => {
                let u = Float64Union {
                    uint64: self.payload_i32() as u64,
                };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            JSValueTag::String => unsafe { self.as_string_unchecked().hash() },
            JSValueTag::Symbol => unsafe { self.as_string_unchecked().atom_info().unwrap().hash() },
            JSValueTag::Float64 => {
                let d = js_value_get_float64(self.0);
                let u = Float64Union {
                    d: if d.is_nan() { f64::NAN } else { d },
                };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            JSValueTag::Bool => self.0 as u32,
            JSValueTag::CatchOffset => {
                let u = Float64Union {
                    uint64: self.payload_i32() as u64,
                };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            _ => 0,
        };

        h ^ (js_value_get_tag(self.0) as u32)
    }

    /// coerce value to bool
    pub fn to_bool(&self) -> Result<bool, i32> {
        match self.tag() {
            JSValueTag::Null | JSValueTag::Undefined => Ok(false),
            JSValueTag::Int | JSValueTag::Bool => Ok(self.payload_i32() != 0),
            JSValueTag::Object => Ok(unsafe { !self.as_object_unchecked().flags().is_htmldda() }),
            JSValueTag::String => Ok(unsafe { !self.as_string_unchecked().is_empty() }),
            JSValueTag::Float64 => {
                let d = js_value_get_float64(self.0);
                Ok(!d.is_nan() && d != 0.0)
            }
            JSValueTag::Exception => Err(-1),
            _ => Ok(true),
        }
    }

    /// Convert `self` to string or symbol value. (string is preferred)
    pub fn to_string_or_symbol(&self, ctx: &mut ScopedContext) -> Result<Self, i32> {
        match self.tag() {
            JSValueTag::String | JSValueTag::Symbol => {
                Ok(*self) // as-is
            }

            JSValueTag::Int => ctx
                .create_string_from_utf8(format!("{}", self.payload_i32()).into_bytes())
                .map(Self::make_string),

            JSValueTag::Bool => if self.payload_i32() != 0 {
                Atom::CONST_true
            } else {
                Atom::CONST_false
            }
            .force_string(ctx)
            .map(Self::make_string),

            JSValueTag::Float64 => {
                let n = js_value_get_float64(self.0);
                let s = f64_stringify(n, 10, None, JS_DTOA_VAR_FORMAT);
                ctx.create_string_from_str(s.as_ref())
                    .map(Self::make_string)
            }
            JSValueTag::Null => Atom::CONST_null.force_string(ctx).map(Self::make_string),
            JSValueTag::Undefined => Atom::CONST_undefined
                .force_string(ctx)
                .map(Self::make_string),

            JSValueTag::Object => self
                .to_primitive(ctx, HINT_STRING)
                .and_then(|s| s.to_string_or_symbol(ctx)),

            JSValueTag::FunctionBytecode => ctx
                .create_string_from_str("[bytecode]")
                .map(Self::make_string),

            JSValueTag::Exception => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                unsafe {
                    std::hint::unreachable_unchecked();
                }
            }
            _ => {
                ctx.throw_type_error("can't convert to string or symbol");
                Err(-1)
            }
        }
    }

    /// Coerce value `self` to string. if `self` is symbol, returns Err.
    ///
    /// If `self` is already a string, it is returned `as-is` (a rope is
    /// preserved; materialisation to a flat buffer is an internal decision made
    /// lazily by the `StringRef` operations that need it, not here).
    pub fn to_string(&self, ctx: &mut ScopedContext) -> Result<StringRef, i32> {
        match self.tag() {
            JSValueTag::String => {
                let s = unsafe { self.as_string_unchecked() };

                // NOTE on GC safety: a value returned by `to_string` must stay reachable
                // until the caller stores it in a traced location. Fresh string
                // allocations are young GC roots, but an *existing* rope node is only alive
                // while some rooted JSValue still references it. If that reference drops
                // while the caller keeps the returned `StringRef` (a `Copy` raw GC pointer)
                // across a GC safepoint, the node can be freed and any later use of it is a
                // use-after-free. We therefore mark the node LOCAL for the current scope so
                // it survives until the scope ends. We use `gc_protect_transient_force`
                // rather than the feature-gated `gc_protect_transient` because the
                // protection must apply even when that feature is disabled (the engine
                // otherwise relies solely on "fresh allocation = root", which this path
                // intentionally avoids to keep ropes lazy).
                //
                // Only ropes need this: flat strings already preserved the
                // "returns a rooted node" invariant before (the old `flatten`
                // returned the flat node as-is too), whereas ropes are what this
                // `to_string` change newly exposes to the unrooted-old-node risk.
                if !s.is_flat() {
                    ctx.gc_protect_transient_force(self);
                }

                Ok(s)
            }
            JSValueTag::Symbol => {
                ctx.throw_type_error("cannot convert symbol to string");
                Err(-1)
            }
            _ => {
                let v = self.to_string_or_symbol(ctx)?;
                match v.tag() {
                    JSValueTag::String => Ok(unsafe { v.as_string_unchecked() }),
                    JSValueTag::Symbol => {
                        ctx.throw_type_error("cannot convert symbol to string");
                        Err(-1)
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    /// convert value to atom. if value is not string or symbol, corece value to string then convert to atom.
    ///
    /// Note: returned atom is cloned, caller should free it after use.
    pub fn to_atom(&self, ctx: &mut ScopedContext) -> Result<Atom, i32> {
        let mut val = *self;

        loop {
            match val.tag() {
                JSValueTag::Undefined => {
                    return Ok(Atom::CONST_undefined);
                }
                JSValueTag::Null => {
                    return Ok(Atom::CONST_null);
                }
                JSValueTag::Bool => {
                    return Ok(if val.payload_i32() != 0 {
                        Atom::CONST_true
                    } else {
                        Atom::CONST_false
                    });
                }
                JSValueTag::Int => {
                    return Atom::from_i64(ctx, val.payload_i32() as i64);
                }
                JSValueTag::String => unsafe {
                    return val.as_string_unchecked().to_atom(ctx, AtomKind::String);
                },
                JSValueTag::Symbol => unsafe {
                    return val.as_string_unchecked().to_atom(ctx, AtomKind::Symbol);
                },
                JSValueTag::Object | JSValueTag::Float64 => {
                    val = val.to_string_or_symbol(ctx)?;
                    continue;
                }
                JSValueTag::Uninitialized
                | JSValueTag::CatchOffset
                | JSValueTag::Module
                | JSValueTag::FunctionBytecode => {
                    ctx.throw_type_error("cannot convert to atom");
                    return Err(-1);
                }

                JSValueTag::StringRope | JSValueTag::Exception => {
                    #[cfg(debug_assertions)]
                    unreachable!();
                    #[cfg(not(debug_assertions))]
                    return Err(-1);
                }
            }
        }
    }

    /// For primitive types (number, bool, string, symbol), return prototype of associated type.
    /// Only works for primitive types, otherwise returns Null.
    pub(crate) fn get_prototype_primitive(&self, ctx: &mut ScopedContext) -> Self {
        match self.tag() {
            JSValueTag::Int | JSValueTag::Float64 => ctx.class_proto(JS_CLASS_NUMBER),
            JSValueTag::Bool => ctx.class_proto(JS_CLASS_BOOLEAN),
            JSValueTag::String => ctx.class_proto(JS_CLASS_STRING),
            JSValueTag::Symbol => ctx.class_proto(crate::JS_CLASS_SYMBOL),
            _ => None,
        }
        .map_or(Self::NULL, Self::from)
    }

    /// get typeof value, equivalent to js `typeof val`
    ///
    /// Note:
    /// returned atom is not cloned, if caller want keep reference, should clone it.
    pub fn type_of(&self) -> Atom {
        match self.tag() {
            JSValueTag::Int | JSValueTag::Float64 => Atom::CONST_number,
            JSValueTag::Bool => Atom::CONST_boolean,
            JSValueTag::String => Atom::CONST_string,
            JSValueTag::Object => {
                if unsafe { self.as_object_unchecked().flags().is_htmldda() } {
                    Atom::CONST_undefined
                } else if self.is_callable() {
                    Atom::CONST_function
                } else {
                    Atom::CONST_object
                }
            }
            JSValueTag::Null => Atom::CONST_object,
            JSValueTag::Undefined => Atom::CONST_undefined,
            JSValueTag::Symbol => Atom::CONST_symbol,
            _ => Atom::CONST_unknown,
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
    pub fn objectify(&self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        match self.tag() {
            JSValueTag::Object => Ok(unsafe { self.as_object_unchecked() }),
            JSValueTag::String => unsafe {
                self.as_string_unchecked()
                    .objectify(ctx, JSValue::UNDEFINED)
            },
            JSValueTag::Int | JSValueTag::Float64 => ctx
                .create_instance_with_payload(ObjectPayload::NUMBER(*self))
                .map_err(|_| -1),
            JSValueTag::Bool => ctx
                .create_instance_with_payload(ObjectPayload::BOOLEAN(self.payload_i32() != 0))
                .map_err(|_| -1),

            JSValueTag::Symbol => ctx
                .create_instance_with_payload(ObjectPayload::SYMBOL(unsafe {
                    self.as_string_unchecked()
                }))
                .map_err(|_| -1),

            JSValueTag::Exception => {
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

    /// create Float64 value as-is
    #[inline(always)]
    pub(crate) const fn make_float(d: f64) -> Self {
        Self::Float64(d)
    }

    #[inline(always)]
    pub const fn make_catch_offset(pos: i32) -> Self {
        Self::CatchOffset(pos)
    }

    #[inline(always)]
    pub fn make_string(s: StringRef) -> Self {
        let p = s.gc_head_ptr();
        Self(JSValueTag::String.mknode(p))
    }

    #[inline(always)]
    pub fn make_symbol(s: StringRef) -> Self {
        let p = s.gc_head_ptr();
        Self(JSValueTag::Symbol.mknode(p))
    }
}

#[cfg(feature = "serde_json_support")]
mod serde_json_support {
    use smallvec::SmallVec;

    use crate::{ScopedContext, object::Object, value::JSValue};

    use super::super::tag::{JSValueTag, js_value_get_float64};

    impl JSValue {
        pub(crate) fn jsvalue_to_serde_json(
            ctx: &mut ScopedContext,
            val: &JSValue,
            stack: &mut SmallVec<[Object; 16]>,
        ) -> Result<serde_json::Value, i32> {
            match val.tag() {
                JSValueTag::Null | JSValueTag::Undefined => Ok(serde_json::Value::Null),
                JSValueTag::Bool => Ok(serde_json::Value::Bool(val.payload_i32() != 0)),
                JSValueTag::Int => Ok(serde_json::Value::Number((val.payload_i32() as i64).into())),
                JSValueTag::Float64 => {
                    let f = js_value_get_float64(val.0);
                    if !f.is_finite() {
                        Ok(serde_json::Value::Null)
                    } else {
                        Ok(serde_json::Number::from_f64(f)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null))
                    }
                }
                JSValueTag::String => {
                    let t = unsafe { val.as_string_unchecked() };
                    let s = t.content().encode_utf8(false).into_owned();
                    Ok(serde_json::Value::String(s))
                }
                JSValueTag::Object => {
                    let obj = unsafe { val.as_object_unchecked() };
                    Self::jsobject_to_serde_json(ctx, &obj, stack)
                }
                _ => Ok(serde_json::Value::Null),
            }
        }
    }
}
