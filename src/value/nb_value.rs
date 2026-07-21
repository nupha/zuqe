// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! NaN-boxing `JSValue` implementation (64-bit encoding).

use std::ptr::NonNull;

use gc_lite::{GcHead, GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_ERROR, ScopedContext,
    function::BytecodeImpl,
    module::ModuleDefImpl,
    number::JSFloat64Union,
    object::{Object, ObjectImpl},
    string::StringRef,
    utils::{hash_usize, unlikely},
};

use super::tag::{
    JS_FLOAT64_TAG_ADDEND, JS_NAN_U64, JSValueTag, js_new_float64, js_value_get_float64,
    js_value_get_tag,
};

/// JSValue: a 64-bit value with the tag in the high 32 bits and
/// the payload in the low 32 bits.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct JSValue(pub(crate) u64);

#[allow(non_upper_case_globals)]
impl JSValue {
    pub const NULL: Self = Self(JSValueTag::Null.mkval(0));
    pub const UNDEFINED: Self = Self(JSValueTag::Undefined.mkval(0));
    pub const EXCEPTION: Self = Self(JSValueTag::Exception.mkval(0));
    pub const UNINITIALIZED: Self = Self(JSValueTag::Uninitialized.mkval(0));
    pub const NAN: Self = Self(JS_NAN_U64);

    pub const fn Bool(v: bool) -> Self {
        Self(JSValueTag::Bool.mkval(v as u32))
    }
    pub const fn Int(v: i32) -> Self {
        Self(JSValueTag::Int.mkval(v as u32))
    }
    pub const fn Float64(v: f64) -> Self {
        Self(js_new_float64(v))
    }
    pub const fn CatchOffset(v: i32) -> Self {
        Self(JSValueTag::CatchOffset.mkval(v as u32))
    }
}

impl From<u64> for JSValue {
    #[inline(always)]
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Default for JSValue {
    #[inline(always)]
    fn default() -> Self {
        JSValue::UNDEFINED
    }
}

// ========================================================================
// Pointer reconstruction helpers
// ========================================================================
impl JSValue {
    /// Reconstruct a raw pointer from the low 32 bits (zero-extended to 64-bit).
    ///
    /// # Safety
    ///
    /// The pointer must have been stored via `From<T>` for a GC type, and the
    /// GC heap must reside in the low 4 GiB address space.
    #[inline(always)]
    unsafe fn reconstruct_ptr<T>(&self) -> *const T {
        (self.0 as u32) as *const T
    }

