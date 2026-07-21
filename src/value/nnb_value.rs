// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::GcHead;

use crate::{
    Atom, JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING, JS_CLASS_SYMBOL, ScopedContext,
    atom::AtomKind,
    function::Bytecode,
    module::ModuleDef,
    number::{JS_DTOA_VAR_FORMAT, JSFloat64Union, f64_stringify},
    object::{Object, ObjectPayload},
    string::StringRef,
    utils::hash_usize,
    value::HINT_STRING,
};

#[repr(u8)]
#[derive(Debug, Clone, Copy, Default)]
pub enum JSValue {
    #[default]
    UNDEFINED,
    NULL,
    Bool(bool),
    Int(i32),
    UNINITIALIZED,
    CatchOffset(i32),
    EXCEPTION,
    Float64(f64),
    String(StringRef),
    #[cfg(feature = "builtin_symbol")]
    Symbol(StringRef),
    Module(ModuleDef),
    Object(Object),
    Bytecode(Bytecode),
}

impl PartialEq for JSValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(b0), Self::Bool(b1)) => *b0 == *b1,
            (Self::Int(i0), Self::Int(i1)) => *i0 == *i1,
            (Self::Float64(f0), Self::Float64(f1)) => f0.eq(f1),
            (Self::String(s0), Self::String(s1)) => s0 == s1,
            (Self::Object(l0), Self::Object(r0)) => l0 == r0,

            #[cfg(feature = "builtin_symbol")]
            (Self::Symbol(s0), Self::Symbol(s1)) => match (s0.atom_info(), s1.atom_info()) {
                (Some(a0), Some(a1)) => a0.id() == a1.id(),
                _ => false,
            },

            (Self::UNDEFINED, Self::UNDEFINED)
            | (Self::NULL, Self::NULL)
            | (Self::EXCEPTION, Self::EXCEPTION)
            | (Self::UNINITIALIZED, Self::UNINITIALIZED) => true,

            (Self::Bytecode(l0), Self::Bytecode(r0)) => l0 == r0,
            (Self::Module(m0), Self::Module(m1)) => m0 == m1,
            (Self::CatchOffset(a), Self::CatchOffset(b)) => a.eq(b),

            _ => false,
        }
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
        Self::Object(value)
    }
}

impl JSValue {
    #[inline(always)]
    pub const fn is_exception(&self) -> bool {
        matches!(self, Self::EXCEPTION)
    }

