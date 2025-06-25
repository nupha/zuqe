use std::{
    borrow::Cow,
    cell::Cell,
    ffi::CStr,
    rc::{Rc, Weak},
};

use crate::{
    atom::{__JS_AtomToUInt32, JSAtom, JSAtomInfo, consts::*},
    class::JSClassExoticMethods,
    context::JSContext,
    error::JSResult,
    gc::GCObjectHeader,
    runtime::JSRuntime,
    value::{JS_FreeValue, JSValue, JSValueConst, object::GCObjectWeakref},
};

use super::{
    StringContent,
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JSPropertyDescriptor,
        check_define_prop_flags,
    },
};

/// Maximum length allowed for JavaScript strings (1GB - 1)
pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

pub type StringContentRc = Rc<StringContent>;

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
pub struct JSStringInner {
    header: GCObjectHeader,

    // Hash value
    hash_and_type: Cell<u32>,

    /// atom info of string, None if not registered as atom.
    atom_type: Cell<Option<JSAtomInfo>>,

    /// string content
    content: StringContentRc,

    /// For tracking string allocations
    #[cfg(feature = "dump_leaks")]
    pub link: crate::utils::list_head,
}

impl JSStringInner {
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// Returns a reference to the underlying string content
    /// Used for string operations and comparisons
    #[inline(always)]
    pub const fn content(&self) -> &StringContentRc {
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
}

#[derive(Debug, Clone)]
pub struct JSString(pub(crate) Rc<JSStringInner>);

/// Convert an Rc<JSStringInner> into JSValue
impl From<JSString> for JSValue {
    #[inline(always)]
    fn from(value: JSString) -> Self {
        Self::String(value)
    }
}

impl JSString {
    /// create JSString from content
    pub fn new(rt: &mut JSRuntime, content: StringContentRc) -> Self {
        let inner = JSStringInner {
            header: GCObjectHeader::new(rt, 1),
            hash_and_type: Cell::new(0),
            atom_type: Cell::new(None),
            content,

            #[cfg(feature = "dump_leaks")]
            link: crate::utils::list_head::default(),
        };

        let s = Self(Rc::new(inner));
        rt.gc_add_object(s.downgrade().into());
        s
    }

    /// Creates a JSStringInner from a Rust string slice
    ///
    /// Efficiently creates a string from UTF-8 encoded Rust str.
    /// The string content is copied to ensure JavaScript string immutability.
    ///
    /// # Arguments
    /// * `rt` - JavaScript runtime for memory allocation
    /// * `str` - Source Rust string slice
    ///
    /// # Returns
    /// * `Ok(Box<JSStringInner>)` - Newly created string
    /// * `Err(-1)` - Memory allocation or UTF-8 validation failed
    #[inline(always)]
    pub fn from_str(rt: &mut JSRuntime, str: &str) -> Result<Self, i32> {
        let sc = StringContent::from_str(str).map_err(|_| -1)?;
        Ok(Self::new(rt, Rc::new(sc)))
    }

    /// Creates a JSStringInner from UTF-8 encoded bytes
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
    /// * `Ok(Box<JSStringInner>)` - Newly created string
    /// * `Err(-1)` - Memory allocation or UTF-8 validation failed
    pub fn from_utf8(rt: &mut JSRuntime, data: Vec<u8>) -> Result<Self, i32> {
        let sc = StringContent::from_utf8(data).map_err(|_| -1)?;
        Ok(Self::new(rt, Rc::new(sc)))
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.id()
    }

