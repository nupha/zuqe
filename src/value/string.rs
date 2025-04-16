use std::{
    borrow::Cow,
    cell::Cell,
    ffi::c_void,
    ffi::{c_char, CStr},
    rc::Rc,
};

use crate::{
    atom::{consts::*, JSAtom, JSAtomInfo, __JS_AtomToUInt32},
    class::JSClassExoticMethods,
    context::JSContext,
    function::JS_InvokeFree,
    runtime::JSRuntime,
    types::{list_head, unlikely},
    value::{
        JSTag, JSValue, JSValueConst, JS_FreeValue, HINT_STRING, JS_TAG_NULL, JS_TAG_STRING,
        JS_TAG_SYMBOL, JS_TAG_UNDEFINED, JS_VALUE_GET_BOOL, JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT,
        JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_PTR, JS_VALUE_GET_STRING,
        JS_VALUE_GET_TAG,
    },
    JS_MKPTR,
};

use super::{
    js_dtoa,
    object::{
        check_define_prop_flags, JSPropertyDescriptor, JS_PROP_ENUMERABLE, JS_PROP_HAS_VALUE,
        JS_PROP_NO_EXOTIC,
    },
    primitives::JS_ToPrimitive,
    StringContent, JS_DTOA_VAR_FORMAT,
};

/// Maximum length allowed for JavaScript strings (1GB - 1)
pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

/// Represents a JavaScript string with reference counting and atom support
///
/// Memory Layout:
/// - 32-bit reference count header for GC
/// - 32-bit hash and type field (30 bits hash + 2 bits atom type)
/// - 32-bit hash next for symbol atom indexing
/// - String content (either UTF-8 or UTF-16 encoded)
///
/// The string can be:
/// - A regular string with dynamic content
/// - An atom string (interned for efficiency)
/// - A symbol string (unique identifier)
#[derive(Debug)]
#[repr(C)]
pub struct JSString {
    // Hash value
    hash_and_type: Cell<u32>,

    /// atom info of string, None if not registered as atom.
    atom_type: Cell<Option<JSAtomInfo>>,

    /// string content
    content: Rc<StringContent>,

    #[cfg(feature = "dump_leaks")]
    pub link: crate::types::list_head, // For tracking string allocations
}

impl JSString {
    /// create new jsstring from Rc<StringContent>
    #[inline]
    pub fn new(content: Rc<StringContent>) -> Rc<Self> {
        Rc::new(JSString {
            hash_and_type: Cell::new(0),
            atom_type: Cell::new(None),
            content,

            #[cfg(feature = "dump_leaks")]
            link: list_head::default(),
        })
    }

    /// Creates a JSString from a Rust string slice
    ///
    /// Efficiently creates a string from UTF-8 encoded Rust str.
    /// The string content is copied to ensure JavaScript string immutability.
    ///
    /// # Arguments
    /// * `rt` - JavaScript runtime for memory allocation
    /// * `str` - Source Rust string slice
    ///
    /// # Returns
    /// * `Ok(Box<JSString>)` - Newly created string
    /// * `Err(-1)` - Memory allocation or UTF-8 validation failed
    #[inline(always)]
    pub fn from_str(str: &str) -> Result<Rc<Self>, i32> {
        StringContent::from_str(str)
            .map(|s| s.into())
            .map_err(|_| -1)
    }

    /// Creates a JSString from UTF-8 encoded bytes
    ///
    /// Handles both ASCII and non-ASCII UTF-8 content:
    /// - ASCII content is stored as-is in UTF-8 format
    /// - Non-ASCII content is converted to UTF-16 for better Unicode support
    ///
    /// # Arguments
    /// * `rt` - JavaScript runtime for memory allocation
    /// * `data` - UTF-8 encoded byte vector
    ///
    /// # Returns
    /// * `Ok(Box<JSString>)` - Newly created string
    /// * `Err(-1)` - Memory allocation or UTF-8 validation failed
    pub fn from_utf8(data: Vec<u8>) -> Result<Rc<Self>, i32> {
        StringContent::from_utf8(data)
            .map(|s| s.into())
            .map_err(|_| -1)
    }