    #[inline(always)]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::NULL)
    }

    #[inline(always)]
    pub const fn is_undefined(&self) -> bool {
        matches!(self, Self::UNDEFINED)
    }

    #[inline(always)]
    pub const fn is_undefined_or_null(&self) -> bool {
        matches!(self, Self::UNDEFINED | Self::NULL)
    }

    #[inline(always)]
    pub const fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    #[inline(always)]
    pub const fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    #[cfg(feature = "builtin_symbol")]
    #[inline(always)]
    pub const fn is_symbol(&self) -> bool {
        matches!(self, Self::Symbol(_))
    }

    #[inline(always)]
    pub const fn is_bool(&self) -> bool {
        matches!(self, Self::Bool(_))
    }

    #[inline(always)]
    pub const fn is_int(&self) -> bool {
        matches!(self, Self::Int(_))
    }

    #[inline(always)]
    pub const fn is_float64(&self) -> bool {
        matches!(self, Self::Float64(_))
    }

    #[inline(always)]
    pub const fn is_number(&self) -> bool {
        matches!(self, Self::Int(_) | Self::Float64(_))
    }

    #[inline(always)]
    pub const fn is_module(&self) -> bool {
        matches!(self, Self::Module(_))
    }

    #[inline(always)]
    pub const fn is_uninitialized(&self) -> bool {
        matches!(self, Self::UNINITIALIZED)
    }

    pub fn is_gc(&self) -> bool {
        match self {
            Self::Object(_) | Self::Bytecode(_) | Self::Module(_) => true,
            Self::String(_) => true,
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => {
                let aid = s.atom_info().unwrap().id();
                !crate::atom::is_const_atom(aid) && !crate::atom::is_small_int_atom(aid)
            }
            _ => false,
        }
    }

    pub fn gc_node_ptr(&self) -> Option<NonNull<GcHead>> {
        match self {
            Self::Object(o) => Some(o.gc_node_ptr()),
            Self::String(s) => Some(s.gc_node_ptr()),
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => {
                let aid = s.atom_info().unwrap().id();
                if !crate::atom::is_const_atom(aid) && !crate::atom::is_small_int_atom(aid) {
                    Some(s.gc_node_ptr())
                } else {
                    None
                }
            }
            Self::Bytecode(b) => Some(b.gc_node_ptr()),
            Self::Module(m) => Some(m.gc_node_ptr()),
            _ => None,
        }
    }

    pub fn hash(&self) -> u32 {
        let h: u32 = match self {
            Self::Object(o) => hash_usize(o.id()),
            Self::Int(d) => {
                let u = JSFloat64Union { uint64: *d as u64 };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            Self::String(s) => s.hash(),
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => s.atom_info().unwrap().hash(),

            Self::Float64(d) => {
                let mut d = *d;
                if d.is_nan() {
                    d = f64::NAN;
                }
                let u = JSFloat64Union { d };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            Self::UNDEFINED | Self::NULL | Self::UNINITIALIZED | Self::EXCEPTION => 0,
            Self::Bool(b) => *b as u32,
            Self::Bytecode(f) => hash_usize(f.gc_node_ptr().as_ptr() as usize),
            Self::Module(m) => hash_usize(m.gc_node_ptr().as_ptr() as usize),
            Self::CatchOffset(i) => {
                let u = JSFloat64Union { uint64: *i as u64 };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
        };

        let tag = unsafe { *<*const _>::from(self).cast::<u8>() };
        h ^ (tag as u32) // XOR
    }

    /// create Float64 value as-is
    #[inline(always)]
    pub(crate) const fn from_f64_as_is(d: f64) -> Self {
        Self::Float64(d)
    }

    /// coerce value to bool
    pub fn to_bool(&self) -> Result<bool, i32> {
        match self {
            Self::NULL | Self::UNDEFINED => Ok(false),
            Self::Int(i) => Ok(*i != 0),
            Self::Bool(b) => Ok(*b),
            Self::Object(o) => Ok(!o.flags().is_htmldda()),
            Self::String(s) => Ok(!s.is_empty()),
            Self::Float64(d) => Ok(!d.is_nan() && *d != 0.0),
            Self::EXCEPTION => Err(-1),
            _ => Ok(true),
        }
    }

    /// Convert `self` to string or symbol value. (string is preferred)
    pub fn to_string_or_symbol(&self, ctx: &mut ScopedContext) -> Result<Self, i32> {
        match self {
            Self::String(_) => {
                Ok(*self) // as-is
            }
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(_) => Ok(*self),

            Self::Int(i) => ctx
                .create_string_from_utf8(format!("{}", *i).into_bytes())
                .map(Self::from),
            Self::Bool(b) => if *b {
                Atom::CONST_true
            } else {
                Atom::CONST_false
            }
            .force_string(ctx)
            .map(Self::from),
            Self::Float64(n) => {
                let s = f64_stringify(*n, 10, None, JS_DTOA_VAR_FORMAT);
                ctx.create_string_from_str(s.as_ref()).map(Self::from)
            }
            Self::NULL => Atom::CONST_null.force_string(ctx).map(Self::from),
            Self::UNDEFINED => Atom::CONST_undefined.force_string(ctx).map(Self::from),

            Self::Object(_) => self
                .to_primitive(ctx, HINT_STRING)
                .and_then(|s| s.to_string_or_symbol(ctx)),

            Self::Bytecode(_) => ctx.create_string_from_str("[bytecode]").map(Self::from),

            Self::EXCEPTION => {
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

    /// convert value to atom. if value is not string or symbol, corece value to string then convert to atom.
    ///
    /// Note: returned atom is cloned, caller should free it after use.
    pub fn to_atom(&self, ctx: &mut ScopedContext) -> Result<Atom, i32> {
        let mut val = *self;

        loop {
            match val {
                Self::UNDEFINED => {
                    return Ok(Atom::CONST_undefined);
                }
                Self::NULL => {
                    return Ok(Atom::CONST_null);
                }
                Self::Bool(b) => {
                    return Ok(if b {
                        Atom::CONST_true
                    } else {
                        Atom::CONST_false
                    });
                }
                Self::Int(i) => {
                    return Atom::from_i64(ctx, i as i64);
                }
                Self::String(s) => {
                    return s.to_atom(ctx, AtomKind::String);
                }
                #[cfg(feature = "builtin_symbol")]
                Self::Symbol(s) => {
                    return s.to_atom(ctx, AtomKind::Symbol);
                }
                Self::Object(_) | Self::Float64(_) => {
                    val = self.to_string_or_symbol(ctx)?;
                    continue;
                }
                Self::UNINITIALIZED
                | Self::CatchOffset(_)
                | Self::Module(_)
                | Self::Bytecode(_) => {
                    ctx.throw_type_error("cannot convert to atom");
                    return Err(-1);
                }
                Self::EXCEPTION => {
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
        match self {
            Self::Int(_) | Self::Float64(_) => ctx.class_proto(JS_CLASS_NUMBER),
            Self::Bool(_) => ctx.class_proto(JS_CLASS_BOOLEAN),
            Self::String(_) => ctx.class_proto(JS_CLASS_STRING),
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(_) => ctx.class_proto(crate::JS_CLASS_SYMBOL),
            _ => None,
        }
        .map_or(Self::NULL, Self::from)
    }

    pub fn as_string(&self) -> Option<&StringRef> {
        match self {
            Self::String(s) => Some(s),
            Self::Object(o) if o.class_id() == JS_CLASS_STRING => o.payload().string(),
            _ => None,
        }
    }

    /// for Symbol value or Symbol object, returns its StringRef descriptor
    pub fn as_symbol(&self) -> Option<&StringRef> {
        match self {
            Self::Symbol(s) => Some(s),
            Self::Object(o) if o.class_id() == JS_CLASS_SYMBOL => o.payload().string(),
            _ => None,
        }
    }

    /// for Boolean value or Boolean object, returns bool
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Object(o) => o.payload().bool().copied(),
            _ => None,
        }
    }

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

    /// get typeof value, equivalent to js `typeof val`
    ///
    /// Note:
    /// returned atom is not cloned, if caller want keep reference, should clone it.
    pub fn type_of(&self) -> Atom {
        match self {
            Self::Int(_) | Self::Float64(_) => Atom::CONST_number,
            Self::Bool(_) => Atom::CONST_boolean,
            Self::String(_) => Atom::CONST_string,
            Self::Object(p) => {
                if p.flags().is_htmldda() {
                    Atom::CONST_undefined
                } else if self.is_callable() {
                    Atom::CONST_function
                } else {
                    Atom::CONST_object
                }
            }
            Self::NULL => Atom::CONST_object,
            Self::UNDEFINED => Atom::CONST_undefined,
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(_) => Atom::CONST_symbol,
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
        match self {
            Self::Object(o) => Ok(*o),
            Self::String(s) => s.objectify(ctx, JSValue::UNDEFINED),

            Self::Int(_) | Self::Float64(_) => ctx
                .create_instance_with_payload(ObjectPayload::NUMBER(*self))
                .map_err(|_| -1),
            Self::Bool(b) => ctx
                .create_instance_with_payload(ObjectPayload::BOOLEAN(*b))
                .map_err(|_| -1),
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => ctx
                .create_instance_with_payload(ObjectPayload::SYMBOL(*s))
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
}

#[cfg(feature = "serde_json_support")]
mod serde_json_support {
    use smallvec::SmallVec;

    use crate::{ScopedContext, object::Object, value::JSValue};

    impl JSValue {
        pub(crate) fn jsvalue_to_serde_json(
            ctx: &mut ScopedContext,
            val: &JSValue,
            stack: &mut SmallVec<[Object; 16]>,
        ) -> Result<serde_json::Value, i32> {
            match val {
                JSValue::NULL | JSValue::UNDEFINED => Ok(serde_json::Value::Null),
                JSValue::Bool(b) => Ok(serde_json::Value::Bool(*b)),
                JSValue::Int(i) => Ok(serde_json::Value::Number((*i as i64).into())),
                JSValue::Float64(f) => {
                    if !f.is_finite() {
                        Ok(serde_json::Value::Null)
                    } else {
                        Ok(serde_json::Number::from_f64(*f)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null))
                    }
                }
                JSValue::String(s) => {
                    let rust_str = s.content().encode_utf8(false).into_owned();
                    Ok(serde_json::Value::String(rust_str))
                }
                JSValue::Object(obj) => Self::object_to_serde_json(ctx, obj, stack),
                _ => Ok(serde_json::Value::Null),
            }
        }
    }
}
