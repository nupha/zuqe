use std::{borrow::Cow, ffi::CStr, num::NonZeroU16, ops::Deref, rc::Rc};

use crate::{
    atom::AtomDesc,
    context::JSContext,
    mem::js_malloc_typed,
    number::IntOrFloat,
    string::{JSString, StringChar},
    value::JSValue,
};

use super::{AtomRegistry, consts::*};

/// atom descriptor kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum AtomDescKind {
    JS_ATOM_KIND_STRING,
    JS_ATOM_KIND_SYMBOL,
    JS_ATOM_KIND_PRIVATE,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum AtomTypeHint {
    String = 1,
    GlobalSymbol,
    Symbol,
    /// private symbol
    Private,
}

/// atom id and type
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum AtomHintAndId {
    String(NonZeroU16),
    GlobalSymbol(NonZeroU16),
    Symbol(NonZeroU16),
    /// private symbol
    Private(NonZeroU16),
}

impl AtomHintAndId {
    /// Get atom type hint
    pub const fn type_hint(&self) -> AtomTypeHint {
        match self {
            Self::String(_) => AtomTypeHint::String,
            Self::Symbol(_) => AtomTypeHint::Symbol,
            Self::GlobalSymbol(_) => AtomTypeHint::GlobalSymbol,
            Self::Private(_) => AtomTypeHint::Private,
        }
    }

    /// Get atom id.
    pub const fn id(&self) -> u32 {
        match self {
            Self::String(a) | Self::Symbol(a) | Self::GlobalSymbol(a) | Self::Private(a) => {
                a.get() as u32
            }
        }
    }

    /// Get atom ref.
    /// Returned atom is not duplicated, caller should clone to keep it referenced.
    #[inline(always)]
    pub const fn atom_ref(&self) -> AtomRef {
        AtomRef(self.id())
    }
}

/// Atom that hold one refcount of atom description.
/// Will auto free atom description when dropped.
#[repr(transparent)]
#[derive(Debug, PartialEq)]
pub struct Atom(pub(super) u32);

impl Drop for Atom {
    fn drop(&mut self) {
        if !self.is_const() {
            AtomRegistry::free(self.0);
        }
    }
}

impl Default for Atom {
    fn default() -> Self {
        Self::NULL
    }
}

impl Atom {
    pub const NULL: Self = Self(0);

    #[inline(always)]
    pub const fn id(&self) -> u32 {
        self.0
    }

    /// get atom description. returns None if atom does not exist.
    #[inline(always)]
    pub(crate) fn description(&self) -> AtomDesc {
        AtomRegistry::get_desc(self.0).unwrap()
    }

    #[inline(always)]
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// check if atom is const atom?
    #[inline(always)]
    pub const fn is_const(&self) -> bool {
        is_const_atom(self.0)
    }