    /// create a new jsstring copying content from an other jsstring
    #[inline(always)]
    pub fn copy_from(str: &JSString) -> Result<Rc<Self>, i32> {
        str.content.copy_to_new().map(|s| s.into()).map_err(|_| -1)
    }

    /// Returns a reference to the underlying string content
    /// Used for string operations and comparisons
    #[inline(always)]
    pub const fn content(&self) -> &Rc<StringContent> {
        &self.content
    }

    /// Returns the length of the string in code units
    /// For UTF-8 strings, returns byte length
    /// For UTF-16 strings, returns number of 16-bit units
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// Checks if the string is empty (zero length)
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Checks if the string uses UTF-16 encoding
    /// Returns true for strings containing non-ASCII characters
    /// Returns false for ASCII-only strings (stored as UTF-8)
    #[inline(always)]
    pub fn is_wide_char(&self) -> bool {
        self.content.is_utf16()
    }

    /// Get hash value of the string.
    #[inline(always)]
    pub fn hash(&self) -> u32 {
        self.hash_and_type.get()
    }

    /// Save hash value
    pub fn set_hash(&self, h: u32) {
        self.hash_and_type.set(h);
    }

    /// Returns the atom type and atom id of the string.
    ///
    /// The atom type indicates whether this string is:
    /// - A regular string (None)
    /// - An interned string atom (String)
    /// - A unique symbol (Symbol)
    #[inline(always)]
    pub fn atom_info(&self) -> Option<JSAtomInfo> {
        self.atom_type.get()
    }

    /// Sets the atom type
    /// Used when converting a string to an atom or symbol.
    pub fn set_atom_type(&self, t: JSAtomInfo) {
        debug_assert!(self.atom_type.get().is_none());
        self.atom_type.set(Some(t));
    }

    /// consume Rc<JSString>, make into JSValue
    #[inline(always)]
    pub fn into_value(self: Rc<Self>) -> JSValue {
        // leak Rc<JSString> raw pointer.
        JS_MKPTR!(JSTag::String, Rc::into_raw(self).cast::<c_void>() as _)
    }
}

/// Convert StringContent to Rc<JSString>
impl From<StringContent> for Rc<JSString> {
    #[inline(always)]
    fn from(value: StringContent) -> Self {
        JSString::new(Rc::new(value))
    }
}

/// Convert an Rc<JSString> into JSValue
impl From<Rc<JSString>> for JSValue {
    #[inline(always)]
    fn from(value: Rc<JSString>) -> Self {
        value.into_value()
    }
}

impl JSContext {
    /// create string value from string content.
    pub unsafe fn create_string_from_content(&mut self, content: StringContent) -> JSValue {
        if content == StringContent::Empty {
            unsafe { JS_ATOM_empty_string.to_value(self, true) }
        } else {
            let s: Rc<JSString> = content.into();
            s.into_value()
        }
    }

    /// create string jsvalue from an utf8 encoded str.
    pub fn create_string(&mut self, str: &str) -> JSValue {
        if str.is_empty() {
            self.rt()
                .get_atom_desc(JS_ATOM_empty_string)
                .cloned()
                .unwrap()
                .into_value()
        } else if str.len() > JS_STRING_LEN_MAX {
            unsafe { self.JS_ThrowInternalError(c"string too long") }
        } else if let Ok(ss) = JSString::from_str(str) {
            ss.into_value()
        } else {
            unsafe {
                self.JS_ThrowOutOfMemory();
            }
            JSValue::EXCEPTION
        }
    }

    /// create string value from a single u16 char.
    /// returns JS_EXCEPTION if fail.
    pub unsafe fn create_string_from_char(&mut self, ch: char) -> JSValue {
        self.create_string_from_content(if ch.is_ascii() {
            StringContent::Utf8(vec![ch as u8])
        } else {
            StringContent::Utf16(ch.encode_utf16(&mut [0; 2]).to_vec())
        })
    }

