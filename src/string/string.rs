// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cmp::Ordering,
    ffi::CStr,
    ops::{Deref, DerefMut, Range},
};

use gc_lite::{GcLocal, GcNode, GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_STRING,
    atom::{Atom, AtomInfo, AtomKind},
    class::ClassExoticMethods,
    context::{ContextImpl, ContextPtr},
    number::{ATOD_ACCEPT_BIN_OCT, JS_DTOA_VAR_FORMAT, f64_stringify, js_atof},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JSPropertyDescriptor, Object,
        ObjectPayload, check_define_prop_flags,
    },
    runtime::RuntimePtr,
    string::{CharCode, StringData},
    utils::unlikely,
    value::{HINT_STRING, JSValue},
};

use super::StringBuf;

/// Maximum length allowed for JavaScript strings (1GB - 1)
pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

/// String data + atom info
pub struct StringImpl {
    /// for empty string, ident string or const string, this should be None.
    rt: RuntimePtr,
    /// string content
    content: StringData,
    /// if string is used as atom, this stores the atom info.
    /// None means the string is not an atom.
    atom_info: Option<AtomInfo>,
}

impl GcTrace for StringImpl {
    #[inline(always)]
    fn trace(&self, _: &mut GcTraceCtx) {}
}

impl Drop for StringImpl {
    fn drop(&mut self) {
        if let Some(a) = &self.atom_info
            && !a.is_const()
            && !a.is_small_int()
        {
            self.rt().remove_atom(a);
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
            #[cfg(feature = "builtin_symbol")]
            (None, Some(AtomKind::Symbol | AtomKind::GlobalSymbol)) => Ordering::Less,
            #[cfg(feature = "builtin_symbol")]
            (Some(AtomKind::Symbol | AtomKind::GlobalSymbol), None) => Ordering::Greater,

            // symbol-atom vs string-atom: symbol is greater
            #[cfg(feature = "builtin_symbol")]
            (
                Some(AtomKind::String | AtomKind::Private),
                Some(AtomKind::Symbol | AtomKind::GlobalSymbol),
            ) => Ordering::Less,
            #[cfg(feature = "builtin_symbol")]
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
        let s = self.content().encode_utf8(false);
        write!(f, "{s}")
    }
}

impl std::fmt::Debug for StringImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.content().encode_utf8(true);
        write!(f, "{s}")

        // f.debug_struct("StringImpl")
        //     .field("rt", &self.rt)
        //     .field("content", &self.content)
        //     .field("atom_info", &self.atom_info)
        //     .finish()
    }
}

