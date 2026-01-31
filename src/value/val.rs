use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use gc_lite::{GcHead, GcTraceOp};

use crate::{
    JS_CLASS_ERROR,
    atom::{Atom, AtomKind},
    function::Bytecode,
    module::ModuleDef,
    number::JSFloat64Union,
    object::Object,
    string::StringRef,
    utils::{hash_usize, unlikely},
};

#[cfg(feature = "nan_boxing")]
mod nb_value {
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    #[repr(transparent)]
    pub struct JSValue(u64);

    impl From<u64> for JSValue {
        fn from(value: u64) -> Self {
            Self(value)
        }
    }

    impl From<JSValue> for u64 {
        fn from(value: JSValue) -> Self {
            value.0
        }
    }

    #[macro_export]
    macro_rules! JS_MKVAL {
        ($tag:expr, $val:expr) => {
            (($tag as u64) << 32) | ($val as u32) as u64
        };
    }

    #[macro_export]
    macro_rules! JS_MKPTR {
        ($tag:expr, $ptr:expr) => {{
            let tag_u32 = $tag as u32;
            let ptr_u64 = $ptr as u64;
            JSValue(((tag_u32 as u64) << 32) | (ptr_u64 & 0xFFFF_FFFF))
        }};
    }

    pub const JS_FLOAT64_TAG_ADDEND: i32 = 0x7ff80000 - JS_TAG_FIRST + 1;

    #[deprecated(note = "use JSValue::NAN instead")]
    pub const JS_NAN: u64 = 0x7ff8000000000000 - ((JS_FLOAT64_TAG_ADDEND as u64) << 32);

    impl JSValue {
        pub const NAN: Self = 0x7ff8000000000000 - ((JS_FLOAT64_TAG_ADDEND as u64) << 32);

        /// get value tag as int
        #[inline(always)]
        pub const fn tag(self) -> i32 {
            // ((v >> 32) & 0xFFFF_FFFF) as i32
            (v >> 32) as i32
        }
    }

    #[inline(always)]
    pub fn JS_VALUE_GET_FLOAT64(v: JSValue) -> f64 {
        union _U {
            v: JSValue,
            d: f64,
        };

        let mut u = _U { v };
        unsafe {
            u.v += (JS_FLOAT64_TAG_ADDEND << 32) as f64;
            u.d
        }
    }

    #[inline(always)]
    pub fn JS_VALUE_GET_FLOAT64(v: JSValue) -> f64 {
        unsafe { v.u.float64 }
    }

    #[inline(always)]
    pub fn JS_VALUE_IS_NAN(v: JSValue) -> bool {
        let tag = v.tag();
        tag == (JS_NAN >> 32)
    }

    #[inline(always)]
    pub fn JS_TAG_IS_FLOAT64(tag: i32) -> bool {
        tag - super::JS_TAG_FIRST >= super::JS_TAG_FLOAT64 - super::JS_TAG_FIRST
    }
}

#[cfg(feature = "nan_boxing")]
pub use nb_value::*;

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

unsafe impl gc_lite::GcTracable for JSValue {
    #[inline(always)]
    fn trace(&self, mut tr: GcTraceOp) {
        if let Some(p) = self.gc_node_ptr() {
            tr.add_node(p);
        }

        // match self {
        //     Self::String(s) => {
        //         if s.rt().is_some() {
        //             // not string for const atom
        //             s.trace(tracer);
        //         }
        //     }
        //     Self::Object(o) => {
        //         o.trace(tracer);
        //     }
        //     #[cfg(feature = "builtin_symbol")]
        //     Self::Symbol(a) => {
        //         a.trace(tracer);
        //     }
        //     Self::Bytecode(b) => {
        //         b.trace(tracer);
        //     }
        //     Self::Module(m) => {
        //         m.trace(tracer);
        //     }
        //     _ => {}
        // }
    }
}

