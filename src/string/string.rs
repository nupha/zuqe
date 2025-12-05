use std::{
    cmp::Ordering,
    ffi::CStr,
    ops::{Deref, DerefMut, Range},
};

use gc_lite::{GcTracable, GcRef};

use crate::{
    JS_CLASS_STRING,
    atom::{Atom, AtomInfo, AtomKind, is_const_atom, is_small_int_atom},
    class::JSClassExoticMethods,
    context::{ContextImpl, ContextPtr},
    number::{ATOD_ACCEPT_BIN_OCT, JS_DTOA_VAR_FORMAT, f64_stringify, js_atof},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JSPropertyDescriptor,
        ObjectPayload, ObjectRef, check_define_prop_flags,
    },
    resource::LocalResource,
    runtime::RuntimePtr,
    string::{StringChar, StringData},
    utils::unlikely,
    value::{HINT_STRING, JSValue},
};

use super::StringBuf;

/// Maximum length allowed for JavaScript strings (1GB - 1)
pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

/// Bundle of string data, and atom info
pub struct StringImpl {
    /// for empty, ident string or const atom description, this should be None.
    rt: Option<RuntimePtr>,
    /// string content
    content: StringData,
    /// if string was registered as atom, this stores the atom info.
    /// None means the string is not registered as atom.
    atom_info: Option<AtomInfo>,
}

unsafe impl GcTracable for StringImpl {
    #[inline(always)]
    fn trace(&self, _: &mut gc_lite::GcTracer) {}
}

impl Drop for StringImpl {
    fn drop(&mut self) {
        if let Some(atom_info) = &self.atom_info {
            let atom_id = atom_info.id();
            if !is_const_atom(atom_id) && !is_small_int_atom(atom_id) {
                Atom::unregister(atom_id);
            }
        }
    }
}

impl Deref for StringImpl {
    type Target = StringData;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

impl PartialEq for StringImpl {
    fn eq(&self, other: &Self) -> bool {
        self.content() == other.content()
    }
}

impl PartialOrd for StringImpl {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let a1 = self.atom_info();
        let a2 = other.atom_info();

        let r = match (a1.map(|x| x.kind()), a2.map(|x| x.kind())) {
            // symbol-atom vs non-atom: symbol is greater
            (None, Some(AtomKind::Symbol | AtomKind::GlobalSymbol)) => Ordering::Less,
            (Some(AtomKind::Symbol | AtomKind::GlobalSymbol), None) => Ordering::Greater,

            // symbol-atom vs string-atom: symbol is greater
            (
                Some(AtomKind::String | AtomKind::Private),
                Some(AtomKind::Symbol | AtomKind::GlobalSymbol),
            ) => Ordering::Less,
            (
                Some(AtomKind::Symbol | AtomKind::GlobalSymbol),
                Some(AtomKind::String | AtomKind::Private),
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

impl std::fmt::Display for StringImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.content().to_utf8(false);
        write!(f, "{s}")
    }
}

impl std::fmt::Debug for StringImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.content().to_utf8(true);
        write!(f, "{s}")
        // f.debug_struct("StringImpl")
        //     .field("rt", &self.rt)
        //     .field("content", &self.content)
        //     .field("atom_info", &self.atom_info)
        //     .finish()
    }
}

