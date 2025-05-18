use std::rc::Rc;

use crate::{
    atom::JS_FreeAtomStruct,
    context::JSContext,
    error::JS_ThrowRangeError,
    gc::{free_zero_refcount, JSGCObjectHeader, JSGCPhaseEnum},
    runtime::JSRuntime,
    types::{list_add, list_del, unlikely, JSRefCountHeader},
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_PROXY, JS_MKVAL,
};

use super::{object::JSObject, string::JSString, JS_ToInt64Sat, MAX_SAFE_INTEGER};

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
        let tag = JS_VALUE_GET_TAG(v);
        tag == (JS_NAN >> 32)
    }

    #[inline(always)]
    pub fn JS_TAG_IS_FLOAT64(tag: i32) -> bool {
        tag - super::JS_TAG_FIRST >= super::JS_TAG_FLOAT64 - super::JS_TAG_FIRST
    }

    #[inline(always)]
    pub fn JS_VALUE_GET_TAG(v: JSValue) -> i32 {
        // ((v >> 32) & 0xFFFF_FFFF) as i32
        (v >> 32) as i32
    }

    #[inline(always)]
    pub fn JS_VALUE_GET_PTR(v: JSValue) -> *mut c_void {
        (v & 0xFFFF_FFFF) as *mut c_void
    }
}

#[cfg(feature = "nan_boxing")]
pub use nb_value::*;

#[cfg(not(feature = "nan_boxing"))]
mod nnb_value {
    use std::ffi::c_void;

    #[derive(Debug, Copy, Clone, Default)]
    pub struct JSValue {
        /// type tag
        pub tag: super::JSTag,
        /// payload
        pub u: JSValueUnion,
    }

    #[derive(Copy, Clone)]
    #[repr(C)]
    pub union JSValueUnion {
        pub int32: i32,
        pub float64: f64,
        pub ptr: *mut c_void,
    }

    impl Default for JSValueUnion {
        fn default() -> Self {
            Self {
                ptr: std::ptr::null_mut(),
            }
        }
    }

    impl std::fmt::Debug for JSValueUnion {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            unsafe {
                f.debug_struct("JSValueUnion")
                    .field("i32", &self.int32)
                    .field("f64", &self.float64)
                    .field("ptr", &self.ptr)
                    .finish()
            }
        }
    }

    #[macro_export]
    macro_rules! JS_MKVAL {
        ($tag:expr, $val:expr) => {
            crate::value::JSValue {
                tag: $tag,
                u: crate::value::JSValueUnion { int32: $val },
            }
        };
    }

    #[macro_export]
    macro_rules! JS_MKPTR {
        ($tag:expr, $ptr:expr) => {{
            crate::value::JSValue {
                tag: $tag,
                u: crate::value::JSValueUnion { ptr: $ptr },
            }
        }};
    }

    impl JSValue {
        pub const NAN: Self = Self {
            tag: super::JSTag::Float64,
            u: JSValueUnion { float64: f64::NAN },
        };
    }

    #[deprecated(note = "use JSValue::NAN instead")]
    pub const JS_NAN: JSValue = JSValue {
        tag: super::JSTag::Float64,
        u: JSValueUnion { float64: f64::NAN },
    };

    #[inline(always)]
    pub fn JS_VALUE_IS_NAN(v: JSValue) -> bool {
        union _U {
            d: f64,
            u64: u64,
        }
        if v.tag != super::JSTag::Float64 {
            false
        } else {
            unsafe {
                let u = _U { d: v.u.float64 };
                (u.u64 & 0x7fffffffffffffff) > 0x7ff0000000000000
            }
        }
    }

    #[inline(always)]
    pub const fn JS_VALUE_GET_FLOAT64(v: JSValue) -> f64 {
        unsafe { v.u.float64 }
    }

    #[inline(always)]
    pub const fn JS_TAG_IS_FLOAT64(tag: i32) -> bool {
        tag == super::JS_TAG_FLOAT64 as i32
    }

    #[inline(always)]
    pub const fn JS_VALUE_GET_TAG(v: JSValue) -> i32 {
        v.tag as i32
    }

    #[inline(always)]
    pub const fn JS_VALUE_GET_PTR(v: JSValue) -> *mut c_void {
        unsafe { v.u.ptr }
    }
}

#[cfg(not(feature = "nan_boxing"))]
pub use nnb_value::*;

pub type JSValueConst = JSValue;

