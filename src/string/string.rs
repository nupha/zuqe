use std::{
    cell::Cell,
    cmp::Ordering,
    ffi::CStr,
    ops::Range,
    rc::{Rc, Weak},
};

use crate::{
    JS_CLASS_STRING,
    atom::{AtomHintAndId, JSAtom, consts::*},
    class::JSClassExoticMethods,
    context::JSContext,
    gc::GCObjectHeader,
    number::{ATOD_ACCEPT_BIN_OCT, f64_stringify, js_atof},
    object::{
        GCObjectWeakref, JS_PROP_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JSObject,
        JSPropertyDescriptor, ObjectPayload, check_define_prop_flags,
    },
    runtime::JSRuntime,
    string::{StringChar, StringContent},
    value::{HINT_STRING, JSValue},
};

use super::StringBuf;

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
pub(crate) struct JSStringInner {
    header: GCObjectHeader,

    /// when registered as atom, this is hash value for atom.
    /// 0 if string is not registered as atom.
    hash_and_type: Cell<u32>,
    /// atom info of string, None if not registered as atom.
    atom_type: Cell<Option<AtomHintAndId>>,

    /// string content
    content: StringContent,

    gc_mark: Cell<bool>,
}

impl PartialEq for JSStringInner {
    fn eq(&self, other: &Self) -> bool {
        self.content() == other.content()
    }
}

impl Eq for JSStringInner {}

impl PartialOrd for JSStringInner {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let a1 = self.atom_info();
        let a2 = other.atom_info();

        let r = match (a1, a2) {
            // symbol-atom vs non-atom: symbol is greater
            (None, Some(AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_))) => {
                Ordering::Less
            }
            (Some(AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_)), None) => {
                Ordering::Greater
            }

            // symbol-atom vs string-atom: symbol is greater
            (
                Some(AtomHintAndId::String(_) | AtomHintAndId::Private(_)),
                Some(AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_)),
            ) => Ordering::Less,
            (
                Some(AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_)),
                Some(AtomHintAndId::String(_) | AtomHintAndId::Private(_)),
            ) => Ordering::Greater,

            // non-atom vs non-atom
            // string-atom vs string-atom
            // symbol-atom vs symbol-atom
            // string-atom vs non-atom
            _ => self.content.cmp(&other.content),
        };

        Some(r)
    }
}

impl std::fmt::Debug for JSStringInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.content)
    }
}

impl JSStringInner {
    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        self.header.rt()
    }

    /// Returns a reference to the underlying string content
    /// Used for string operations and comparisons
    #[inline(always)]
    pub const fn content(&self) -> &StringContent {
        &self.content
    }

    /// count string chars (include broken surrogate code).
    #[inline(always)]
    pub fn chars_count(&self) -> usize {
        self.content.chars_count()
    }

    /// Returns the codepoint length
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// Checks if the string is empty (zero length)
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0 && !self.is_ident()
    }

    #[inline(always)]
    pub fn is_ident(&self) -> bool {
        self.content.is_ident()
    }

    /// Checks if the string uses UTF-16 encoding
    /// Returns true for strings containing non-ASCII characters
    /// Returns false for ASCII-only strings (stored as UTF-8)
    #[inline(always)]
    pub fn is_wide(&self) -> bool {
        self.content.is_wide()
    }

    /// Get hash value of the string.
    #[inline(always)]
    pub const fn hash(&self) -> u32 {
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
    pub const fn atom_info(&self) -> Option<AtomHintAndId> {
        self.atom_type.get()
    }

    /// Sets the atom type
    /// Used when converting a string to an atom or symbol.
    pub fn set_atom_type(&self, t: AtomHintAndId) {
        debug_assert!(self.atom_type.get().is_none());
        self.atom_type.set(Some(t));
    }
}

#[derive(Clone)]
pub struct JSString(pub(crate) Rc<JSStringInner>);

impl PartialEq for JSString {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.0.content() == other.0.content()
    }
}

impl From<JSString> for JSValue {
    #[inline(always)]
    fn from(value: JSString) -> Self {
        Self::String(value)
    }
}

impl TryFrom<JSObject> for JSString {
    type Error = i32;

    fn try_from(obj: JSObject) -> Result<Self, Self::Error> {
        if obj.class_id() == JS_CLASS_STRING {
            // JS_CLASS_STRING object contains string
            if let Some(JSValue::String(s)) = obj.0.payload.borrow().value() {
                Ok(s)
            } else {
                unreachable!()
            }
        } else {
            Err(-1)
        }
    }
}

impl TryFrom<JSValue> for JSString {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        match value {
            JSValue::String(s) => Ok(s),
            JSValue::Object(o) => o.try_into(),
            _ => Err(-1),
        }
    }
}