impl StringImpl {
    /// create StringImpl with string content, and atom info.
    const fn new(rt: Option<RuntimePtr>, content: StringData, atom_info: Option<AtomInfo>) -> Self {
        StringImpl {
            // TODO: remove RuntimePtr lifetime
            rt,
            content,
            atom_info,
        }
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// None means this is a const atom description
    #[inline(always)]
    pub(crate) const fn rt(&self) -> Option<RuntimePtr> {
        self.rt
    }

    /// Returns a reference to the underlying string content
    /// Used for string operations and comparisons
    #[inline(always)]
    pub const fn content(&self) -> &StringData {
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
    pub const fn is_ident(&self) -> bool {
        self.content.is_ident()
    }

    /// Checks if the string uses UTF-16 encoding
    /// Returns true for strings containing non-ASCII characters
    /// Returns false for ASCII-only strings (stored as UTF-8)
    #[inline(always)]
    pub const fn is_wide(&self) -> bool {
        self.content.is_wide()
    }

    /// Returns the atom info associated with this string.
    #[inline(always)]
    pub const fn atom_info(&self) -> Option<&AtomInfo> {
        self.atom_info.as_ref()
    }

    pub fn set_atom_info(&mut self, atom_info: AtomInfo) {
        debug_assert!(self.atom_info.is_none());
        self.atom_info.replace(atom_info);
    }

    /// get raw code at index `nth`
    pub fn get(&self, nth: usize) -> Option<u16> {
        self.content().get(nth)
    }

    /// copy content to make a new string, with given rt.
    pub(crate) fn copy_to_new(&self, rt: RuntimePtr) -> Result<StringRef, i32> {
        match self.content() {
            StringData::Empty => StringRef::new_internal(Some(rt), StringData::Empty, None),
            StringData::Ident => StringRef::new_internal(Some(rt), StringData::Ident, None),
            StringData::AsciiStatic(s) => StringRef::from_static(Some(rt), s, None),
            _ => {
                debug_assert!(self.rt.is_some(), "can't copy const atom description");
                StringRef::new(self.rt.unwrap(), self.content().clone())
            }
        }
    }

    /// coerce string to number. if can't convert string to number, returns NAN.
    pub fn to_number(&self, ctx: &ContextImpl) -> JSValue {
        let str = self.content().to_utf8(false);
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
                    JSValue::from(num)
                }
            } else {
                ctx.rt().take_exception();
                JSValue::Float64(f64::NAN)
            }
        }
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct StringRef(GcRef<StringImpl>);

unsafe impl GcTracable for StringRef {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        // Note: String for const atom (w/o rt) must not set mark.
        if unsafe { self.0.as_ref().rt().is_some() } {
            tracer.mark(self.0);
        }
    }
}

impl Deref for StringRef {
    type Target = StringImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for StringRef {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl PartialEq for StringRef {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        unsafe { self.0.as_ref().content() == other.0.as_ref().content() }
    }
}

impl std::fmt::Display for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe { write!(f, "{}", self.0.as_ref()) }
    }
}

impl std::fmt::Debug for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe { write!(f, "{:?}", self.0.as_ref()) }
    }
}

impl From<StringRef> for JSValue {
    #[inline(always)]
    fn from(value: StringRef) -> Self {
        Self::String(value)
    }
}

impl TryFrom<ObjectRef> for StringRef {
    type Error = i32;

    fn try_from(obj: ObjectRef) -> Result<Self, Self::Error> {
        if obj.class_id() == JS_CLASS_STRING {
            if let Some(JSValue::String(s)) = obj.payload().value() {
                Ok(*s)
            } else {
                unreachable!()
            }
        } else {
            Err(-1)
        }
    }
}

impl TryFrom<JSValue> for StringRef {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        match value {
            JSValue::String(s) => Ok(s),
            JSValue::Object(o) => o.try_into(),
            _ => Err(-1),
        }
    }
}

impl StringRef {
    /// create StringImpl with string content, and atom.
    fn new_internal(
        rt: Option<RuntimePtr>,
        content: StringData,
        atom_info: Option<AtomInfo>,
    ) -> Result<Self, i32> {
        let s = StringImpl::new(rt, content, atom_info);
        if let Some(mut rt) = rt {
            rt.gc_alloc(s)
        } else {
            LocalResource::gc_alloc(rt.map(|r| r.gc_part()), s)
        }
        .map_err(|_| {
            println!("[O.o] alloc error");
            -1
        })
        .map(Self)
    }

    /// create StringImpl with string content
    #[inline(always)]
    pub(crate) fn new(rt: RuntimePtr, content: StringData) -> Result<Self, i32> {
        Self::new_internal(Some(rt), content, None)
    }