    /// get atom descriptor kind
    pub fn kind(&self) -> AtomDescKind {
        if self.is_small_int() {
            AtomDescKind::JS_ATOM_KIND_STRING
        } else {
            let p = self.description();
            match p.atom_info().unwrap() {
                AtomHintAndId::String(_) => AtomDescKind::JS_ATOM_KIND_STRING,
                AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_) => {
                    AtomDescKind::JS_ATOM_KIND_SYMBOL
                }
                AtomHintAndId::Private(_) => AtomDescKind::JS_ATOM_KIND_PRIVATE,
            }
        }
    }

    /// test if atom has normal string description
    pub fn has_string_desc(&self) -> bool {
        if self.is_small_int() {
            false
        } else {
            let p = self.description();
            !p.content().is_ident() && {
                let a = p.atom_info().unwrap();
                (matches!(a, AtomHintAndId::Symbol(_)) && p.hash() == JS_ATOM_HASH_SYMBOL)
                    || matches!(a, AtomHintAndId::GlobalSymbol(_))
            }
        }
    }

    /// test if the atom is small int type
    #[inline(always)]
    pub const fn is_small_int(&self) -> bool {
        (self.0 & JS_ATOM_TAG_INT) != 0
    }

    /// if atom is integral and the int is in range [0, JS_ATOM_MAX_INT],
    /// returns the integer.
    #[inline]
    pub const fn to_small_int(&self) -> Option<u32> {
        if self.is_small_int() {
            Some(self.0 & !JS_ATOM_TAG_INT)
        } else {
            None
        }
    }

    /// Check if atom can be used as a numeric index property,
    /// if yes, returns the index.
    pub fn to_numeric_index(&self, ctx: &JSContext) -> Result<Option<IntOrFloat>, i32> {
        if let Some(n) = self.to_small_int() {
            return Ok(Some(IntOrFloat::Int64(n as _)));
        }

        let d = self.description();
        let desc: JSString<'_> = unsafe {
            // SAFETY: Atom descriptions have 'static lifetime and are safe to transmute
            std::mem::transmute(d)
        };

        let content = desc.content();
        if content.is_empty() || content.is_ident() {
            return Ok(None);
        }

        if let AtomHintAndId::String(_) = desc.atom_info().unwrap() {
            let buf_len = content.len();

            // check for: -0, Infinity, -Infinity
            let mut chars = content.iter();

            if let Some((_, c)) = chars.next() {
                if let Some(mut ch) = c.char() {
                    if ch == '-' {
                        if let Some((_, StringChar::Char(c2))) = chars.next() {
                            // -0 case is specific
                            if c2 == '0' && buf_len == 2 {
                                return Ok(Some(IntOrFloat::Float64(-0.0)));
                            } else {
                                ch = c2;
                            }
                        } else {
                            return Ok(None);
                        }
                    }

                    if !ch.is_ascii_digit() {
                        // XXX: should test NaN, but the tests do not check it
                        if ch == 'I' {
                            let mut nfinity = "nfinity".chars();
                            loop {
                                match (chars.next(), nfinity.next()) {
                                    (Some((_, StringChar::Char(a))), Some(b)) if a == b => {}
                                    (None, None) => {
                                        break; // Infinity is allowed
                                    }
                                    _ => {
                                        return Ok(None);
                                    }
                                }
                            }
                        } else {
                            return Ok(None);
                        }
                    }
                } else {
                    // broken surrogate
                    return Ok(None);
                }
            }

            // XXX: bignum: would be better to only accept integer to avoid
            // relying on current floating point precision
            // this is ECMA CanonicalNumericIndexString primitive
            let num = desc.to_number(ctx);

            // test num as string is same to original value
            let str = num.to_string(ctx)?;
            if content.eq(str.content()) {
                match num {
                    JSValue::Int(i) => Ok(Some(IntOrFloat::Int64(i as _))),
                    JSValue::Float64(d) => Ok(Some(IntOrFloat::Float64(d))),
                    _ => unreachable!(),
                }
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    pub fn is_numeric_index(&self, ctx: &JSContext) -> Result<bool, i32> {
        self.to_numeric_index(ctx).map(|x| x.is_some())
    }

    /// Convert atom to String or Symbol value.
    pub fn string_or_symbol<'a>(&self, ctx: &JSContext<'a>) -> Result<JSValue<'a>, i32> {
        if let Some(n) = self.to_small_int() {
            ctx.create_string_from_utf8(format!("{n}",).into_bytes())
                .map(JSValue::String)
        } else {
            let d = unsafe {
                // SAFETY: transmute 'static to 'a is safe
                std::mem::transmute::<_, JSString<'a>>(self.description())
            };

            match d.atom_info().unwrap() {
                AtomHintAndId::String(_) | AtomHintAndId::Private(_) => Ok(JSValue::String(d)),
                AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_) => Ok(JSValue::Symbol(d)),
            }
        }
    }

    /// Force convert atom to String.
    /// If atom is Symbol kind, returns its string descriptor.
    pub fn force_string<'a>(&self, ctx: &JSContext<'a>) -> Result<JSString<'a>, i32> {
        match self.string_or_symbol(ctx)? {
            JSValue::String(s) => {
                debug_assert!(!s.content().is_ident());
                Ok(s)
            }
            JSValue::Symbol(d) => {
                // force symbol to string
                if d.content().is_ident() {
                    JS_ATOM_empty_string.force_string(ctx)
                } else {
                    Ok(d)
                }
            }
            _ => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                unsafe {
                    std::hint::unreachable_unchecked();
                }
            }
        }
    }

    /// if atom can be used as array index, which is an int in range \[0, 2^32-1), returns the int.
    pub fn to_array_index(&self) -> Option<u32> {
        if self.0 == 0 {
            None
        } else if let Some(n) = self.to_small_int() {
            Some(n)
        } else {
            let p = self.description();
            if let AtomHintAndId::String(_) = p.atom_info().unwrap() {
                // Per js specification, array length could be 2^32-1 in max,
                // thus array index must be less than u32::MAX.
                if let Some(n) = p.content().parse_u32()
                    && n < u32::MAX
                {
                    return Some(n);
                }
            }

            None
        }
    }

    /// if atom can be represented/parsed as an i64, returns the i64.
    pub fn to_i64(&self) -> Option<i64> {
        if self.0 == 0 {
            None
        } else if let Some(n) = self.to_small_int() {
            Some(n as i64)
        } else {
            let p = self.description();
            if let AtomHintAndId::String(_) = p.atom_info().unwrap() {
                return p.content().parse_i64();
            }
            None
        }
    }

    /// run closure with utf8 bytes converted from atom
    pub fn with_utf8<R>(&self, f: impl FnOnce(Cow<'_, str>) -> R) -> R {
        if self.is_null() {
            f(Cow::Borrowed("<null>"))
        } else if let Some(n) = self.to_small_int() {
            f(Cow::Owned(format!("{n}")))
        } else {
            f(self.description().content().to_utf8(false))
        }
    }

    /// convert atom to utf8 encoded String
    #[inline(always)]
    pub fn to_utf8(&self) -> String {
        self.with_utf8(|s| s.into_owned())
    }

    /// make AtomRef from Atom
    #[inline(always)]
    pub const fn to_ref(&self) -> AtomRef {
        AtomRef(self.0)
    }

    /// compare atom with AtomRef
    #[inline(always)]
    pub const fn eq_ref(&self, other: AtomRef) -> bool {
        self.0 == other.0
    }

    /// consume the atom, return the atom id, DO NOT decrease refcount.
    #[inline(always)]
    pub(crate) unsafe fn leak(mut self) -> u32 {
        std::mem::replace(&mut self.0, 0)
    }
}

