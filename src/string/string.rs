use std::{
    borrow::Cow,
    cell::Cell,
    ffi::CStr,
    ops::Range,
    rc::{Rc, Weak},
    str::FromStr,
};

use crate::{
    JS_CLASS_STRING,
    atom::{AtomHintAndId, JSAtom, consts::*},
    class::JSClassExoticMethods,
    context::JSContext,
    gc::GCObjectHeader,
    number::f64_stringify,
    object::ObjectDetail,
    object::{
        GCObjectWeakref, JS_PROP_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JSObject,
        JSPropertyDescriptor, check_define_prop_flags, js_get_length32,
    },
    runtime::JSRuntime,
    string::StringChar,
    value::HINT_STRING,
    value::{JSValue, JSValueConst},
};

use super::StringBuf;

/// Maximum length allowed for JavaScript strings (1GB - 1)
pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

pub type StringContent = Rc<StringBuf>;

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
pub struct JSStringInner {
    header: GCObjectHeader,

    // Hash value
    hash_and_type: Cell<u32>,

    /// atom info of string, None if not registered as atom.
    atom_type: Cell<Option<AtomHintAndId>>,

    /// string content
    content: StringContent,

    gc_mark: Cell<bool>,
}

impl JSStringInner {
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

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
    pub fn char_count(&self) -> usize {
        self.content.chars_count()
    }

    /// Returns the buffer length
    #[inline(always)]
    pub fn buf_len(&self) -> usize {
        self.content.buf_len()
    }

    /// Checks if the string is empty (zero length)
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.buf_len() == 0 && !self.is_null()
    }

    #[inline(always)]
    pub fn is_null(&self) -> bool {
        self.content.is_null()
    }

    #[inline(always)]
    pub fn is_empty_or_null(&self) -> bool {
        self.content.is_empty_or_null()
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
        write!(f, "{self}")
    }
}