    #[inline(always)]
    unsafe fn reconstruct_object(&self) -> Object {
        Object(GcRef::<ObjectImpl>::from_ref_unchecked(
            &*self.reconstruct_ptr::<ObjectImpl>(),
        ))
    }
    #[inline(always)]
    unsafe fn reconstruct_string(&self) -> StringRef {
        let ptr = self.reconstruct_ptr::<GcHead>();
        std::mem::transmute::<NonNull<GcHead>, StringRef>(NonNull::new_unchecked(
            ptr as *mut GcHead,
        ))
    }
    #[inline(always)]
    unsafe fn reconstruct_bytecode(&self) -> crate::function::Bytecode {
        std::mem::transmute::<GcRef<BytecodeImpl>, crate::function::Bytecode>(
            GcRef::<BytecodeImpl>::from_ref_unchecked(&*self.reconstruct_ptr::<BytecodeImpl>()),
        )
    }
    #[inline(always)]
    unsafe fn reconstruct_module(&self) -> crate::module::ModuleDef {
        std::mem::transmute::<GcRef<ModuleDefImpl>, crate::module::ModuleDef>(
            GcRef::<ModuleDefImpl>::from_ref_unchecked(&*self.reconstruct_ptr::<ModuleDefImpl>()),
        )
    }
}

impl JSValue {
    #[inline(always)]
    pub fn is_exception(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Exception as i32)
    }
    #[inline(always)]
    pub fn is_null(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Null as i32)
    }
    #[inline(always)]
    pub fn is_undefined(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Undefined as i32)
    }
    #[inline(always)]
    pub fn is_undefined_or_null(&self) -> bool {
        let t = js_value_get_tag(self.0);
        t == (JSValueTag::Undefined as i32) || t == (JSValueTag::Null as i32)
    }
    #[inline(always)]
    pub fn is_object(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Object as i32)
    }
    #[inline(always)]
    pub fn is_string(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::String as i32)
    }
    #[inline(always)]
    pub fn is_bool(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Bool as i32)
    }
    #[inline(always)]
    pub fn is_int(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Int as i32)
    }
    #[inline(always)]
    pub fn is_float64(&self) -> bool {
        JSValueTag::is_float64_raw(js_value_get_tag(self.0))
    }
    #[inline(always)]
    pub fn is_number(&self) -> bool {
        let t = js_value_get_tag(self.0);
        t == (JSValueTag::Int as i32) || JSValueTag::is_float64_raw(t)
    }
    #[inline(always)]
    pub fn is_module(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Module as i32)
    }
    #[inline(always)]
    pub fn is_uninitialized(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Uninitialized as i32)
    }

    #[cfg(feature = "builtin_symbol")]
    #[inline(always)]
    pub fn is_symbol(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Symbol as i32)
    }
    pub fn is_htmldda(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Object as i32)
            && unsafe { self.reconstruct_object().flags().is_htmldda() }
    }
    #[inline]
    pub fn is_callable(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Object as i32)
            && unsafe { self.reconstruct_object().is_callable() }
    }
    pub fn is_error(&self) -> bool {
        js_value_get_tag(self.0) == (JSValueTag::Object as i32)
            && unsafe { self.reconstruct_object().class_id() == JS_CLASS_ERROR }
    }
    pub fn is_gc(&self) -> bool {
        match JSValueTag::norm_tag(js_value_get_tag(self.0)) {
            JSValueTag::Object
            | JSValueTag::String
            | JSValueTag::FunctionBytecode
            | JSValueTag::Module => true,
            #[cfg(feature = "builtin_symbol")]
            JSValueTag::Symbol => unsafe {
                self.reconstruct_string().atom_info().is_some_and(|a| {
                    !crate::atom::is_const_atom(a.id()) && !crate::atom::is_small_int_atom(a.id())
                })
            },
            _ => false,
        }
    }
}

impl JSValue {
    #[inline(always)]
    pub const fn tag(self) -> i32 {
        (self.0 >> 32) as i32
    }
    pub fn norm_tag(self) -> i32 {
        JSValueTag::norm_tag(self.tag()).to_i32()
    }
    #[inline(always)]
    pub fn as_u64(self) -> u64 {
        self.0
    }
    #[inline(always)]
    pub fn payload_u32(self) -> u32 {
        self.0 as u32
    }
    #[inline(always)]
    pub fn payload_i32(self) -> i32 {
        self.0 as i32
    }