    /// create string jsvalue from an utf8 encoded str.
    pub fn create_string_from_utf8(&mut self, data: Vec<u8>) -> JSValue {
        if data.is_empty() {
            unsafe { JS_ATOM_empty_string.to_value(self, true) }
        } else if data.len() > JS_STRING_LEN_MAX {
            unsafe { self.JS_ThrowInternalError(c"string too long") }
        } else {
            if let Ok(ss) = JSString::from_utf8(data) {
                ss.into_value()
            } else {
                unsafe {
                    self.JS_ThrowOutOfMemory();
                }
                JSValue::EXCEPTION
            }
        }
    }
}

/* return < 0, 0 or > 0 */
#[deprecated(note = "use StringContent::cmp() instead")]
pub unsafe fn js_string_compare(
    ctx: *mut JSContext,
    p1: *const JSString,
    p2: *const JSString,
) -> i32 {
    match (*p1).content().cmp(p2.as_ref().unwrap().content()) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

unsafe fn memcmp16(a: *const u16, b: *const u16, len: usize) -> i32 {
    for i in 0..len {
        let ai = *a.add(i);
        let bi = *b.add(i);
        if ai != bi {
            return ai as i32 - bi as i32;
        }
    }
    0
}

unsafe fn memcmp16_8(a: *const u16, b: *const u8, len: usize) -> i32 {
    for i in 0..len {
        let ai = *a.add(i);
        let bi = *b.add(i) as u16;
        if ai != bi {
            return ai as i32 - bi as i32;
        }
    }
    0
}

// #[inline(always)]
// pub unsafe fn hash_string8(str: &str, mut h: u32) -> u32 {
//     for c in str.as_bytes() {
//         h = hash_char(char::from_u32_unchecked((*c) as u32), h);
//     }
//     h
// }

// #[inline(always)]
// pub unsafe fn hash_string16(str: &[u16], mut h: u32) -> u32 {
//     for c in str {
//         h = hash_char(char::from_u32_unchecked(*c as u32), h);
//     }
//     h
// }

#[inline(always)]
pub fn is_num(c: i32) -> bool {
    ('0' as i32..='9' as i32).contains(&c)
}

#[deprecated(note = "use StringContent::parse_u32() instead")]
pub unsafe fn is_num_string(pval: &mut u32, p: *const JSString) -> bool {
    match (*p).content().parse_u32() {
        Ok(n) => {
            *pval = n;
            true
        }
        Err(_) => false,
    }
}

/// `val` keeps ref count unchanged after calling this function.
pub unsafe fn JS_ToStringInternal(
    ctx: *mut JSContext,
    val: JSValueConst,
    is_ToPropertyKey: bool,
) -> JSValue {
    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::String => val.dup(),
        JSTag::Int => (*ctx).create_string(&format!("{}", JS_VALUE_GET_INT(val))),
        JSTag::Bool => if JS_VALUE_GET_BOOL(val) {
            JS_ATOM_true
        } else {
            JS_ATOM_false
        }
        .to_value(ctx.as_mut().unwrap(), true),
        JSTag::Null => JS_ATOM_null.to_value(ctx.as_mut().unwrap(), true),
        JSTag::Undefined => JS_ATOM_undefined.to_value(ctx.as_mut().unwrap(), true),
        JSTag::Exception => JSValue::EXCEPTION,
        JSTag::Object => {
            let val1 = JS_ToPrimitive(ctx, &val, HINT_STRING);
            if val1.is_exception() {
                val1
            } else {
                let ret = JS_ToStringInternal(ctx, val1, is_ToPropertyKey);
                JS_FreeValue(ctx, val1);
                ret
            }
        }
        JSTag::FunctionBytecode => (*ctx).create_string("[function bytecode]"),
        JSTag::Symbol => {
            if is_ToPropertyKey {
                val.dup()
            } else {
                ctx.as_mut()
                    .unwrap()
                    .JS_ThrowTypeError(c"cannot convert symbol to string")
            }
        }
        JSTag::Float64 => js_dtoa(ctx, JS_VALUE_GET_FLOAT64(val), 10, 0, JS_DTOA_VAR_FORMAT),
        JSTag::Module => (*ctx).create_string("[module]"),
        JSTag::Uninitialized => (*ctx).create_string("[uninitialized]"),
        JSTag::CatchOffset => (*ctx).create_string("[cacthoffset]"),
        JSTag::BigDecimal | JSTag::BigFloat | JSTag::BigInt => {
            unimplemented!()
        }
    }
}