impl PartialEq for JSValue {
    /// Compare two JSValue of same type
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(b0), Self::Bool(b1)) => *b0 == *b1,
            (Self::Int(i0), Self::Int(i1)) => *i0 == *i1,
            (Self::Float64(f0), Self::Float64(f1)) => f0.eq(f1),
            (Self::String(s0), Self::String(s1)) => s0 == s1,
            #[cfg(feature = "builtin_symbol")]
            (Self::Symbol(a0), Self::Symbol(a1)) => a0 == a1,
            (Self::Object(l0), Self::Object(r0)) => l0 == r0,

            (Self::UNDEFINED, Self::UNDEFINED)
            | (Self::NULL, Self::NULL)
            | (Self::EXCEPTION, Self::EXCEPTION)
            | (Self::UNINITIALIZED, Self::UNINITIALIZED) => true,

            (Self::Bytecode(l0), Self::Bytecode(r0)) => l0.id() == r0.id(),
            (Self::Module(m0), Self::Module(m1)) => m0.id() == m1.id(),
            (Self::CatchOffset(a), Self::CatchOffset(b)) => a.eq(b),

            _ => false,
        }
    }
}

// #[cfg(not(feature = "nan_boxing"))]
// mod nnb_value {
//     use std::{ffi::c_void, mem::ManuallyDrop};

//     #[derive(Debug, Clone, Default)]
//     pub struct JSValue {
//         pub tag: super::JSTag,
//         pub u: JSValueUnion,
//     }

//     #[derive(Clone)]
//     pub union JSValueUnion {
//         pub int32: i32,
//         pub float64: f64,
//         /// for values need ref count which have a payload
//         pub payload: ManuallyDrop<ValuePayload>,
//     }

//     impl Default for JSValueUnion {
//         fn default() -> Self {
//             Self { int32: 0 }
//         }
//     }

//     impl std::fmt::Debug for JSValueUnion {
//         fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//             unsafe {
//                 f.debug_struct("JSValueUnion")
//                     .field("i32", &self.int32)
//                     .field("f64", &self.float64)
//                     .field("ptr", &self.ptr)
//                     .finish()
//             }
//         }
//     }

//     impl JSValue {
//         pub const NAN: Self = Self {
//             tag: super::JSTag::Float64,
//             u: JSValueUnion { float64: f64::NAN },
//         };

//         /// get value tag as int
//         #[inline(always)]
//         pub const fn tag(self) -> i32 {
//             self.tag as i32
//         }
//     }

//     #[deprecated(note = "use JSValue::NAN instead")]
//     pub const JS_NAN: JSValue = JSValue {
//         tag: super::JSTag::Float64,
//         u: JSValueUnion { float64: f64::NAN },
//     };

//     #[inline(always)]
//     pub fn JS_VALUE_IS_NAN(v: JSValue) -> bool {
//         union _U {
//             d: f64,
//             u64: u64,
//         }
//         if v.tag != super::JSTag::Float64 {
//             false
//         } else {
//             unsafe {
//                 let u = _U { d: v.u.float64 };
//                 (u.u64 & 0x7fffffffffffffff) > 0x7ff0000000000000
//             }
//         }
//     }

//     #[inline(always)]
//     pub const fn JS_VALUE_GET_FLOAT64(v: JSValue) -> f64 {
//         unsafe { v.u.float64 }
//     }

//     #[inline(always)]
//     pub const fn JS_TAG_IS_FLOAT64(tag: i32) -> bool {
//         tag == super::JS_TAG_FLOAT64
//     }
// }