pub const SIZEOF_JSVALUE: usize = std::mem::size_of::<JSValue>();

///////////////////

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i32)]
pub enum JSTag {
    BigDecimal = -11,
    BigInt = -10,
    BigFloat = -9,
    Symbol = -8,
    String = -7,
    Module = -3,
    FunctionBytecode = -2,
    Object = -1,

    // these types don't need ref count
    Int = 0,
    Bool = 1,
    Null = 2,
    Undefined = 3,
    Uninitialized = 4,
    CatchOffset = 5,
    Exception = 6,
    Float64 = 7,
}

impl Default for JSTag {
    fn default() -> Self {
        Self::Int
    }
}

pub const JS_TAG_BIG_DECIMAL: i32 = JSTag::BigDecimal as i32;
pub const JS_TAG_BIG_INT: i32 = JSTag::BigInt as i32;
pub const JS_TAG_BIG_FLOAT: i32 = JSTag::BigFloat as i32;
pub const JS_TAG_SYMBOL: i32 = JSTag::Symbol as i32;
pub const JS_TAG_STRING: i32 = JSTag::String as i32;
pub const JS_TAG_MODULE: i32 = JSTag::Module as i32;
pub const JS_TAG_FUNCTION_BYTECODE: i32 = JSTag::FunctionBytecode as i32;
pub const JS_TAG_OBJECT: i32 = JSTag::Object as i32;
pub const JS_TAG_INT: i32 = JSTag::Int as i32;
pub const JS_TAG_BOOL: i32 = JSTag::Bool as i32;
pub const JS_TAG_NULL: i32 = JSTag::Null as i32;
pub const JS_TAG_UNDEFINED: i32 = JSTag::Undefined as i32;
pub const JS_TAG_UNINITIALIZED: i32 = JSTag::Uninitialized as i32;
pub const JS_TAG_CATCH_OFFSET: i32 = JSTag::CatchOffset as i32;
pub const JS_TAG_EXCEPTION: i32 = JSTag::Exception as i32;
pub const JS_TAG_FLOAT64: i32 = JSTag::Float64 as i32;

pub const JS_TAG_FIRST: i32 = JSTag::BigDecimal as i32;
pub const JS_TAG_LAST: i32 = JSTag::Float64 as i32;

//
// CONSTANT JSVALUES
//
#[deprecated(note = "use JSValue::NULL instead")]
pub const JS_NULL: JSValue = JSValue::NULL;
#[deprecated(note = "use JSValue::UNDEFINED instead")]
pub const JS_UNDEFINED: JSValue = JS_MKVAL!(JSTag::Undefined, 0);
#[deprecated(note = "use JSValue::FLASE instead")]
pub const JS_FALSE: JSValue = JS_MKVAL!(JSTag::Bool, 0);
#[deprecated(note = "use JSValue::TRUE instead")]
pub const JS_TRUE: JSValue = JS_MKVAL!(JSTag::Bool, 1);
#[deprecated(note = "use JSValue::EXCEPTION instead")]
pub const JS_EXCEPTION: JSValue = JS_MKVAL!(JSTag::Exception, 0);

// Property definition types
pub const JS_DEF_CFUNC: u8 = 0;
pub const JS_DEF_CGETSET: u8 = 1;
pub const JS_DEF_CGETSET_MAGIC: u8 = 2;
pub const JS_DEF_PROP_STRING: u8 = 3;
pub const JS_DEF_PROP_INT32: u8 = 4;
pub const JS_DEF_PROP_INT64: u8 = 5;
pub const JS_DEF_PROP_DOUBLE: u8 = 6;
pub const JS_DEF_PROP_UNDEFINED: u8 = 7;
pub const JS_DEF_OBJECT: u8 = 8;
pub const JS_DEF_ALIAS: u8 = 9;

impl JSValue {
    //
    // CONSTANT JS VALUES
    //
    pub const NULL: JSValue = JS_MKVAL!(JSTag::Null, 0);
    pub const UNDEFINED: JSValue = JS_MKVAL!(JSTag::Undefined, 0);
    pub const FALSE: JSValue = JS_MKVAL!(JSTag::Bool, 0);
    pub const TRUE: JSValue = JS_MKVAL!(JSTag::Bool, 1);
    pub const EXCEPTION: JSValue = JS_MKVAL!(JSTag::Exception, 0);
    pub const ZERO: JSValue = JS_MKVAL!(JSTag::Int, 0);
    pub(crate) const UNINITIALIZED: JSValue = JS_MKVAL!(JSTag::Uninitialized, 0);