impl Clone for Atom {
    fn clone(&self) -> Self {
        if !self.is_const() {
            let p = AtomRegistry::get_desc(self.0).unwrap();
            let _ = Rc::into_raw(p.0); // leak refcount for atom
        }

        Self(self.0)
    }
}

/// the atom reference. not change refcount.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomRef(pub(crate) u32);

impl Deref for AtomRef {
    type Target = Atom;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { std::mem::transmute::<&AtomRef, &Atom>(self) }
    }
}

impl AtomRef {
    pub const NULL: Self = Self(0);
}

pub const JS_ATOM_NULL: AtomRef = AtomRef(0);

pub const JS_ATOM_HASH_SYMBOL: u32 = 0;
pub const JS_ATOM_HASH_PRIVATE: u32 = 1;
pub const JS_ATOM_HASH_MASK: u32 = (1 << 30) - 1;

pub const JS_ATOM_TAG_INT: u32 = 1 << 31;
pub const JS_ATOM_MAX_INT: u32 = JS_ATOM_TAG_INT - 1;
pub const JS_ATOM_MAX: u32 = u16::MAX as u32 - 1;

pub const ATOM_GET_STR_BUF_SIZE: usize = 64;

impl<'a> JSString<'a> {
    /// convert string to atom. if string is not registered as atom yet, do registration.
    pub fn to_atom(&self) -> Result<Atom, i32> {
        if let Some(a) = self.atom_info() {
            // str already registered as atom
            Ok(a.atom_ref().deref().clone())
        } else {
            // register as string atom
            Atom::from_string(self.clone(), AtomTypeHint::String)
        }
    }
}