impl StringImpl {
    /// create StringImpl with string data, and atom info.
    pub(crate) const fn new(
        rt: RuntimePtr,
        content: StringData,
        atom_info: Option<AtomInfo>,
    ) -> Self {
        StringImpl {
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
    pub(crate) const fn rt(&self) -> RuntimePtr {
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

    /// Returns the atom info associated to this string.
    /// If string is not an Atom, returns None.
    #[inline(always)]
    pub const fn atom_info(&self) -> Option<&AtomInfo> {
        self.atom_info.as_ref()
    }

    pub(crate) fn set_atom_info(&mut self, atom_info: AtomInfo) {
        debug_assert!(self.atom_info.is_none());
        self.atom_info.replace(atom_info);
    }

    /// get raw code at index `nth`
    pub fn get(&self, nth: usize) -> Option<u16> {
        self.content().get(nth)
    }

    /// copy content to make a new string, with given rt.
    pub(crate) fn copy_to_new(&self, rt: RuntimePtr) -> Result<LocalString, i32> {
        match self.content() {
            StringData::Empty => StringRef::create_internal(rt, StringData::Empty, None),
            StringData::Ident => StringRef::create_internal(rt, StringData::Ident, None),
            StringData::AsciiStatic(s) => StringRef::create_static(rt, s, None),
            _ => StringRef::create(self.rt(), self.content().clone()),
        }
    }

    /// coerce string to number. if can't convert string to number, returns NAN.
    pub fn to_number(&self, ctx: &ContextImpl) -> JSValue {
        let str = self.content().encode_utf8(false);
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
                ctx.rt_ptr().take_exception();
                JSValue::Float64(f64::NAN)
            }
        }
    }

    #[inline(always)]
    pub fn to_local(&self) -> LocalString {
        LocalString::from(self.gc_ref().to_local(self.rt().gc_heap()))
    }

    /// convert String value to String object
    pub fn objectify(&self, ctx: &ContextImpl, ctor: JSValue) -> Result<Object, i32> {
        let len = self.len();
        let mut o = ctx
            .create_object_from_ctor(ctor, ObjectPayload::STRING(self.gc_ref().into()))
            .map_err(|_| -1)?;

        o.define_property_value(ctx, Atom::CONST_length, JSValue::from(len as u32), 0)?;
        Ok(o)
    }

    /// treat value `other` as string, and concat self with other.
    pub(crate) fn concat(&self, ctx: &ContextImpl, other: JSValue) -> Result<LocalString, i32> {
        let s2 = other.to_string(ctx)?;

        if self.is_empty() {
            return Ok(s2);
        } else if s2.is_empty() {
            return Ok(self.to_local());
        }

        // concat self, s2
        let buf = self
            .content()
            .to_buf()
            .and_then(|mut sb| sb.push_content(s2.content()).map(|_| sb))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        StringRef::create(ctx.rt_ptr(), StringData::from_buf(buf)).map(LocalString::from)
    }

    /// make sub string with content of given range.
    /// range is about buffer index, and end index is exclusive.
    pub fn sub_string(&self, ctx: &ContextImpl, range: Range<usize>) -> Result<LocalString, i32> {
        let end = std::cmp::min(range.end, self.len());

        if range.start == 0 && end == self.len() {
            Ok(self.to_local())
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

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct StringRef(GcRef<StringImpl>);

impl GcTrace for StringRef {
    #[inline(always)]
    fn trace(&self, tr: &mut GcTraceCtx) {
        tr.add(self.0);
    }
}

impl Deref for StringRef {
    type Target = StringImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl DerefMut for StringRef {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.deref_mut()
    }
}

impl PartialEq for StringRef {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.0.content() == other.0.content()
    }
}

impl std::fmt::Display for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.deref())
    }
}

impl std::fmt::Debug for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0.deref())
    }
}

impl From<GcRef<StringImpl>> for StringRef {
    fn from(r: GcRef<StringImpl>) -> Self {
        Self(r)
    }
}

impl From<GcRef<StringImpl>> for JSValue {
    #[inline(always)]
    fn from(value: GcRef<StringImpl>) -> Self {
        Self::String(StringRef(value))
    }
}

impl From<GcLocal<StringImpl>> for JSValue {
    #[inline(always)]
    fn from(value: GcLocal<StringImpl>) -> Self {
        Self::String(StringRef(value.gc_ref()))
    }
}

impl From<&GcLocal<StringImpl>> for JSValue {
    #[inline(always)]
    fn from(value: &GcLocal<StringImpl>) -> Self {
        Self::String(StringRef(value.gc_ref()))
    }
}

impl From<&GcLocal<StringImpl>> for StringRef {
    #[inline(always)]
    fn from(value: &GcLocal<StringImpl>) -> Self {
        StringRef(value.gc_ref())
    }
}

impl From<StringRef> for JSValue {
    #[inline(always)]
    fn from(value: StringRef) -> Self {
        Self::String(value)
    }
}

#[repr(transparent)]
pub struct LocalString(GcLocal<StringImpl>);

impl Deref for LocalString {
    type Target = StringImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl DerefMut for LocalString {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.deref_mut()
    }
}

impl std::fmt::Display for LocalString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.deref())
    }
}

impl std::fmt::Debug for LocalString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0.deref())
    }
}

impl LocalString {
    #[inline(always)]
    pub const fn gc_local(&self) -> &GcLocal<StringImpl> {
        &self.0
    }