    /// set new value and returns the old value.
    #[inline(always)]
    pub fn replace(&mut self, new_val: JSValue) -> JSValue {
        std::mem::replace(self, new_val)
    }

    /// set new value and free the old value.
    #[inline(always)]
    pub fn replace_free(&mut self, rt: *mut JSRuntime, new_val: JSValue) {
        let v0 = std::mem::replace(self, new_val);
        unsafe {
            JS_FreeValueRT(rt, v0);
        }
    }

    /// increase value refcount
    #[inline(always)]
    pub fn dup(self) -> Self {
        if JS_VALUE_HAS_REF_COUNT(self) {
            match JS_VALUE_GET_TAG(self) {
                JS_TAG_STRING | JS_TAG_SYMBOL => unsafe {
                    Rc::increment_strong_count(self.string());
                },
                _ => {
                    let p = JS_VALUE_GET_PTR(self).cast::<JSRefCountHeader>();
                    unsafe {
                        p.as_mut().unwrap().ref_count += 1;
                    }
                }
            }
        }

        self
    }

    /// get value refcount
    pub fn ref_count(&self) -> Option<i32> {
        let v = *self;

        if JS_VALUE_HAS_REF_COUNT(v) {
            match JS_VALUE_GET_TAG(v) {
                JS_TAG_STRING | JS_TAG_SYMBOL => {
                    let p = unsafe { Rc::from_raw(v.string()) };
                    let r = Rc::strong_count(&p);
                    let _ = Rc::into_raw(p);
                    Some(r as i32)
                }
                _ => {
                    let p = JS_VALUE_GET_PTR(v).cast::<JSRefCountHeader>();
                    Some(unsafe { (*p).ref_count })
                }
            }
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn is_exception(&self) -> bool {
        JS_VALUE_GET_TAG(*self) == JS_TAG_EXCEPTION
    }

    #[inline(always)]
    pub fn is_null(&self) -> bool {
        JS_VALUE_GET_TAG(*self) == JS_TAG_NULL
    }

    #[inline(always)]
    pub fn is_undefined(&self) -> bool {
        JS_VALUE_GET_TAG(*self) == JS_TAG_UNDEFINED
    }

    #[inline(always)]
    pub fn is_undefined_or_null(&self) -> bool {
        matches!(JS_VALUE_GET_TAG(*self), JS_TAG_UNDEFINED | JS_TAG_NULL)
    }

    #[inline(always)]
    pub fn is_object(&self) -> bool {
        JS_VALUE_GET_TAG(*self) == JS_TAG_OBJECT
    }

    #[inline(always)]
    pub fn is_string(&self) -> bool {
        JS_VALUE_GET_TAG(*self) == JS_TAG_STRING
    }

    #[inline(always)]
    pub fn is_int(&self) -> bool {
        JS_VALUE_GET_TAG(*self) == JS_TAG_INT
    }

    /// Determine if value `self` can be called like a function, that means value is a:
    ///
    /// - Compiled bytecode functions
    /// - Proxy objects that wrapping a function
    /// - Objects of native class which has `call` handler
    pub fn is_callable(self, ctx: &JSContext) -> bool {
        if JS_VALUE_GET_TAG(self) != JS_TAG_OBJECT {
            false
        } else {
            let p = unsafe { self.object_unchecked() };
            match p.class_id() {
                JS_CLASS_BYTECODE_FUNCTION => true,
                JS_CLASS_PROXY => unsafe { (*p.u.proxy_data).is_func },
                cid => {
                    // check if class proto is callable
                    ctx.rt()
                        .class_array
                        .get(cid as usize)
                        .and_then(|c| c.call)
                        .is_some()
                }
            }
        }
    }

    /// check if self is an exception. returns Err if yes, otherwise returns Ok(value)
    #[inline(always)]
    pub fn check_exception(self) -> Result<Self, i32> {
        if self.is_exception() {
            Err(-1)
        } else {
            Ok(self)
        }
    }
}

/* same as JS_VALUE_GET_TAG, but return JS_TAG_FLOAT64 with NaN boxing */
#[inline(always)]
pub fn JS_VALUE_GET_NORM_TAG(v: JSValue) -> JSTag {
    let tag = JS_VALUE_GET_TAG(v);
    if JS_TAG_IS_FLOAT64(tag) {
        JSTag::Float64
    } else {
        debug_assert!(
            (JS_TAG_FIRST..JS_TAG_LAST).contains(&tag),
            "unknown value tag {tag}"
        );
        unsafe { std::mem::transmute::<i32, JSTag>(tag) }
    }
}

#[inline(always)]
pub fn JS_VALUE_HAS_REF_COUNT(v: JSValue) -> bool {
    (JS_VALUE_GET_TAG(v) as u32) >= JS_TAG_FIRST as u32
    // JS_VALUE_GET_TAG(v) < 0
}

pub unsafe fn JS_VALUE_REF_COUNT(v: JSValue) -> Option<i32> {
    if JS_VALUE_HAS_REF_COUNT(v) {
        let p = JS_VALUE_GET_PTR(v).cast::<JSRefCountHeader>();
        Some((*p).ref_count)
    } else {
        None
    }
}

#[deprecated(note = "use JSValue::string() instead")]
#[inline(always)]
pub(crate) unsafe fn JS_VALUE_GET_STRING(v: JSValue) -> *mut JSString {
    v.string() as *const JSString as _
}

#[deprecated(note = "use JSValue::object() instead")]
#[inline(always)]
pub unsafe fn JS_VALUE_GET_OBJ(v: JSValue) -> *mut JSObject {
    v.object_unchecked() as *const JSObject as _
}

#[cfg(feature = "nan_boxing")]
#[inline(always)]
pub fn JS_VALUE_GET_INT(v: JSValue) -> i32 {
    v as i32
}

#[cfg(not(feature = "nan_boxing"))]
#[inline(always)]
pub fn JS_VALUE_GET_INT(v: JSValue) -> i32 {
    unsafe { v.u.int32 }
}

#[inline(always)]
pub fn JS_VALUE_GET_BOOL(v: JSValue) -> bool {
    JS_VALUE_GET_INT(v) != 0
}

#[inline(always)]
pub fn JS_FreeValue(ctx: *mut JSContext, v: JSValue) {
    unsafe { JS_FreeValueRT((*ctx).rt_mut(), v) }
}

#[inline]
pub unsafe fn JS_FreeValueRT(rt: *mut JSRuntime, v: JSValue) {
    if JS_VALUE_HAS_REF_COUNT(v) {
        match JS_VALUE_GET_TAG(v) {
            JS_TAG_STRING | JS_TAG_SYMBOL => {
                let s = Rc::from_raw(v.string());
                if Rc::strong_count(&s) == 1 {
                    // rc is about to drop, drop atom description too
                    if let Some(at) = s.atom_info() {
                        JS_FreeAtomStruct(rt, at.atom());
                    }
                }
            }
            _ => {
                let p = JS_VALUE_GET_PTR(v).cast::<JSRefCountHeader>();
                debug_assert!((*p).ref_count > 0);

                (*p).ref_count -= 1;
                if (*p).ref_count <= 0 {
                    __JS_FreeValueRT(rt, v);
                }
            }
        }
    }
}

/* called with the ref_count of 'v' reaches zero. */
#[inline(always)]
pub unsafe fn __JS_FreeValueRT(rt: *mut JSRuntime, v: JSValue) {
    let tag = JS_VALUE_GET_TAG(v);

    #[cfg(feature = "dump_free")]
    {
        print!("Freeing ({tag:2}) ");
        if tag == JS_TAG_OBJECT {
            JS_DumpObject(rt, JS_VALUE_GET_OBJ(v));
        } else {
            JS_DumpValueShort(rt, v);
            println!();
        }
    }

    match tag {
        JS_TAG_OBJECT | JS_TAG_FUNCTION_BYTECODE => {
            let p = JS_VALUE_GET_OBJ(v).cast::<JSGCObjectHeader>();
            if (*rt).gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
                list_del(&mut (*p).link);
                list_add(&mut (*p).link, &mut (*rt).gc_zero_ref_count_list);

                if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
                    free_zero_refcount(rt);
                }
            }
        }
        JS_TAG_STRING | JS_TAG_SYMBOL => {
            unreachable!("won't freed here");
        }
        JS_TAG_MODULE => {
            unreachable!("never freed here");
        }
        JS_TAG_BIG_INT | JS_TAG_BIG_FLOAT | JS_TAG_BIG_DECIMAL => {
            unimplemented!()
        }
        _ => {
            panic!("__JS_FreeValue: unknown tag={}", tag);
        }
    }
}