/// `val` keeps ref count unchanged after calling this function.
pub unsafe fn JS_ToString(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    JS_ToStringInternal(ctx, val, false)
}

/// `val` is consumed once.
pub unsafe fn JS_ToStringFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    let ret = JS_ToString(ctx, val);
    JS_FreeValue(ctx, val);
    ret
}

pub unsafe fn JS_ToLocaleStringFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    if val.is_undefined() || val.is_null() {
        JS_ToStringFree(ctx, val)
    } else {
        JS_InvokeFree(
            ctx.as_mut().unwrap(),
            val,
            JS_ATOM_toLocaleString,
            0,
            std::ptr::null_mut(),
        )
    }
}

pub unsafe fn JS_ToStringCheckObject(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    if matches!(JS_VALUE_GET_TAG(val), JS_TAG_NULL | JS_TAG_UNDEFINED) {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"null or undefined are forbidden")
    } else {
        JS_ToString(ctx, val)
    }
}

#[inline(always)]
pub unsafe fn JS_IsEmptyString(v: JSValueConst) -> bool {
    JS_VALUE_GET_TAG(v) == JS_TAG_STRING && (*JS_VALUE_GET_STRING(v)).is_empty()
}

/// Note: string value `str2` is consumed inside.
pub unsafe fn JS_ConcatString3(
    ctx: *mut JSContext,
    str1: &CStr,
    mut str2: JSValue,
    str3: &CStr,
) -> JSValue {
    macro_rules! fail {
        () => {
            JS_FreeValue(ctx, str2);
            return JSValue::EXCEPTION;
        };
    }

    if unlikely(JS_VALUE_GET_TAG(str2) != JS_TAG_STRING) {
        str2 = JS_ToStringFree(ctx, str2);
        if str2.is_exception() {
            fail!();
        }
    }

    let p = JS_VALUE_GET_STRING(str2);
    let len1 = str1.count_bytes();
    let len3 = str3.count_bytes();

    let mut res_string = if let Ok(s) =
        StringContent::with_capacity(len1 + (*p).len() + len3, (*p).is_wide_char())
    {
        s
    } else {
        fail!();
    };

    let r = res_string
        .push_str(str1.to_string_lossy().as_ref())
        .and_then(|_| res_string.push((*p).content()))
        .and_then(|_| res_string.push_str(str3.to_string_lossy().as_ref()));

    JS_FreeValue(ctx, str2);

    if r.is_err() {
        fail!();
    }

    (*ctx).create_string_from_content(res_string)
}

/// op1 and op2 are converted to strings.
/// For convience, op1 or op2 = JS_EXCEPTION are accepted and return JSValue::EXCEPTION.
/// op1 and op2 are consumed
pub unsafe fn JS_ConcatString(ctx: *mut JSContext, op1: JSValue, op2: JSValue) -> JSValue {
    let str1 = if JS_VALUE_GET_TAG(op1) != JS_TAG_STRING {
        let v = JS_ToStringFree(ctx, op1);
        if v.is_exception() {
            JS_FreeValue(ctx, op2);
            return JSValue::EXCEPTION;
        }
        v
    } else {
        op1
    };

    let str2 = if JS_VALUE_GET_TAG(op2) != JS_TAG_STRING {
        let v = JS_ToStringFree(ctx, op2);
        if v.is_exception() {
            JS_FreeValue(ctx, str1);
            return JSValue::EXCEPTION;
        }
        v
    } else {
        op2
    };

    let p1 = Rc::from_raw(JS_VALUE_GET_STRING(str1));
    let p2 = Rc::from_raw(JS_VALUE_GET_STRING(str2));

    if p1.is_empty() {
        return p2.into_value();
    } else if p2.is_empty() {
        return p1.into_value();
    }

    match Rc::try_unwrap(p1) {
        Ok(mut ss) => {
            // p1 ref count == 1
            match Rc::try_unwrap(ss.content) {
                Ok(mut cc) => {
                    if cc.push(&p2.content).is_err() {
                        ss.content = Rc::new(cc);
                        (*ctx).JS_ThrowOutOfMemory();
                        JSValue::EXCEPTION
                    } else {
                        (*ctx).create_string_from_content(cc)
                    }
                }
                Err(cc) => {
                    if let Ok(mut cc2) = cc.copy_to_new() {
                        if cc2.push(p2.content()).is_err() {
                            (*ctx).JS_ThrowOutOfMemory();
                            JSValue::EXCEPTION
                        } else {
                            (*ctx).create_string_from_content(cc2)
                        }
                    } else {
                        (*ctx).JS_ThrowOutOfMemory();
                        JSValue::EXCEPTION
                    }
                }
            }
        }
        Err(ss) => {
            if let Ok(new_sc) = ss.content().copy_to_new().and_then(|mut s| {
                s.push(p2.content())?;
                Ok(s)
            }) {
                (*ctx).create_string_from_content(new_sc)
            } else {
                JSValue::EXCEPTION
            }
        }
    }
}