    pub fn as_object(&self) -> Option<Object> {
        if js_value_get_tag(self.0) == (JSValueTag::Object as i32) {
            Some(unsafe { self.reconstruct_object() })
        } else {
            None
        }
    }
    pub fn as_string(&self) -> Option<StringRef> {
        if js_value_get_tag(self.0) == (JSValueTag::String as i32) {
            Some(unsafe { self.reconstruct_string() })
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if js_value_get_tag(self.0) == (JSValueTag::Bool as i32) {
            Some(self.0 as u32 != 0)
        } else {
            None
        }
    }
    pub unsafe fn as_object_unchecked(&self) -> Object {
        self.reconstruct_object()
    }

    pub fn gc_node_ptr(&self) -> Option<NonNull<GcHead>> {
        match JSValueTag::norm_tag(js_value_get_tag(self.0)) {
            JSValueTag::Object => Some(unsafe { self.reconstruct_object().gc_node_ptr() }),
            JSValueTag::String => Some(unsafe { self.reconstruct_string().gc_node_ptr() }),
            #[cfg(feature = "builtin_symbol")]
            JSValueTag::Symbol => unsafe {
                let sr = self.reconstruct_string();
                let aid = sr.atom_info().unwrap().id();
                if !crate::atom::is_const_atom(aid) && !crate::atom::is_small_int_atom(aid) {
                    Some(sr.gc_node_ptr())
                } else {
                    None
                }
            },
            JSValueTag::FunctionBytecode => {
                Some(unsafe { self.reconstruct_bytecode().gc_node_ptr() })
            }
            JSValueTag::Module => Some(unsafe { self.reconstruct_module().gc_node_ptr() }),
            _ => None,
        }
    }
    #[inline(always)]
    pub fn gc_node(&self) -> Option<&GcHead> {
        self.gc_node_ptr().map(|n| unsafe { n.as_ref() })
    }

    pub fn ensure_object(&self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        self.as_object().ok_or_else(|| {
            ctx.throw_type_error_not_an_object();
            -1
        })
    }
    pub fn into_object(self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        self.ensure_object(ctx)
    }
    pub fn ensure_callable(&self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        let o = self.ensure_object(ctx)?;
        if o.is_callable() {
            Ok(o)
        } else {
            ctx.throw_type_error("not a function");
            Err(-1)
        }
    }
    pub fn into_callable(self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        self.ensure_callable(ctx)
    }

    #[inline]
    pub fn err(self) -> Result<Self, i32> {
        if unlikely(self.is_exception()) {
            Err(-1)
        } else {
            Ok(self)
        }
    }

    pub(crate) fn hash(&self) -> u32 {
        let tag = js_value_get_tag(self.0);
        let h: u32 = match JSValueTag::norm_tag(tag) {
            JSValueTag::Object => unsafe { hash_usize(self.reconstruct_object().id()) },
            JSValueTag::Int => {
                let u = JSFloat64Union {
                    uint64: self.payload_i32() as u64,
                };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            JSValueTag::String => unsafe { self.reconstruct_string().hash() },
            #[cfg(feature = "builtin_symbol")]
            JSValueTag::Symbol => unsafe { self.reconstruct_string().atom_info().unwrap().hash() },
            JSValueTag::Float64 => {
                let d = js_value_get_float64(self.0);
                let u = JSFloat64Union {
                    d: if d.is_nan() { f64::NAN } else { d },
                };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            JSValueTag::Bool => self.0 as u32,
            JSValueTag::CatchOffset => {
                let u = JSFloat64Union {
                    uint64: self.payload_i32() as u64,
                };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            JSValueTag::FunctionBytecode => unsafe {
                hash_usize(self.reconstruct_bytecode().gc_node_ptr().as_ptr() as usize)
            },
            JSValueTag::Module => unsafe {
                hash_usize(self.reconstruct_module().gc_node_ptr().as_ptr() as usize)
            },
            _ => 0,
        };
        h ^ (tag as u32)
    }
}

impl PartialEq for JSValue {
    fn eq(&self, other: &Self) -> bool {
        let ta = js_value_get_tag(self.0);
        let tb = js_value_get_tag(other.0);
        if ta != tb {
            return false;
        }
        let nt = JSValueTag::norm_tag(ta);
        match nt {
            JSValueTag::Bool => self.0 as u32 == other.0 as u32,
            JSValueTag::Int => self.payload_i32() == other.payload_i32(),
            JSValueTag::Float64 => js_value_get_float64(self.0).eq(&js_value_get_float64(other.0)),
            JSValueTag::String => unsafe {
                self.reconstruct_string() == other.reconstruct_string()
            },
            JSValueTag::Object => unsafe {
                self.reconstruct_object() == other.reconstruct_object()
            },
            #[cfg(feature = "builtin_symbol")]
            JSValueTag::Symbol => unsafe {
                match (
                    self.reconstruct_string().atom_info(),
                    other.reconstruct_string().atom_info(),
                ) {
                    (Some(a0), Some(a1)) => a0.id() == a1.id(),
                    _ => false,
                }
            },
            JSValueTag::FunctionBytecode => unsafe {
                self.reconstruct_bytecode() == other.reconstruct_bytecode()
            },
            JSValueTag::Module => unsafe {
                self.reconstruct_module() == other.reconstruct_module()
            },
            JSValueTag::CatchOffset => self.payload_i32() == other.payload_i32(),
            _ => true,
        }
    }
}

impl GcTrace for JSValue {
    #[inline(always)]
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(p) = self.gc_node_ptr() {
            tr.add_node(p);
        }
    }
}

impl Eq for JSValue {}
impl std::hash::Hash for JSValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u32(self.hash());
    }
}