    /// Creates a StringImpl from a Rust string slice
    ///
    /// Efficiently creates a string from UTF-8 encoded Rust str.
    /// The string content is copied to ensure JavaScript string immutability.
    ///
    /// # Arguments
    /// * `rt` - JavaScript runtime for memory allocation
    /// * `str` - Source Rust string slice
    ///
    /// # Returns
    /// * `Ok(Box<StringImpl>)` - Newly created string
    /// * `Err(-1)` - Memory allocation or UTF-8 validation failed
    pub(crate) fn from_str(rt: RuntimePtr, s: &str) -> Result<Self, i32> {
        let sbuf = StringBuf::from_str(s)?;
        let data = StringData::from_buf(sbuf);
        Self::new_internal(Some(rt), data, None)
    }

    /// static str `s` must be ascii str.
    #[inline(always)]
    pub(crate) fn from_static(
        rt: Option<RuntimePtr>,
        s: &'static str,
        atom_info: Option<AtomInfo>,
    ) -> Result<Self, i32> {
        debug_assert!(s.is_ascii());
        Self::new_internal(rt, StringData::AsciiStatic(s), atom_info)
    }

    /// Creates a StringImpl from UTF-8 encoded bytes
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
    /// * `Ok(Box<StringImpl>)` - Newly created string
    /// * `Err(-1)` - Memory allocation or UTF-8 validation failed
    pub(crate) fn from_utf8(rt: RuntimePtr, bytes: Vec<u8>) -> Result<Self, i32> {
        let data = StringData::from_buf(StringBuf::from_utf8(bytes).map_err(|_| -1)?);
        Self::new(rt, data)
    }

    /// convert string to a String instance with content transfered.
    pub fn objectify(self, ctx: &ContextImpl, ctor: JSValue) -> Result<ObjectRef, i32> {
        let len = self.len();
        let mut o = ctx
            .create_object_from_ctor(ctor, ObjectPayload::STRING(self.into()))
            .map_err(|_| -1)?;

        o.define_property_value(ctx, Atom::CONST_length, JSValue::from(len as u32), 0)?;
        Ok(o)
    }

    #[inline(always)]
    pub const fn gc_ref(&self) -> GcRef<StringImpl> {
        self.0
    }

    /// make sub string with content of given range.
    /// range is about buffer index, and end index is exclusive.
    pub fn sub_string(&self, ctx: &ContextImpl, range: Range<usize>) -> Result<Self, i32> {
        let end = std::cmp::min(range.end, self.len());

        if range.start == 0 && end == self.len() {
            Ok(*self)
        } else {
            let count = end.saturating_sub(range.start);
            if count > 0 {
                ctx.create_string_with_buf(self.content().to_buf_range(range).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?)
            } else {
                ctx.create_string_with_content(StringData::Empty)
            }
        }
    }
}

impl ContextImpl {
    /// create string value from string buf.
    pub(crate) fn create_string_with_buf(&self, sbuf: StringBuf) -> Result<StringRef, i32> {
        if sbuf.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else {
            let data = StringData::from_buf(sbuf);
            StringRef::new(self.rt(), data).map_err(|_| {
                self.throw_out_of_memory();
                -1
            })
        }
    }

    /// create string value from string content.
    pub(crate) fn create_string_with_content(&self, content: StringData) -> Result<StringRef, i32> {
        if content.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else {
            StringRef::new(self.rt(), content).map_err(|_| {
                self.throw_out_of_memory();
                -1
            })
        }
    }

    /// create string from utf8 encoded data.
    pub fn create_string_from_utf8(&self, utf8_bytes: Vec<u8>) -> Result<StringRef, i32> {
        if utf8_bytes.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else if utf8_bytes.len() > JS_STRING_LEN_MAX {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            StringRef::from_utf8(self.rt(), utf8_bytes).inspect_err(|_| {
                self.throw_out_of_memory();
            })
        }
    }

    /// create string from str
    pub fn create_string_from_str(&self, str: &str) -> Result<StringRef, i32> {
        if unlikely(str.is_empty()) {
            Atom::CONST_empty_string.force_string(self)
        } else if unlikely(str.len() > JS_STRING_LEN_MAX) {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            StringRef::from_str(self.rt(), str).inspect_err(|_| {
                self.throw_out_of_memory();
            })
        }
    }