impl JSString {
    /// create JSString with string content
    pub fn new(rt: &mut JSRuntime, content: StringContent) -> Self {
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
    pub fn from_str(rt: &mut JSRuntime, s: &str) -> Result<Self, i32> {
        Ok(Self::new(rt, Rc::new(StringBuf::from_str(s)?)))
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
    pub fn from_utf8(rt: &mut JSRuntime, bytes: Vec<u8>) -> Result<Self, i32> {
        Ok(Self::new(
            rt,
            Rc::new(StringBuf::from_utf8(bytes).map_err(|_| -1)?),
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
    pub fn char_count(&self) -> usize {
        self.0.char_count()
    }

    /// buffer length.
    #[inline(always)]
    pub fn buf_len(&self) -> usize {
        self.0.buf_len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[inline(always)]
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    #[inline(always)]
    pub fn is_empty_or_null(&self) -> bool {
        self.0.is_empty_or_null()
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
        let len = self.buf_len();
        let o = ctx
            .create_object_apply_ctor(ctor, ObjectDetail::STRING(self.clone()))
            .map_err(|_| -1)?;
        o.define_property_value(ctx, JS_ATOM_length, len as u32, 0)?;
        Ok(o.into())
    }

    /// make sub string with content of given range.
    /// range is about buffer index, and end index is exclusive.
    pub fn sub_string(&self, ctx: &JSContext, range: Range<usize>) -> Result<JSString, i32> {
        let end = std::cmp::min(range.end, self.buf_len());

        if range.start == 0 && end == self.buf_len() {
            Ok(self.clone())
        } else {
            let count = end.saturating_sub(range.start);
            if count > 0 {
                let sub = self.content().slice(range).to_owned().map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                ctx.create_string_with_buf(sub)
            } else {
                ctx.create_string_with_buf(StringBuf::Empty)
            }
        }
    }

    /// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
    ///
    /// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
    /// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
    /// 返回: 包含替换后的字符串值
    pub fn get_substitution(
        ctx: &JSContext,
        matched: &Self,           // 匹配到的子字符串（对应 $&）
        str: &Self,               // 原始字符串。
        position: usize,          // 匹配发生的位置索引
        captures: &JSValue,       // 命名捕获组对象。（对应 $1, $2 等）
        named_captures: &JSValue, // 命名捕获组。（对应 $<name>）
        replacement: &Self,       // 替换字符串或替换函数。
    ) -> Result<JSString, i32> {
        let captures_len = if !captures.is_undefined() {
            js_get_length32(ctx, captures)?
        } else {
            0
        };

        let matched_len = matched.buf_len();
        let rep_len = replacement.buf_len();
        let rep_cc = replacement.content();

        let mut res_content =
            StringBuf::with_capacity(str.buf_len(), str.is_wide()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        // escape $-patterns in `replacement`
        let mut i = 0_usize;
        loop {
            let mut j = if let Some(x) = rep_cc.index_of_char('$', i) {
                if x + 1 >= rep_len {
                    break;
                }
                x as usize
            } else {
                break;
            };
            debug_assert!(j >= i);

            if j > i {
                res_content
                    .push_content_slice(&rep_cc.slice(i..j))
                    .map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
            }

            let j0 = j;
            j += 1;
            let (_, c) = rep_cc.iter_since(j).next().unwrap();
            j += 1;

            match c {
                StringChar::Char('$') => {
                    // $$ -> $
                    res_content.push_char(c).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
                StringChar::Char('&') => {
                    // $& -> matched
                    res_content.push(matched.content()).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
                StringChar::Char('`') => {
                    // $` -> str[0..position]
                    res_content
                        .push_content_slice(&str.content().slice(0..position))
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                }
                StringChar::Char('\'') => {
                    // $' -> str[position+matched.len()..]
                    res_content
                        .push_content_slice(
                            &str.content().slice((position + matched_len)..str.buf_len()),
                        )
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                }
                StringChar::Char(a @ ('0'..'9')) => {
                    // $k -> captures[k]
                    let mut k = a as u32 - '0' as u32;
                    if j < rep_len {
                        if let Some((_, StringChar::Char(c1))) = rep_cc.iter_since(j).next()
                            && c1.is_ascii_digit()
                        {
                            let k1 = k * 10 + (c1 as u32 - '0' as u32);
                            if k1 >= 1 && k1 < captures_len {
                                k = k1;
                                j += 1;
                            }
                        }
                    }

                    if (1..captures_len).contains(&k) {
                        let s = captures.get_property_i64(ctx, k as i64)?;
                        if !s.is_undefined() {
                            let sval = s.to_string(ctx)?;
                            res_content.push(sval.content()).map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                        }
                    } else {
                        res_content
                            .push_content_slice(&rep_cc.slice(j0..j))
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                    }
                }
                StringChar::Char('<') if !named_captures.is_undefined() => {
                    // $<name> -> names_captures[name]
                    if let Some(k) = rep_cc.index_of_char('>', j as _) {
                        let name = replacement.sub_string(ctx, j..k)?;
                        let capture = named_captures.get_property_value(ctx, &name.into())?;
                        if !capture.is_undefined() {
                            let sval = capture.to_string(ctx)?;
                            res_content.push(sval.content()).map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                        }
                        j = k + 1;
                    } else {
                        res_content
                            .push_content_slice(&rep_cc.slice(j0..j))
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                    }
                }
                _ => {
                    res_content
                        .push_content_slice(&rep_cc.slice(j0..j))
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                }
            }

            i = j;
        }

        res_content
            .push_content_slice(&rep_cc.slice(i..rep_len))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        ctx.create_string_with_buf(res_content)
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
    pub fn create_string_with_buf(&self, sbuf: StringBuf) -> Result<JSString, i32> {
        if sbuf.is_empty() {
            JS_ATOM_empty_string.to_string(self)
        } else {
            Ok(JSString::new(self.rt_mut(), Rc::new(sbuf)))
        }
    }

    /// create string value from string content.
    pub fn create_string_with_content(&self, content: StringContent) -> Result<JSString, i32> {
        if content.is_empty() {
            JS_ATOM_empty_string.to_string(self)
        } else {
            Ok(JSString::new(self.rt_mut(), content))
        }
    }

    /// create string from utf8 encoded data.
    pub fn create_string_from_utf8(&self, utf8_bytes: Vec<u8>) -> Result<JSString, i32> {
        if utf8_bytes.is_empty() {
            JS_ATOM_empty_string.to_string(self)
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

    /// create string value from a single u16 char.
    pub fn create_string_from_char(&self, ch: StringChar) -> Result<JSString, i32> {
        let buf = match ch {
            StringChar::Char(ch) => {
                if ch.is_ascii() {
                    StringBuf::Ascii(vec![ch as u8])
                } else {
                    StringBuf::Utf16(ch.encode_utf16(&mut [0; 2]).to_vec())
                }
            }
            StringChar::Code(c) => StringBuf::Utf16(vec![c]),
        };
        self.create_string_with_buf(buf)
    }
}

/* return < 0, 0 or > 0 */
#[deprecated(note = "use StringBuf::cmp() instead")]
pub unsafe fn js_string_compare(ctx: &JSContext, p1: &JSString, p2: &JSString) -> i32 {
    match p1.content().cmp(p2.content()) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

#[deprecated(note = "use JSString::is_empty()")]
pub fn JS_IsEmptyString(v: &JSValue) -> bool {
    if let JSValue::String(s) = v {
        s.is_empty()
    } else {
        false
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

    let res_string = StringBuf::with_capacity(len1 + p.buf_len() + len3, p.is_wide())
        .and_then(|mut s| {
            // str1 + str2 + str3
            s.push_str(str1.to_string_lossy().as_ref())
                .and_then(|_| s.push(p.content()))
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

    match Rc::try_unwrap(p1.0) {
        Ok(mut ss) => {
            // p1 ref count == 1
            match Rc::try_unwrap(ss.content) {
                Ok(mut cc) => {
                    if cc.push(p2.content()).is_err() {
                        ss.content = Rc::new(cc);
                        ctx.throw_out_of_memory();
                        Err(-1)
                    } else {
                        ctx.create_string_with_buf(cc)
                    }
                }
                Err(cc) => {
                    if let Ok(mut cc2) = cc.copy_to_new() {
                        if cc2.push(p2.content()).is_err() {
                            ctx.throw_out_of_memory();
                            Err(-1)
                        } else {
                            ctx.create_string_with_buf(cc2)
                        }
                    } else {
                        ctx.throw_out_of_memory();
                        Err(-1)
                    }
                }
            }
        }
        Err(ss) => {
            let new_sc = ss
                .content()
                .copy_to_new()
                .and_then(|mut s| {
                    s.push(p2.content())?;
                    Ok(s)
                })
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            ctx.create_string_with_buf(new_sc)
        }
    }
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

    /// Convert value `self` to string or symbol.
    /// if `self` itself is not a symbol, string is preferred.
    /// returns String or Symbol value.
    pub fn to_string_or_symbol(&self, ctx: &JSContext) -> Result<Self, i32> {
        match self {
            Self::String(_) | Self::Symbol(_) => {
                Ok(self.clone()) // AS-IS original string or symbol
            }
            Self::Int(i) => ctx
                .create_string_from_utf8(format!("{}", *i).into_bytes())
                .map(Self::String),
            Self::Bool(b) => if *b { JS_ATOM_true } else { JS_ATOM_false }
                .to_string(ctx)
                .map(Self::String),
            Self::NULL => JS_ATOM_null.to_string(ctx).map(Self::String),
            Self::UNDEFINED => JS_ATOM_undefined.to_string(ctx).map(Self::String),
            Self::Object(_) => self
                .to_primitive(ctx, HINT_STRING)?
                .to_string_or_symbol(ctx),
            Self::Module(_) => ctx.create_string_from_str("[module]").map(Self::String),
            Self::Bytecode(_) => ctx
                .create_string_from_str("[function bytecode]")
                .map(Self::String),
            Self::Float64(n) => {
                let s = f64_stringify(*n, 10, None, 0);
                ctx.create_string_from_str(s.as_ref()).map(Self::String)
            }
            Self::UNINITIALIZED => ctx
                .create_string_from_str("[uninitialized]")
                .map(Self::String),
            Self::CatchOffset(_) => ctx
                .create_string_from_str("[cacthoffset]")
                .map(Self::String),
            Self::AsyncStateResume(_) => ctx
                .create_string_from_str("[asyncfuncresume]")
                .map(Self::String),
            Self::EXCEPTION => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                Err(-1)
            }
        }
    }

    /// Coerce value `self` to string.
    /// if `self` is symbol value, returns Err.
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

    /// Invoking `toLocaleString()` method, returns result string.
    pub fn to_locale_string(&self, ctx: &JSContext) -> Result<JSString, i32> {
        if self.is_undefined_or_null() {
            self.to_string(ctx)
        } else if let Self::String(s) = self.invoke(ctx, JS_ATOM_toLocaleString, &[])? {
            Ok(s)
        } else {
            ctx.throw_internal_error("toLocaleString() must return string");
            Err(-1)
        }
    }

    /// Convert value `self` to utf8 encoded text.
    pub fn to_utf8(&self, ctx: &JSContext) -> Result<Cow<'_, str>, i32> {
        match self {
            Self::String(s) | Self::Symbol(s) => {
                Ok(match s.content().to_utf8() {
                    Cow::Borrowed(str) => {
                        // use string description content pointer.
                        Cow::Borrowed(unsafe { (str as *const str).as_ref().unwrap() })
                    }
                    o => o,
                })
            }
            _ => Ok(Cow::Owned(
                self.to_string(ctx)?.content().to_utf8().into_owned(),
            )),
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
// String class methods
//

/// prop is not consumed
fn string_define_own_property(
    ctx: &JSContext,
    this_obj: &JSValue,
    prop: JSAtom,
    val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: i32,
) -> i32 {
    macro_rules! fail {
        () => {
            return if ctx.throw_type_error_or_false(flags as _, "property is not configurable") {
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
                    val,
                    getter,
                    setter,
                    flags as u32 | JS_PROP_NO_EXOTIC,
                )
                .map_or(-1, |b| b as i32);
        };
    }

    if let Some(idx) = prop.to_int() {
        let ob = unsafe { this_obj.object_unchecked() };
        if let JSValue::String(p1) = ob.0.payload.borrow().value().unwrap() {
            let len = p1.buf_len();
            let nth_char = p1.content().iter_since(idx as usize).next().map(|(_, c)| c);

            if idx >= len as u32 {
                def!();
            }
            if unsafe { !check_define_prop_flags(JS_PROP_ENUMERABLE, flags as _) } {
                fail!();
            }

            // check that the same value is configured
            if flags & JS_PROP_HAS_VALUE as i32 != 0 {
                if !val.is_string() {
                    fail!();
                }

                let p2 = unsafe { val.string_unchecked() };
                let len2 = p2.buf_len();
                let nth_char2 = p2.content().iter().next().map(|(_, c)| c);

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

pub static js_string_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    get_own_property: Some(
        |ctx: &JSContext,
         obj: JSValue,
         prop: JSAtom|
         -> Result<Option<JSPropertyDescriptor>, i32> {
            // This is a class exotic method: obj class_id is JS_CLASS_STRING
            if let Some(idx) = prop.to_int()
                && let Some(JSValue::String(s)) =
                    obj.object().and_then(|o| o.0.payload.borrow().value())
                && idx < s.buf_len() as u32
            {
                let (_, ch) = s.content().iter_since(idx as usize).next().unwrap();
                return Ok(Some(JSPropertyDescriptor {
                    flags: JS_PROP_ENUMERABLE as _,
                    value: ctx.create_string_from_char(ch)?.into(),
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
            if let Some(idx) = prop.to_int() {
                let buflen = TryInto::<JSString>::try_into(obj).unwrap().buf_len();
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