// #[cfg(not(feature = "nan_boxing"))]
// pub use nnb_value::*;

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

    pub fn is_htmldda(&self) -> bool {
        if let Self::Object(o) = self {
            o.flags().is_htmldda()
        } else {
            false
        }
    }

    /// Test if value `self` can be called, that means value is a:
    ///
    /// - Compiled bytecode functions
    /// - Proxy objects that wrapping a function
    /// - Objects of native class which has `call` handler
    #[inline]
    pub fn is_function(&self) -> bool {
        self.as_object().is_some_and(|o| o.is_function())
    }

    pub fn is_error(&self) -> bool {
        self.as_object()
            .is_some_and(|o| o.class_id() == JS_CLASS_ERROR)
    }

    /// if `self` is an exception, returns Err if yes; Otherwise returns Ok(self)
    #[inline]
    pub const fn err(self) -> Result<Self, i32> {
        if unlikely(self.is_exception()) {
            Err(-1)
        } else {
            Ok(self)
        }
    }

    /// calc hash of value
    pub(crate) fn hash(&self) -> u32 {
        let h: u32 = match self {
            Self::Object(o) => hash_usize(o.id()),
            Self::Int(d) => {
                let u = JSFloat64Union { uint64: *d as u64 };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
            Self::String(s) => s.atom_info().map_or_else(
                || Atom::calc_atom_hash(s.content(), AtomKind::String),
                |a| a.hash(),
            ),
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
            Self::Bytecode(f) => hash_usize(f.id()),
            Self::Module(m) => hash_usize(m.id()),
            Self::CatchOffset(i) => {
                let u = JSFloat64Union { uint64: *i as u64 };
                unsafe { (u.uint32[0] ^ u.uint32[1]).wrapping_mul(3163) }
            }
        };

        let tag = unsafe { *<*const _>::from(self).cast::<u8>() };
        h ^ (tag as u32) // XOR
    }

    /// check if value is gc aware
    #[inline]
    pub fn is_gc(&self) -> bool {
        match self {
            Self::Object(_) | Self::Bytecode(_) | Self::Module(_) => true,
            Self::String(s) if s.rt().is_some() => true,
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => {
                let aid = s.atom_info().unwrap().id();
                !crate::atom::is_const_atom(aid) && !crate::atom::is_small_int_atom(aid)
            }
            _ => false,
        }
    }

    /// get value's gc node raw pointer. returns null if value do not need gc.
    #[inline]
    pub fn gc_node_ptr(&self) -> Option<NonNull<GcHead>> {
        match self {
            Self::Object(o) => Some(o.gc_ref().node_ptr()),
            Self::String(s) => {
                // if s.rt().is_some() {
                //     Some(s.gc_ref().node_ptr())
                // } else {
                //     None
                // }
                debug_assert!(s.rt().is_some(), "O.o");
                Some(s.gc_ref().node_ptr())
            }
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => {
                let aid = s.atom_info().unwrap().id();
                if !crate::atom::is_const_atom(aid) && !crate::atom::is_small_int_atom(aid) {
                    Some(s.gc_ref().node_ptr())
                } else {
                    None
                }
            }
            Self::Bytecode(b) => Some(b.gc_ref().node_ptr()),
            Self::Module(m) => Some(m.gc_ref().node_ptr()),
            _ => None,
        }
    }
}

pub(crate) const HINT_STRING: i32 = 0;
pub(crate) const HINT_NUMBER: i32 = 1;
pub(crate) const HINT_NONE: i32 = 2;
pub(crate) const HINT_FORCE_ORDINARY: i32 = 0x01 << 4; /* don't try Symbol.toPrimitive */

/// Value with lifetime
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Value<'a> {
    inner: JSValue,
    _marker: PhantomData<&'a ()>,
}

impl<'a> Deref for Value<'a> {
    type Target = JSValue;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> DerefMut for Value<'a> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<'a> From<Value<'a>> for JSValue {
    #[inline(always)]
    fn from(value: Value<'a>) -> Self {
        unsafe { std::mem::transmute::<Value<'a>, JSValue>(value) }
    }
}

impl<'a> From<JSValue> for Value<'a> {
    #[inline(always)]
    fn from(value: JSValue) -> Self {
        unsafe { std::mem::transmute::<JSValue, Value<'a>>(value) }
    }
}

impl<'a> std::fmt::Debug for Value<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.inner)
    }
}