#[deprecated(note = "use JSContext::create_string() instead")]
pub unsafe fn JS_NewStringLen(ctx: *mut JSContext, buf: *const u8, buf_len: usize) -> JSValue {
    (*ctx).create_string(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
        buf, buf_len,
    )))
}

#[deprecated(note = "use JSContext::create_string_utf8() instead")]
pub unsafe fn JS_NewString(ctx: *mut JSContext, str: &CStr) -> JSValue {
    (*ctx).create_string(str.to_string_lossy().as_ref())
}

impl JSValue {
    /// get a reference to value's string description. won't effect value's ref_count.
    /// will panic if value is not string type.
    #[inline(always)]
    pub unsafe fn string<'a>(self) -> &'a JSString {
        debug_assert!(matches!(
            JS_VALUE_GET_TAG(self),
            JS_TAG_STRING | JS_TAG_SYMBOL
        ));
        debug_assert!(!JS_VALUE_GET_PTR(self).is_null());

        JS_VALUE_GET_PTR(self).cast::<JSString>().as_ref().unwrap()
    }

    /// take string desciption out from value.
    /// will panic if value is not string type.
    pub fn take_string(self) -> Rc<JSString> {
        debug_assert!(matches!(
            JS_VALUE_GET_TAG(self),
            JS_TAG_STRING | JS_TAG_SYMBOL
        ));

        let p = JS_VALUE_GET_PTR(self).cast::<JSString>();
        debug_assert!(!p.is_null());
        unsafe { Rc::from_raw(p) }
    }

    /// run callback with value's string description. won't effect value' ref_count.
    /// will panic if value is not string type.
    pub fn with_string<R>(self, f: impl FnOnce(&Rc<JSString>) -> R) -> R {
        debug_assert_eq!(JS_VALUE_GET_TAG(self), JS_TAG_STRING);
        debug_assert!(!JS_VALUE_GET_PTR(self).is_null());

        let s = self.take_string();
        debug_assert!(Rc::strong_count(&s) > 0);

        let r = f(&s);

        // restore string desciption
        let _ = Rc::into_raw(s);

        r
    }

    /// Get utf8 string representation of JSValue `self`
    /// Note: the `self` value keep ref count unchanged after calling this function
    pub fn to_utf8_string(self, ctx: &mut JSContext) -> Result<Cow<'_, str>, i32> {
        if JS_VALUE_GET_TAG(self) == JSTag::String as i32 {
            let cc = self.with_string(|p| p.content().clone());
            debug_assert!(Rc::strong_count(&cc) > 1);

            let ret = match cc.to_str() {
                Cow::Borrowed(p) => {
                    // use string description content pointer.
                    Cow::Borrowed(unsafe { (p as *const str).as_ref().unwrap() })
                }
                Cow::Owned(t) => Cow::Owned(t),
            };

            Ok(ret)
        } else {
            let tmp = unsafe { JS_ToString(ctx, self) };
            if tmp.is_exception() {
                Err(-1)
            } else {
                Ok(Cow::Owned(
                    tmp.take_string().content().to_str().into_owned(),
                ))
            }
        }
    }
}