    /// create string value from a single string char.
    pub fn create_string_from_char(&self, ch: StringChar) -> Result<StringRef, i32> {
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
    ctx: &ContextImpl,
    str1: &CStr,
    str2: JSValue,
    str3: &CStr,
) -> Result<StringRef, i32> {
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
pub fn JS_ConcatString(ctx: &ContextImpl, op1: JSValue, op2: JSValue) -> Result<StringRef, i32> {
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
    let buf = p1
        .content()
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
    /// get a reference to value's string description. won't change value's ref_count.
    /// will panic if value is not string type.
    #[inline(always)]
    pub unsafe fn string_unchecked(&self) -> &StringRef {
        if let Self::String(s) = self {
            s
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            std::hint::unreachable_unchecked();
        }
    }

    /// Convert `self` to string or symbol value. (string is preferred)
    pub fn to_string_or_symbol(&self, ctx: &ContextImpl) -> Result<Self, i32> {
        match self {
            Self::String(_) | Self::Symbol(_) => {
                Ok(*self) // string or symbol: as-is
            }

            Self::Int(i) => ctx
                .create_string_from_utf8(format!("{}", *i).into_bytes())
                .map(Self::String),
            Self::Bool(b) => if *b {
                Atom::CONST_true
            } else {
                Atom::CONST_false
            }
            .force_string(ctx)
            .map(Self::String),
            Self::Float64(n) => {
                let s = f64_stringify(*n, 10, None, JS_DTOA_VAR_FORMAT);
                ctx.create_string_from_str(s.as_ref()).map(Self::String)
            }
            Self::NULL => Atom::CONST_null.force_string(ctx).map(Self::String),
            Self::UNDEFINED => Atom::CONST_undefined.force_string(ctx).map(Self::String),

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
    pub fn to_propkey(&self, ctx: &ContextImpl) -> Result<JSValue, i32> {
        self.to_string_or_symbol(ctx)
    }

    /// Coerce value `self` to string.
    /// if `self` is symbol, will return Err.
    pub fn to_string(&self, ctx: &ContextImpl) -> Result<StringRef, i32> {
        match self {
            Self::String(s) => Ok(*s),
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
    pub fn to_string_non_undefined_or_null(&self, ctx: &ContextImpl) -> Result<StringRef, i32> {
        if self.is_undefined_or_null() {
            ctx.throw_type_error("null or undefined are forbidden");
            Err(-1)
        } else {
            self.to_string(ctx)
        }
    }

    /// Invoking `toLocaleString()` method.
    pub fn to_locale_string(&self, ctx: &ContextImpl) -> Result<JSValue, i32> {
        if self.is_undefined_or_null() {
            self.to_string(ctx).map(JSValue::String)
        } else {
            self.invoke(ctx, Atom::CONST_toLocaleString, &[])
        }
    }
}

//
// String class exotic methods
//

/// prop is not consumed
fn string_define_own_property(
    ctx: ContextPtr,
    mut this_obj: JSValue,
    prop: Atom,
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
            // use clone to make `this_obj` &Value -> &mut JSValue
            return this_obj.define_property(
                &ctx,
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
        if let JSValue::String(p1) = ob.payload().value().unwrap() {
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

pub(crate) const js_string_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    get_own_property: Some(
        |ctx: ContextPtr, obj: JSValue, prop: Atom| -> Result<Option<JSPropertyDescriptor>, i32> {
            // This is a class exotic method for JS_CLASS_STRING object
            if let Some(idx) = prop.to_small_int()
                && let Some(JSValue::String(s)) = obj.as_object().and_then(|o| o.payload().value())
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

    delete_property: Some(|_, obj: JSValue, prop: Atom| -> Result<bool, i32> {
        if let Some(idx) = prop.to_small_int() {
            let buflen = StringRef::try_from(obj).unwrap().len();
            if (idx as usize) < buflen {
                return Ok(false);
            }
        }

        Ok(true)
    }),

    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
};