    pub fn content(&self) -> &StringContentRc {
        self.0.content()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn is_wide_char(&self) -> bool {
        self.0.is_wide_char()
    }

    /// Get hash value of the string.
    #[inline(always)]
    pub fn hash(&self) -> u32 {
        self.0.hash()
    }

    /// Save hash value
    pub fn set_hash(&self, h: u32) {
        self.0.set_hash(h);
    }

    pub fn set_atom_type(&self, t: JSAtomInfo) {
        self.0.set_atom_type(t);
    }

    /// Returns the atom type and atom id of the string.
    ///
    /// The atom type indicates whether this string is:
    /// - A regular string (None)
    /// - An interned string atom (String)
    /// - A unique symbol (Symbol)
    #[inline(always)]
    pub fn atom_info(&self) -> Option<JSAtomInfo> {
        self.0.atom_info()
    }

    pub fn downgrade(&self) -> JSStringWeakref {
        JSStringWeakref(Rc::downgrade(&self.0))
    }

    /// get sub string in specified range, and return the sub string value.
    pub fn sub_string(&self, ctx: &JSContext, start: usize, end: usize) -> JSResult {
        let end = std::cmp::min(end, self.len());

        if start == 0 && end == self.len() {
            Ok(JSValue::String(self.clone()))
        } else {
            let count = if end > start { end - start } else { 0 };
            if count > 0 {
                let sub = self.content().slice(start..end).to_owned().map_err(|_| {
                    ctx.JS_ThrowOutOfMemory();
                    -1
                })?;
                ctx.create_string_from_content(sub).check_exception()
            } else {
                Ok(ctx.create_string_from_content(StringContent::Empty))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct JSStringWeakref(Weak<JSStringInner>);

impl From<JSStringWeakref> for GCObjectWeakref {
    fn from(value: JSStringWeakref) -> Self {
        Self::String(value)
    }
}

impl JSStringWeakref {
    pub fn upgrade(&self) -> Option<JSString> {
        self.0.upgrade().map(JSString)
    }
}

impl JSContext {
    /// create string value from string content.
    pub fn create_string_from_content(&self, content: StringContent) -> JSValue {
        if content == StringContent::Empty {
            JS_ATOM_empty_string.to_value(self, true)
        } else {
            let s = JSString::new(self.rt_mut(), Rc::new(content));
            JSValue::String(s)
        }
    }

    /// create string jsvalue from an utf8 encoded str.
    pub fn create_string(&self, str: &str) -> JSValue {
        if str.is_empty() {
            self.rt()
                .get_atom_desc(JS_ATOM_empty_string)
                .unwrap()
                .into()
        } else if str.len() > JS_STRING_LEN_MAX {
            self.JS_ThrowInternalError("string too long");
            JSValue::EXCEPTION
        } else if let Ok(ss) = JSString::from_str(self.rt_mut(), str) {
            JSValue::String(ss)
        } else {
            self.JS_ThrowOutOfMemory();
            JSValue::EXCEPTION
        }
    }

    /// create string value from a single u16 char.
    /// returns JS_EXCEPTION if fail.
    pub fn create_string_from_char(&self, ch: char) -> JSValue {
        self.create_string_from_content(if ch.is_ascii() {
            StringContent::Utf8(vec![ch as u8])
        } else {
            StringContent::Utf16(ch.encode_utf16(&mut [0; 2]).to_vec())
        })
    }

    /// create string jsvalue from an utf8 encoded str.
    pub fn create_string_from_utf8(&self, data: Vec<u8>) -> JSValue {
        if data.is_empty() {
            JS_ATOM_empty_string.to_value(self, true)
        } else if data.len() > JS_STRING_LEN_MAX {
            self.JS_ThrowInternalError("string too long");
            JSValue::EXCEPTION
        } else {
            if let Ok(ss) = JSString::from_utf8(self.rt_mut(), data) {
                JSValue::String(ss)
            } else {
                self.JS_ThrowOutOfMemory();
                JSValue::EXCEPTION
            }
        }
    }
}

/* return < 0, 0 or > 0 */
#[deprecated(note = "use StringContent::cmp() instead")]
pub unsafe fn js_string_compare(ctx: &JSContext, p1: &JSString, p2: &JSString) -> i32 {
    match p1.content().cmp(p2.content()) {
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
pub unsafe fn is_num_string(pval: &mut u32, p: *const JSStringInner) -> bool {
    match (*p).content().parse_u32() {
        Ok(n) => {
            *pval = n;
            true
        }
        Err(_) => false,
    }
}

/// `val` is consumed.
#[deprecated(note = "use JSValue::coerce_to_string_value() + JS_FreeValue() instead")]
pub fn JS_ToStringFree(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_string_value(ctx)
        .unwrap_or(JSValue::EXCEPTION)
}

/// `val` is consumed.
#[deprecated(note = "use JSValue::to_locale_string() + JS_FreeValue() instead")]
pub unsafe fn JS_ToLocaleStringFree(ctx: &JSContext, val: JSValue) -> JSValue {
    let r = val.to_locale_string(ctx).unwrap_or(JSValue::EXCEPTION);
    r
}

pub fn JS_ToStringCheckObject(ctx: &JSContext, val: JSValueConst) -> JSResult {
    if val.is_undefined_or_null() {
        ctx.JS_ThrowTypeError("null or undefined are forbidden");
        Err(-1)
    } else {
        val.coerce_to_string_value(ctx)
    }
}

pub fn JS_IsEmptyString(v: &JSValue) -> bool {
    if let JSValue::String(s) = v {
        s.is_empty()
    } else {
        false
    }
}

/// Note: string value `str2` is consumed inside.
pub unsafe fn JS_ConcatString3(
    ctx: &JSContext,
    str1: &CStr,
    mut str2: JSValue,
    str3: &CStr,
) -> JSValue {
    macro_rules! fail {
        () => {
            JS_FreeValue(ctx, &str2);
            return JSValue::EXCEPTION;
        };
    }

    if !matches!(str2, JSValue::String(_)) {
        str2 = JS_ToStringFree(ctx, str2);
        if str2.is_exception() {
            fail!();
        }
    }

    let p = str2.string_unchecked();
    let len1 = str1.count_bytes();
    let len3 = str3.count_bytes();

    let mut res_string =
        if let Ok(s) = StringContent::with_capacity(len1 + (*p).len() + len3, p.is_wide_char()) {
            s
        } else {
            fail!();
        };

    let r = res_string
        .push_str(str1.to_string_lossy().as_ref())
        .and_then(|_| res_string.push((*p).content()))
        .and_then(|_| res_string.push_str(str3.to_string_lossy().as_ref()));

    JS_FreeValue(ctx, &str2);

    if r.is_err() {
        fail!();
    }

    ctx.create_string_from_content(res_string)
}

/// op1 and op2 are converted to strings.
/// For convience, op1 or op2 = JS_EXCEPTION are accepted and return JSValue::EXCEPTION.
/// op1 and op2 are consumed
pub unsafe fn JS_ConcatString(ctx: &JSContext, op1: JSValue, op2: JSValue) -> JSValue {
    let str1 = if !op1.is_string() {
        let v = JS_ToStringFree(ctx, op1);
        if v.is_exception() {
            JS_FreeValue(ctx, &op2);
            return JSValue::EXCEPTION;
        }
        v
    } else {
        op1
    };

    let str2 = if !op2.is_string() {
        let v = JS_ToStringFree(ctx, op2);
        if v.is_exception() {
            JS_FreeValue(ctx, &str1);
            return JSValue::EXCEPTION;
        }
        v
    } else {
        op2
    };

    let p1 = str1.into_string_unchecked();
    let p2 = str2.into_string_unchecked();

    if p1.is_empty() {
        return p2.clone().into();
    } else if p2.is_empty() {
        return p1.clone().into();
    }

    match Rc::try_unwrap(p1.0) {
        Ok(mut ss) => {
            // p1 ref count == 1
            match Rc::try_unwrap(ss.content) {
                Ok(mut cc) => {
                    if cc.push(p2.content()).is_err() {
                        ss.content = Rc::new(cc);
                        ctx.JS_ThrowOutOfMemory();
                        JSValue::EXCEPTION
                    } else {
                        ctx.create_string_from_content(cc)
                    }
                }
                Err(cc) => {
                    if let Ok(mut cc2) = cc.copy_to_new() {
                        if cc2.push(p2.content()).is_err() {
                            ctx.JS_ThrowOutOfMemory();
                            JSValue::EXCEPTION
                        } else {
                            ctx.create_string_from_content(cc2)
                        }
                    } else {
                        ctx.JS_ThrowOutOfMemory();
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
                ctx.create_string_from_content(new_sc)
            } else {
                JSValue::EXCEPTION
            }
        }
    }
}

#[deprecated(note = "use JSContext::create_string() instead")]
pub unsafe fn JS_NewStringLen(ctx: &JSContext, buf: *const u8, buf_len: usize) -> JSValue {
    ctx.create_string(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
        buf, buf_len,
    )))
}

#[deprecated(note = "use JSContext::create_string_utf8() instead")]
pub unsafe fn JS_NewString(ctx: &JSContext, str: &CStr) -> JSValue {
    ctx.create_string(str.to_string_lossy().as_ref())
}

impl JSValue {
    /// get a reference to value's string description. won't effect value's ref_count.
    /// will panic if value is not string type.
    #[inline(always)]
    pub unsafe fn string_unchecked(&self) -> &JSString {
        if let Self::String(s) | Self::Symbol(s) = self {
            s
        } else {
            unreachable!()
        }
    }

    /// get a reference to value's string description.
    #[inline(always)]
    pub fn string(&self) -> Option<&JSString> {
        if let Self::String(s) | Self::Symbol(s) = self {
            Some(s)
        } else {
            None
        }
    }

    /// take string description out from value.
    /// will panic if value is not string or symbol.
    pub unsafe fn into_string_unchecked(self) -> JSString {
        if let Self::String(s) | Self::Symbol(s) = self {
            s
        } else {
            unreachable!()
        }
    }

    /// Coerce value `self` to utf8 string.
    /// Note: `self` is not consumed.
    pub fn coerce_to_utf8(&self, ctx: &JSContext) -> Result<Cow<str>, i32> {
        if let Some(p) = self.string() {
            let cc = p.content();

            let ret = match cc.to_str() {
                Cow::Borrowed(str) => {
                    // use string description content pointer.
                    Cow::Borrowed(unsafe { (str as *const str).as_ref().unwrap() })
                }
                Cow::Owned(t) => Cow::Owned(t),
            };
            Ok(ret)
        } else {
            let str_val = self.coerce_to_string_value(ctx)?;
            Ok(Cow::Owned(unsafe {
                str_val
                    .into_string_unchecked()
                    .content()
                    .to_str()
                    .into_owned()
            }))
        }
    }
}

pub unsafe fn js_string_obj_get_length(obj: JSValueConst) -> usize {
    // This is a class exotic method: obj class_id is JS_CLASS_STRING
    if let Some(v) = obj.object_unchecked().0.isa.borrow().value() {
        if v.is_string() {
            v.string_unchecked().len()
        } else {
            0
        }
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
pub fn js_string_get_own_property(
    ctx: &JSContext,
    obj: JSValueConst,
    prop: JSAtom,
) -> Result<Option<JSPropertyDescriptor>, i32> {
    // This is a class exotic method: obj class_id is JS_CLASS_STRING
    if prop.is_int_type() {
        if let JSValue::String(p1) = unsafe { obj.object_unchecked() }
            .0
            .isa
            .borrow()
            .value()
            .unwrap()
        {
            let idx = __JS_AtomToUInt32(prop);
            if idx < p1.len() as u32 {
                let ch = p1.content().chars().nth(idx as usize).unwrap();
                return Ok(Some(JSPropertyDescriptor {
                    flags: JS_PROP_ENUMERABLE as _,
                    value: ctx.create_string_from_char(ch),
                    getter: JSValue::UNDEFINED,
                    setter: JSValue::UNDEFINED,
                }));
            }
        }
    }

    Ok(None)
}

pub unsafe fn js_string_define_own_property(
    ctx: &JSContext,
    this_obj: &JSValueConst,
    prop: JSAtom,
    val: &JSValueConst,
    getter: &JSValueConst,
    setter: &JSValueConst,
    flags: i32,
) -> i32 {
    macro_rules! fail {
        () => {
            return if ctx.JS_ThrowTypeErrorOrFalse(flags as _, "property is not configurable") {
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
                    ctx,
                    prop,
                    &val,
                    &getter,
                    &setter,
                    flags as u32 | JS_PROP_NO_EXOTIC,
                )
                .map_or(-1, |b| b as i32);
        };
    }

    if prop.is_int_type() {
        let idx = __JS_AtomToUInt32(prop);

        if let JSValue::String(p1) = this_obj.object_unchecked().0.isa.borrow().value().unwrap() {
            let len = p1.len();
            let nth_char = p1.content().chars().nth(idx as usize);

            if idx >= len as u32 {
                def!();
            }
            if !check_define_prop_flags(JS_PROP_ENUMERABLE, flags as _) {
                fail!();
            }

            // check that the same value is configured
            if flags & JS_PROP_HAS_VALUE as i32 != 0 {
                if !val.is_string() {
                    fail!();
                }

                let p2 = val.string_unchecked();
                let len2 = p2.len();
                let nth_char2 = p2.content().chars().next();

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
    } else {
        def!();
    }
}

pub fn js_string_delete_property(
    ctx: &JSContext,
    obj: JSValueConst,
    prop: JSAtom,
) -> Result<bool, i32> {
    if prop.is_int_type() {
        let idx = __JS_AtomToUInt32(prop);
        if idx < unsafe { js_string_obj_get_length(obj) as u32 } {
            return Ok(false);
        }
    }

    Ok(true)
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