impl std::fmt::Display for JSString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.content().to_utf8();
        write!(f, "{s}")
    }
}

impl std::fmt::Debug for JSString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl JSString {
    /// create JSString with string content
    pub(crate) fn new(rt: &mut JSRuntime, content: StringContent) -> Self {
        let inner = JSStringInner {
            header: GCObjectHeader::new(rt, 1),
            hash_and_type: Cell::new(0),
            atom_type: Cell::new(None),
            content,
            gc_mark: Cell::new(false),
        };

        let s = Self(Rc::new(inner));
        //rt.gc_add_object(s.downgrade().into());
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
    pub(crate) fn from_str(rt: &mut JSRuntime, s: &str) -> Result<Self, i32> {
        Ok(Self::new(rt, StringBuf::from_str(s)?.into()))
    }

    /// Creates a JSString from UTF-8 encoded bytes
    ///
    /// Handles both ASCII and non-ASCII content:
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
    pub(crate) fn from_utf8(rt: &mut JSRuntime, bytes: Vec<u8>) -> Result<Self, i32> {
        Ok(Self::new(
            rt,
            StringBuf::from_utf8(bytes).map_err(|_| -1)?.into(),
        ))
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.id()
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        self.0.rt()
    }

    #[inline(always)]
    pub fn content(&self) -> &StringContent {
        self.0.content()
    }

    /// get raw code at index `i`
    #[inline]
    pub fn get(&self, i: usize) -> Option<u16> {
        self.content().get(i)
    }

    /// count string chars (include broken surrogate code).
    #[inline(always)]
    pub fn chars_count(&self) -> usize {
        self.0.chars_count()
    }

    /// codepoint length.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[inline(always)]
    pub fn is_ident(&self) -> bool {
        self.0.is_ident()
    }

    #[inline(always)]
    pub fn is_wide(&self) -> bool {
        self.0.is_wide()
    }

    /// Get hash value of the string.
    #[inline(always)]
    pub(crate) fn hash(&self) -> u32 {
        self.0.hash()
    }

    /// Save hash value
    pub(crate) fn set_hash(&self, h: u32) {
        self.0.set_hash(h);
    }

    pub fn set_atom_type(&self, t: AtomHintAndId) {
        self.0.set_atom_type(t);
    }

    /// Returns the atom type and atom id of the string.
    ///
    /// The atom type indicates whether this string is:
    /// - A regular string (None)
    /// - An interned string atom (String)
    /// - A unique symbol (Symbol)
    #[inline(always)]
    pub fn atom_info(&self) -> Option<AtomHintAndId> {
        self.0.atom_info()
    }

    /// make String class object with content from string `self`.
    pub fn to_object(&self, ctx: &JSContext, ctor: &JSValue) -> Result<JSObject, i32> {
        let len = self.len();
        let o = ctx
            .create_object_apply_ctor(ctor, ObjectPayload::STRING(self.clone()))
            .map_err(|_| -1)?;

        o.define_property_value(ctx, JS_ATOM_length, JSValue::from(len as u32), 0)?;
        Ok(o.into())
    }

    /// make sub string with content of given range.
    /// range is about buffer index, and end index is exclusive.
    pub fn sub_string(&self, ctx: &JSContext, range: Range<usize>) -> Result<JSString, i32> {
        let end = std::cmp::min(range.end, self.len());

        if range.start == 0 && end == self.len() {
            Ok(self.clone())
        } else {
            let count = end.saturating_sub(range.start);
            if count > 0 {
                ctx.create_string_with_buf(self.content().to_buf_range(range).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?)
            } else {
                ctx.create_string_with_content(StringContent::empty())
            }
        }
    }

    /// coerce string to number. if can't convert string to number, returns NAN.
    pub fn to_number(&self, ctx: &JSContext) -> JSValue {
        let str = self.content().to_utf8();
        let str = str.as_ref().trim();

        if str.is_empty() {
            JSValue::Int(0)
        } else {
            let mut p = str.as_ptr();
            if let Ok(num) = unsafe { js_atof(ctx, p.cast(), &mut p, 0, ATOD_ACCEPT_BIN_OCT) } {
                if unsafe { p.offset_from(str.as_ptr()) != str.len() as isize } {
                    // `str` has remaining characters that are not part of the number:
                    // not a valid number.
                    JSValue::Float64(f64::NAN)
                } else {
                    num
                }
            } else {
                let _ = ctx.take_exception();
                JSValue::Float64(f64::NAN)
            }
        }
    }

    pub fn downgrade(&self) -> JSStringWeakref {
        JSStringWeakref(Rc::downgrade(&self.0))
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.gc_mark.get()
    }

    pub fn gc_set_mark(&self, b: bool) {
        self.0.gc_mark.set(b);
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
    /// create string value from string buf.
    pub(crate) fn create_string_with_buf(&self, sbuf: StringBuf) -> Result<JSString, i32> {
        if sbuf.is_empty() {
            JS_ATOM_empty_string.force_string(self)
        } else {
            Ok(JSString::new(self.rt_mut(), sbuf.into()))
        }
    }

    /// create string value from string content.
    pub(crate) fn create_string_with_content(
        &self,
        content: StringContent,
    ) -> Result<JSString, i32> {
        if content.is_empty() {
            JS_ATOM_empty_string.force_string(self)
        } else {
            Ok(JSString::new(self.rt_mut(), content))
        }
    }

    /// create string from utf8 encoded data.
    pub fn create_string_from_utf8(&self, utf8_bytes: Vec<u8>) -> Result<JSString, i32> {
        if utf8_bytes.is_empty() {
            JS_ATOM_empty_string.force_string(self)
        } else if utf8_bytes.len() > JS_STRING_LEN_MAX {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            JSString::from_utf8(self.rt_mut(), utf8_bytes).inspect_err(|_| {
                self.throw_out_of_memory();
            })
        }
    }

    /// create string from str
    pub fn create_string_from_str(&self, str: &str) -> Result<JSString, i32> {
        if str.is_empty() {
            Ok(self.rt().get_atom_desc(JS_ATOM_empty_string).unwrap())
        } else if str.len() > JS_STRING_LEN_MAX {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            JSString::from_str(self.rt_mut(), str).inspect_err(|_| {
                self.throw_out_of_memory();
            })
        }
    }

    /// create string value from a single string char.
    pub fn create_string_from_char(&self, ch: StringChar) -> Result<JSString, i32> {
        let buf = match ch {
            StringChar::Char(ch) => {
                if ch.is_ascii() {
                    StringBuf::Ascii(vec![ch as u8])
                } else {
                    StringBuf::Utf16(ch.encode_utf16(&mut [0; 2]).to_vec())
                }
            }
            StringChar::Code(c) => {
                if c < 0x80 {
                    StringBuf::Ascii(vec![c as u8])
                } else {
                    StringBuf::Utf16(vec![c])
                }
            }
        };
        self.create_string_with_buf(buf)
    }
}

/// Note: string value `str2` is consumed inside.
pub fn JS_ConcatString3(
    ctx: &JSContext,
    str1: &CStr,
    str2: JSValue,
    str3: &CStr,
) -> Result<JSString, i32> {
    let p = if let JSValue::String(s) = str2 {
        s
    } else {
        str2.to_string(ctx)?
    };

    let len1 = str1.count_bytes();
    let len3 = str3.count_bytes();

    let res_string = StringBuf::with_capacity(len1 + p.len() + len3, false)
        .and_then(|mut s| {
            // str1 + str2 + str3
            s.push_str(str1.to_string_lossy().as_ref())
                .and_then(|_| s.push_content(p.content()))
                .and_then(|_| s.push_str(str3.to_string_lossy().as_ref()))
                .map(|_| s)
        })
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

    ctx.create_string_with_buf(res_string)
}

/// op1 and op2 are converted to strings.
/// For convience, op1 or op2 = JS_EXCEPTION are accepted and return JSValue::EXCEPTION.
/// op1 and op2 are consumed
pub fn JS_ConcatString(ctx: &JSContext, op1: JSValue, op2: JSValue) -> Result<JSString, i32> {
    let p1 = if let JSValue::String(s) = op1 {
        s
    } else {
        op1.to_string(ctx)?
    };

    let p2 = if let JSValue::String(s) = op2 {
        s
    } else {
        op2.to_string(ctx)?
    };

    if p1.is_empty() {
        return Ok(p2);
    } else if p2.is_empty() {
        return Ok(p1);
    }

    // concat p1, p2
    let buf =
        p1.0.content()
            .to_buf()
            .and_then(|mut lst| {
                if let Err(e) = lst.push_content(p2.content()) {
                    Err(e)
                } else {
                    Ok(lst)
                }
            })
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    ctx.create_string_with_buf(buf)
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

    /// Convert `self` to string or symbol value. (string is preferred)
    pub fn to_string_or_symbol(&self, ctx: &JSContext) -> Result<Self, i32> {
        match self {
            Self::String(_) | Self::Symbol(_) => {
                Ok(self.clone()) // string or symbol: as-is
            }

            Self::Int(i) => ctx
                .create_string_from_utf8(format!("{}", *i).into_bytes())
                .map(Self::String),
            Self::Bool(b) => if *b { JS_ATOM_true } else { JS_ATOM_false }
                .force_string(ctx)
                .map(Self::String),
            Self::Float64(n) => {
                let s = f64_stringify(*n, 10, None, 0);
                ctx.create_string_from_str(s.as_ref()).map(Self::String)
            }
            Self::NULL => JS_ATOM_null.force_string(ctx).map(Self::String),
            Self::UNDEFINED => JS_ATOM_undefined.force_string(ctx).map(Self::String),

            Self::Object(_) => self
                .to_primitive(ctx, HINT_STRING)
                .and_then(|s| s.to_string_or_symbol(ctx)),

            Self::Bytecode(_) => ctx
                .create_string_from_str("[function bytecode]")
                .map(Self::String),

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

    /// An alias for [`JSValue::to_string_or_symbol`](Self::to_string_or_symbol).
    #[inline(always)]
    pub fn to_propkey(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.to_string_or_symbol(ctx)
    }

    /// Coerce value `self` to string.
    /// if `self` is symbol, will return Err.
    pub fn to_string(&self, ctx: &JSContext) -> Result<JSString, i32> {
        match self {
            Self::String(s) => Ok(s.clone()),
            Self::Symbol(_) => {
                ctx.throw_type_error("cannot convert symbol to string");
                Err(-1)
            }
            _ => match self.to_string_or_symbol(ctx)? {
                JSValue::String(s) => Ok(s),
                JSValue::Symbol(_) => {
                    ctx.throw_type_error("cannot convert symbol to string");
                    Err(-1)
                }
                _ => unreachable!(),
            },
        }
    }

    /// Coerce value `val` to string, but null or undefined are forbidden.
    pub fn to_string_non_undefined_or_null(&self, ctx: &JSContext) -> Result<JSString, i32> {
        if self.is_undefined_or_null() {
            ctx.throw_type_error("null or undefined are forbidden");
            Err(-1)
        } else {
            self.to_string(ctx)
        }
    }

    /// Invoking `toLocaleString()` method.
    pub fn to_locale_string(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        if self.is_undefined_or_null() {
            self.to_string(ctx).map(JSValue::String)
        } else {
            self.invoke(ctx, JS_ATOM_toLocaleString, &[])
        }
    }
}

/// starts_with() equivalent
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
// String class exotic methods
//

/// prop is not consumed
fn string_define_own_property(
    ctx: &JSContext,
    this_obj: &JSValue,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    macro_rules! fail {
        () => {
            return if ctx.can_throw(flags as _) {
                ctx.throw_type_error("property is not configurable");
                Err(-1)
            } else {
                Ok(false)
            };
        };
    }

    macro_rules! def {
        () => {
            return this_obj.define_property(
                ctx,
                prop,
                val,
                getter,
                setter,
                flags as u32 | JS_PROP_NO_EXOTIC,
            );
        };
    }

    if let Some(idx) = prop.to_small_int() {
        let ob = unsafe { this_obj.as_object_unchecked() };
        if let JSValue::String(p1) = ob.0.payload.borrow().value().unwrap() {
            let len = p1.len();
            let nth_char = p1.content().iter_since(idx as usize).next().map(|(_, c)| c);

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

                let p2 = unsafe { val.string_unchecked() };
                let len2 = p2.len();
                let nth_char2 = p2.content().iter().next().map(|(_, c)| c);

                if len2 != 1 {
                    fail!();
                }
                if nth_char != nth_char2 {
                    fail!();
                }
            }
            Ok(true)
        } else {
            def!();
        }
    } else {
        def!();
    }
}

pub static js_string_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    get_own_property: Some(
        |ctx: &JSContext,
         obj: JSValue,
         prop: JSAtom|
         -> Result<Option<JSPropertyDescriptor>, i32> {
            // This is a class exotic method for JS_CLASS_STRING object
            if let Some(idx) = prop.to_small_int()
                && let Some(JSValue::String(s)) =
                    obj.as_object().and_then(|o| o.0.payload.borrow().value())
                && idx < s.len() as u32
            {
                let code = s.content().get(idx as usize).unwrap();
                return Ok(Some(JSPropertyDescriptor {
                    flags: JS_PROP_ENUMERABLE as _,
                    value: ctx.create_string_from_char(code.into())?.into(),
                    getter: JSValue::UNDEFINED,
                    setter: JSValue::UNDEFINED,
                }));
            }

            Ok(None)
        },
    ),

    define_own_property: Some(string_define_own_property),

    delete_property: Some(
        |_: &JSContext, obj: JSValue, prop: JSAtom| -> Result<bool, i32> {
            if let Some(idx) = prop.to_small_int() {
                let buflen = TryInto::<JSString>::try_into(obj).unwrap().len();
                if (idx as usize) < buflen {
                    return Ok(false);
                }
            }

            Ok(true)
        },
    ),

    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
};