pub unsafe fn js_string_obj_get_length(obj: JSValueConst) -> usize {
    // This is a class exotic method: obj class_id is JS_CLASS_STRING
    let p = JS_VALUE_GET_OBJ(obj);

    if JS_VALUE_GET_TAG((*p).u.object_data) == JS_TAG_STRING {
        let p1 = JS_VALUE_GET_STRING((*p).u.object_data);
        (*p1).len()
    } else {
        0
    }
}

/// starts_with() equivelent
pub unsafe fn strstart(str: *const u8, val: *const u8, ptr: *mut *const u8) -> bool {
    let mut p = str;
    let mut q = val;
    while *q != 0 {
        if *p != *q {
            return false;
        }
        p = p.offset(1);
        q = q.offset(1);
    }
    if !ptr.is_null() {
        *ptr = p;
    }
    true
}

//
// String class methods
//
pub unsafe fn js_string_get_own_property(
    ctx: *mut JSContext,
    desc: *mut JSPropertyDescriptor,
    obj: JSValueConst,
    prop: JSAtom,
) -> i32 {
    // This is a class exotic method: obj class_id is JS_CLASS_STRING
    let mut ret = 0;

    if prop.is_int_type() {
        let p = JS_VALUE_GET_OBJ(obj).as_ref().unwrap();
        if JS_VALUE_GET_TAG(p.u.object_data) == JS_TAG_STRING {
            let p1 = Rc::from_raw(JS_VALUE_GET_STRING(p.u.object_data));

            let idx = __JS_AtomToUInt32(prop);
            if idx < p1.len() as u32 {
                if !desc.is_null() {
                    let ch = p1.content().chars().nth(idx as usize).unwrap();
                    (*desc).flags = JS_PROP_ENUMERABLE as _;
                    (*desc).value = (*ctx).create_string_from_char(ch);
                    (*desc).getter = JSValue::UNDEFINED;
                    (*desc).setter = JSValue::UNDEFINED;
                }

                ret = 1;
            }

            let _ = Rc::into_raw(p1);
        }
    }

    ret
}

pub unsafe fn js_string_define_own_property(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: i32,
) -> i32 {
    macro_rules! fail {
        () => {
            return if (*ctx).JS_ThrowTypeErrorOrFalse(flags as _, c"property is not configurable") {
                0
            } else {
                -1
            };
        };
    }
    macro_rules! def {
        () => {
            return this_obj
                .define_property(
                    ctx.as_mut().unwrap(),
                    prop,
                    val,
                    getter,
                    setter,
                    flags as u32 | JS_PROP_NO_EXOTIC,
                )
                .map_or(-1, |b| b as i32);
        };
    }

    if prop.is_int_type() {
        let idx = __JS_AtomToUInt32(prop);
        let p = JS_VALUE_GET_OBJ(this_obj).as_mut().unwrap();
        if JS_VALUE_GET_TAG(p.u.object_data) != JS_TAG_STRING {
            def!();
        }

        let p1 = Rc::from_raw(JS_VALUE_GET_STRING(p.u.object_data));
        let len = p1.len();
        let nth_char = p1.content().chars().nth(idx as usize);
        let _ = Rc::into_raw(p1);

        if idx >= len as u32 {
            def!();
        }
        if !check_define_prop_flags(JS_PROP_ENUMERABLE, flags as _) {
            fail!();
        }

        // check that the same value is configured
        if flags & JS_PROP_HAS_VALUE as i32 != 0 {
            if JS_VALUE_GET_TAG(val) != JS_TAG_STRING {
                fail!();
            }

            let p2 = Rc::from_raw(JS_VALUE_GET_STRING(val));
            let len2 = p2.len();
            let nth_char2 = p2.content().chars().next();
            let _ = Rc::into_raw(p2);

            if len2 != 1 {
                fail!();
            }
            if nth_char != nth_char2 {
                fail!();
            }
        }
        1
    } else {
        def!();
    }
}

pub unsafe fn js_string_delete_property(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
) -> i32 {
    if prop.is_int_type() {
        let idx = __JS_AtomToUInt32(prop);
        if idx < js_string_obj_get_length(obj) as u32 {
            return 0;
        }
    }
    0
}

pub static js_string_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    get_own_property: Some(js_string_get_own_property),
    define_own_property: Some(js_string_define_own_property),
    delete_property: Some(js_string_delete_property),
    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
};