// #[inline(always)]
// pub unsafe fn __JS_FreeValue(ctx: &mut JSContext, v: JSValue) {
//     __JS_FreeValueRT(ctx.rt_mut(), v);
// }

#[inline]
pub fn JS_IsNumber(v: JSValueConst) -> bool {
    let tag = JS_VALUE_GET_TAG(v);
    (tag == JS_TAG_INT || JS_TAG_IS_FLOAT64(tag)) as _
}

#[inline]
#[allow(unused_variables)] // C2RS: Suppress unused parameter warning for ctx
pub fn JS_IsBigInt(ctx: *mut JSContext, v: JSValueConst) -> bool {
    let tag = JS_VALUE_GET_TAG(v);
    tag == JS_TAG_BIG_INT
}

#[inline]
pub fn JS_IsBigFloat(v: JSValueConst) -> bool {
    let tag = JS_VALUE_GET_TAG(v);
    tag == JS_TAG_BIG_FLOAT
}

#[inline]
pub fn JS_IsBigDecimal(v: JSValueConst) -> bool {
    let tag = JS_VALUE_GET_TAG(v);
    tag == JS_TAG_BIG_DECIMAL
}

#[inline]
pub fn JS_IsBool(v: JSValueConst) -> bool {
    JS_VALUE_GET_TAG(v) == JS_TAG_BOOL
}

