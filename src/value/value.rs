use crate::{
    JS_CLASS_ERROR,
    atom::{AtomHintAndId, AtomTypeHint, JSAtomStruct},
    function::{AsyncObjPayload, Bytecode},
    module::ModuleDef,
    number::JSFloat64Union,
    object::JSObject,
    string::JSString,
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
#[derive(Debug, Clone)]
pub enum JSValue {
    UNDEFINED,
    NULL,
    Bool(bool),
    Int(i32),
    UNINITIALIZED,
    CatchOffset(i32),
    EXCEPTION,
    Float64(f64),
    String(JSString),
    Symbol(JSAtomStruct),
    Module(ModuleDef),
    Object(JSObject),
    Bytecode(Bytecode),

    /// Special for async state resume execution only
    AsyncStateResume(AsyncObjPayload),
}

impl PartialEq for JSValue {
    /// Compare two JSValue of same type
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::UNDEFINED, Self::UNDEFINED)
            | (Self::NULL, Self::NULL)
            | (Self::EXCEPTION, Self::EXCEPTION)
            | (Self::UNINITIALIZED, Self::UNINITIALIZED) => true,

            (Self::Bool(b0), Self::Bool(b1)) => *b0 == *b1,
            (Self::Int(i0), Self::Int(i1)) => *i0 == *i1,
            (Self::String(s0), Self::String(s1)) => s0.content() == s1.content(),
            (Self::Symbol(s0), Self::Symbol(s1)) => {
                match (s0.atom_info().unwrap(), s1.atom_info().unwrap()) {
                    (AtomHintAndId::GlobalSymbol(a0), AtomHintAndId::GlobalSymbol(a1)) => a0 == a1,
                    _ => false,
                }
            }
            (Self::Float64(f0), Self::Float64(f1)) => f0.eq(f1),
            (Self::Object(l0), Self::Object(r0)) => l0.id() == r0.id(),
            (Self::Bytecode(l0), Self::Bytecode(r0)) => l0.id() == r0.id(),
            (Self::Module(m0), Self::Module(m1)) => m0.id() == m1.id(),
            (Self::CatchOffset(a), Self::CatchOffset(b)) => *a == *b,

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

pub type JSValueConst = JSValue;

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
        if let Self::Object(obj) = self {
            obj.flags().is_htmldda()
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
    pub fn is_callable(&self) -> bool {
        self.object().map_or(false, JSObject::is_callable)
    }

    pub fn is_error(&self) -> bool {
        self.object()
            .map_or(false, |o| o.class_id() == JS_CLASS_ERROR)
    }

    /// if `self` is an exception, returns Err if yes; Otherwise returns Ok(self)
    #[inline]
    pub fn err(self) -> Result<Self, i32> {
        if self.is_exception() {
            Err(-1)
        } else {
            Ok(self)
        }
    }

    /// calculate hash for given `id`
    #[inline(always)]
    pub(crate) const fn id_to_hash(id: usize) -> u32 {
        (id * 3163) as u32
    }

    /// calculate hash for jsvalue
    pub fn hash(&self) -> u32 {
        let h: u32 = match self {
            Self::Object(o) => Self::id_to_hash(o.id()),
            Self::Int(d) => {
                let u = JSFloat64Union { uint64: *d as u64 };
                unsafe { (u.uint32[0] ^ u.uint32[1]) * 3163 }
            }
            Self::String(s) | Self::Symbol(s) => s.content().calc_hash(AtomTypeHint::String),
            Self::Float64(d) => {
                let mut d = *d;
                if d.is_nan() {
                    d = f64::NAN;
                }
                let u = JSFloat64Union { d };
                unsafe { (u.uint32[0] ^ u.uint32[1]) * 3163 }
            }
            Self::UNDEFINED | Self::NULL | Self::UNINITIALIZED | Self::EXCEPTION => 0,
            Self::Bool(b) => *b as u32,
            Self::Bytecode(f) => Self::id_to_hash(f.id()),
            Self::Module(m) => Self::id_to_hash(m.id()),
            Self::CatchOffset(i) => {
                let u = JSFloat64Union { uint64: *i as u64 };
                unsafe { (u.uint32[0] ^ u.uint32[1]) * 3163 }
            }
            Self::AsyncStateResume(f) => Self::id_to_hash(f.id()),
        };

        let tag = unsafe { *<*const _>::from(self).cast::<u8>() };
        h ^ (tag as u32) // XOR
    }
}

//
// constants values
//
pub const VALUE_UNDEFINED: JSValue = JSValue::UNDEFINED;
pub const VALUE_NULL: JSValue = JSValue::NULL;
pub const VALUE_TRUE: JSValue = JSValue::Bool(true);
pub const VALUE_FALSE: JSValue = JSValue::Bool(false);
pub const VALUE_ZERO: JSValue = JSValue::Int(0);

pub const HINT_STRING: i32 = 0;
pub const HINT_NUMBER: i32 = 1;
pub const HINT_NONE: i32 = 2;
pub const HINT_FORCE_ORDINARY: i32 = 0x01 << 4; /* don't try Symbol.toPrimitive */