    #[inline(always)]
    pub fn into_inner(self) -> GcLocal<StringImpl> {
        self.0
    }

    #[inline(always)]
    pub fn to_ref(&self) -> StringRef {
        StringRef(self.0.gc_ref())
    }
}

impl From<GcLocal<StringImpl>> for LocalString {
    #[inline(always)]
    fn from(value: GcLocal<StringImpl>) -> Self {
        LocalString(value)
    }
}

impl From<LocalString> for JSValue {
    #[inline(always)]
    fn from(value: LocalString) -> Self {
        JSValue::String(value.to_ref())
    }
}

impl From<&LocalString> for JSValue {
    #[inline(always)]
    fn from(value: &LocalString) -> Self {
        JSValue::String(value.to_ref())
    }
}

impl From<&LocalString> for StringRef {
    #[inline(always)]
    fn from(value: &LocalString) -> Self {
        value.to_ref()
    }
}

impl TryFrom<Object> for StringRef {
    type Error = i32;

    fn try_from(obj: Object) -> Result<Self, Self::Error> {
        Self::try_from(JSValue::Object(obj))
    }
}

impl TryFrom<JSValue> for StringRef {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        value.as_string().copied().ok_or(-1)
    }
}

impl StringRef {
    /// create String with string data, and optional atom info.
    fn create_internal(
        mut rt: RuntimePtr,
        content: StringData,
        atom_info: Option<AtomInfo>,
    ) -> Result<LocalString, i32> {
        let s = StringImpl::new(rt, content, atom_info);
        let ss = rt
            .gc_alloc_local(s)
            .map_err(|_| {
                eprintln!("[O.o] alloc error");
                -1
            })
            .map(LocalString)?;

        // O.o
        #[cfg(debug_assertions)]
        unsafe {
            use gc_lite::GcNode;

            let t = format!("[StringImpl] {}", ss.encode_utf8(true));
            ss.gc_head_ptr().as_mut().debug_set_dbg_string(t.into());
        }

        Ok(ss)
    }

    #[inline(always)]
    pub(crate) fn create(rt: RuntimePtr, content: StringData) -> Result<LocalString, i32> {
        Self::create_internal(rt, content, None)
    }

    #[inline]
    pub(crate) fn create_str(rt: RuntimePtr, s: &str) -> Result<LocalString, i32> {
        let sbuf = StringBuf::from_str(s)?;
        Self::create_internal(rt, StringData::from_buf(sbuf), None)
    }

    #[inline]
    pub(crate) fn create_utf8(rt: RuntimePtr, utf8_bytes: Vec<u8>) -> Result<LocalString, i32> {
        let data = StringData::from_buf(StringBuf::from_utf8(utf8_bytes).map_err(|_| -1)?);
        Self::create(rt, data)
    }

    /// static str `s` must be ascii str.
    #[inline(always)]
    pub(crate) fn create_static(
        rt: RuntimePtr,
        s: &'static str,
        atom_info: Option<AtomInfo>,
    ) -> Result<LocalString, i32> {
        Self::create_internal(rt, StringData::from_static(s), atom_info)
    }

    #[inline(always)]
    pub const fn gc_ref(&self) -> GcRef<StringImpl> {
        self.0
    }
}

impl ContextImpl {
    /// create string value from string buf.
    pub(crate) fn create_string_with_buf(&self, sbuf: StringBuf) -> Result<LocalString, i32> {
        if sbuf.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else {
            let data = StringData::from_buf(sbuf);
            StringRef::create(self.rt_ptr(), data)
                .map(LocalString::from)
                .map_err(|_| {
                    self.throw_out_of_memory();
                    -1
                })
        }
    }

    /// create string value from string content.
    pub(crate) fn create_string_with_content(
        &self,
        content: StringData,
    ) -> Result<LocalString, i32> {
        if content.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else {
            StringRef::create(self.rt_ptr(), content)
                .map(LocalString::from)
                .map_err(|_| {
                    self.throw_out_of_memory();
                    -1
                })
        }
    }