#[deprecated(note = "use JSValue::is_null() instead")]
pub fn JS_IsNull(v: JSValueConst) -> bool {
    JS_VALUE_GET_TAG(v) == JS_TAG_NULL
}

#[deprecated(note = "use JSValue::is_undefined() instead")]
pub fn JS_IsUndefined(v: JSValueConst) -> bool {
    JS_VALUE_GET_TAG(v) == JS_TAG_UNDEFINED
}

#[deprecated(note = "use JSValue::is_exception() instead")]
pub fn JS_IsException(v: JSValueConst) -> bool {
    v.is_exception()
}

#[inline]
pub fn JS_IsUninitialized(v: JSValueConst) -> bool {
    unlikely(JS_VALUE_GET_TAG(v) == JS_TAG_UNINITIALIZED)
}

#[deprecated(note = "use JSValue::is_string() instead")]
#[inline]
pub fn JS_IsString(v: JSValueConst) -> bool {
    v.is_string()
}

#[inline]
pub fn JS_IsSymbol(v: JSValueConst) -> bool {
    JS_VALUE_GET_TAG(v) == JS_TAG_SYMBOL
}

#[deprecated(note = "use JSValue::is_object() instead")]
#[inline]
pub fn JS_IsObject(v: JSValueConst) -> bool {
    v.is_object()
}

#[inline]
pub fn JS_IsHTMLDDA(obj: JSValueConst) -> bool {
    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        false
    } else {
        unsafe { obj.object_unchecked().detailed.is_htmldda() }
    }
}

#[deprecated(note = "use JSValue::inc_ref_count() instead")]
#[inline(always)]
pub fn JS_DupValueRT(rt: *mut JSRuntime, v: JSValueConst) -> JSValue {
    v.dup()
}

#[deprecated(note = "use JSValue::dup() instead")]
pub unsafe fn JS_DupValue(ctx: *mut JSContext, v: JSValueConst) -> JSValue {
    v.dup()
}

/// set new value and free the old value.
/// (freeing the value can reallocate the object data)
#[deprecated(note = "use JSValue::replace_free() instead")]
#[inline(always)]
pub unsafe fn set_value(ctx: *mut JSContext, pval: *mut JSValue, new_val: JSValue) {
    (*pval).replace_free((*ctx).rt_mut(), new_val);
}

pub unsafe fn JS_ToIndex(ctx: *mut JSContext, plen: *mut u64, val: JSValueConst) -> i32 {
    let mut v: i64 = 0;
    if JS_ToInt64Sat(ctx, &mut v as *mut i64, val) != 0 {
        return -1;
    }
    if !(0..=MAX_SAFE_INTEGER).contains(&v) {
        JS_ThrowRangeError(ctx, c"invalid array index");
        *plen = 0;
        return -1;
    }
    *plen = v as u64;
    0
}

pub const HINT_STRING: i32 = 0;
pub const HINT_NUMBER: i32 = 1;
pub const HINT_NONE: i32 = 2;
pub const HINT_FORCE_ORDINARY: i32 = 0x01 << 4; /* don't try Symbol.toPrimitive */