impl<'a> JSContext<'a> {
    pub fn create_atom_string(&self, str: &str) -> Result<JSString<'a>, i32> {
        let atom = Atom::new(self.rt().as_ref(), str)?;
        atom.force_string(self)
    }
}

impl<'a> JSValue<'a> {
    /// convert value to atom. if value is not string or symbol, corece value to string then convert to atom.
    ///
    /// Note: returned atom is cloned, caller should free it after use.
    pub fn to_atom(&self, ctx: &JSContext<'a>) -> Result<Atom, i32> {
        let mut val = self.clone();

        loop {
            match val {
                JSValue::UNDEFINED => {
                    return Ok(Atom::new_const(JS_ATOM_undefined));
                }
                JSValue::NULL => {
                    return Ok(Atom::new_const(JS_ATOM_null));
                }
                JSValue::Bool(b) => {
                    return Ok(Atom::new_const(if b {
                        JS_ATOM_true
                    } else {
                        JS_ATOM_false
                    }));
                }
                JSValue::Int(i) => {
                    return Atom::from_i64(ctx.rt().as_ref(), i as i64);
                }
                JSValue::String(s) => {
                    return s.to_atom();
                }
                JSValue::Symbol(d) => {
                    return d.to_atom();
                }

                JSValue::Object(_) | JSValue::Float64(_) => {
                    val = self.to_string_or_symbol(ctx)?;
                    continue;
                }

                JSValue::UNINITIALIZED
                | JSValue::CatchOffset(_)
                | JSValue::Module(_)
                | JSValue::Bytecode(_)
                | JSValue::AsyncStateResume(_) => {
                    ctx.throw_type_error("cannot convert to atom");
                    return Err(-1);
                }

                JSValue::EXCEPTION => {
                    #[cfg(debug_assertions)]
                    unreachable!();
                    #[cfg(not(debug_assertions))]
                    return Err(-1);
                }
            }
        }
    }
}

/// return a string atom containing name concatenated with str1.
/// Note: returned atom should be freed by caller.
pub unsafe fn js_atom_concat_str(ctx: &JSContext, name: &Atom, str1: *const u8) -> Atom {
    let str_val = if let Ok(s) = name.force_string(ctx) {
        s
    } else {
        return Atom::NULL;
    };
    let astr = str_val.content().to_utf8(false);
    let len = astr.len();
    let len1 = unsafe { CStr::from_ptr(str1.cast()).count_bytes() };
    let ttl_len = len + len1 + 1;

    // FIXME: use Vec::try_reserve_exact()
    if let Some(p) = js_malloc_typed::<u8>(ctx, ttl_len, false) {
        let mut new_str = Vec::from_raw_parts(p.as_ptr(), ttl_len, ttl_len);
        std::ptr::copy_nonoverlapping(astr.as_ptr().cast(), new_str.as_mut_ptr(), len);
        std::ptr::copy_nonoverlapping(str1, new_str.as_mut_ptr().offset(len as isize), len1);

        Atom::new(ctx.rt().as_ref(), std::str::from_utf8_unchecked(&new_str)).unwrap_or(Atom::NULL)
    } else {
        Atom::NULL
    }
}

/// Note: returned atom should be freed by caller.
pub fn js_atom_concat_num(ctx: &JSContext, name: &Atom, n: u32) -> Atom {
    unsafe { js_atom_concat_str(ctx, name, format!("{n}\0").as_ptr()) }
}

#[inline(always)]
pub(super) const fn is_const_atom(id: u32) -> bool {
    // Hint: for u32 atoms, `self.0 as i32` should be minus.
    (id as i32) < JS_ATOM_INIT_COUNT as i32
}