    /// create string from utf8 encoded data.
    pub fn create_string_from_utf8(&self, utf8_bytes: Vec<u8>) -> Result<LocalString, i32> {
        if utf8_bytes.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else if utf8_bytes.len() > JS_STRING_LEN_MAX {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            StringRef::create_utf8(self.rt_ptr(), utf8_bytes)
                .map(LocalString::from)
                .inspect_err(|_| {
                    self.throw_out_of_memory();
                })
        }
    }

    /// create string from str
    pub fn create_string_from_str(&self, str: &str) -> Result<LocalString, i32> {
        if unlikely(str.is_empty()) {
            Atom::CONST_empty_string.force_string(self)
        } else if unlikely(str.len() > JS_STRING_LEN_MAX) {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            StringRef::create_str(self.rt_ptr(), str)
                .map(LocalString::from)
                .inspect_err(|_| {
                    self.throw_out_of_memory();
                })
        }
    }

    /// create string value from a single string char.
    pub fn create_string_from_char(&self, ch: CharCode) -> Result<LocalString, i32> {
        let sbuf = match ch.encode_utf16(&mut [0; 2]) {
            &[b @ (0..0x80), _] => StringBuf::Ascii(vec![b as u8]),
            bb => StringBuf::Utf16(bb.to_vec()),
        };
        self.create_string_with_buf(sbuf)
    }
}

pub(crate) fn JS_ConcatString3(
    ctx: &ContextImpl,
    str1: &CStr,
    str2: JSValue,
    str3: &CStr,
) -> Result<LocalString, i32> {
    let p = if let JSValue::String(s) = str2 {
        s.to_local()
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

    /// An alias for [`JSValue::to_string_or_symbol`](Self::to_string_or_symbol).
    #[inline(always)]
    pub fn to_propkey(&self, ctx: &ContextImpl) -> Result<JSValue, i32> {
        self.to_string_or_symbol(ctx)
    }

    /// Coerce value `self` to string. if `self` is symbol, returns Err.
    pub fn to_string(&self, ctx: &ContextImpl) -> Result<LocalString, i32> {
        match self {
            Self::String(s) => Ok(s.to_local()),
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(_) => {
                ctx.throw_type_error("cannot convert symbol to string");
                Err(-1)
            }
            _ => match self.to_string_or_symbol(ctx)? {
                JSValue::String(s) => Ok(s.to_local()),
                #[cfg(feature = "builtin_symbol")]
                JSValue::Symbol(_) => {
                    ctx.throw_type_error("cannot convert symbol to string");
                    Err(-1)
                }
                _ => unreachable!(),
            },
        }
    }

    /// Coerce value `val` to string, but null or undefined are forbidden.
    pub fn to_string_non_undefined_or_null(&self, ctx: &ContextImpl) -> Result<LocalString, i32> {
        if unlikely(self.is_undefined_or_null()) {
            ctx.throw_type_error("null or undefined are forbidden");
            Err(-1)
        } else {
            self.to_string(ctx)
        }
    }

    /// Invoking `toLocaleString()` method.
    pub fn to_locale_string(&self, ctx: &ContextImpl) -> Result<JSValue, i32> {
        if self.is_undefined_or_null() {
            self.to_string(ctx).map(|s| JSValue::from(s))
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
        if ob.class_id() == JS_CLASS_STRING {
            let s = ob.payload().string().unwrap();
            let len = s.len();
            let nth_char = s.content().chars_since(idx as usize).next().map(|(_, c)| c);

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
                let nth_char2 = p2.content().chars().next().map(|(_, c)| c);

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

pub(crate) const js_string_exotic_methods: ClassExoticMethods = ClassExoticMethods {
    get_own_property: Some(
        |ctx: ContextPtr, obj: JSValue, prop: Atom| -> Result<Option<JSPropertyDescriptor>, i32> {
            // This is a class exotic method for JS_CLASS_STRING object
            if let Some(idx) = prop.to_small_int()
                && let Some(s) = obj.as_string()
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
    get_prototype: None,
    set_prototype: None,
};
