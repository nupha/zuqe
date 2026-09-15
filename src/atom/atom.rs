// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::borrow::Cow;

use gc_lite::{GcNode, GcTrace, GcTraceCtx};

use crate::{
    RuntimeImpl,
    atom::{ATOM_CONST_DEFS, AtomDescKind, AtomKind, USER_ATOM_ID_START},
    context::ScopedContext,
    number::IntOrFloat,
    string::{CharCode, StringImpl, StringRef},
    value::{JSValue, JSValueTag},
};

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord)]
pub struct Atom(pub(super) u32);

impl GcTrace for Atom {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if !self.is_null() && !self.is_const() && !self.is_small_int() {
            let rt = tr.opaque().cast::<RuntimeImpl>();
            debug_assert!(!rt.is_null());

            let s = unsafe { &*rt }.atom_desc_string(self.id()).unwrap();
            s.trace(tr);
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

    /// Take ownership of atom id, will free it when dropped.
    #[inline(always)]
    pub(crate) const fn from_id(id: u32) -> Self {
        Self(id)
    }

    /// Build a small-int atom for an array index (`0..=JS_ATOM_MAX_INT`).
    /// Allocation-free, for per-index property lookups (e.g. sparse-array hole checks).
    #[inline]
    pub fn from_small_int(idx: u32) -> Self {
        debug_assert!((0..=JS_ATOM_MAX_INT).contains(&idx));
        Self(JS_ATOM_TAG_INT | idx)
    }

    #[inline(always)]
    pub const fn id(&self) -> u32 {
        self.0
    }

    #[inline(always)]
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// test if the atom is small int type
    #[inline(always)]
    pub const fn is_small_int(&self) -> bool {
        is_small_int_atom(self.0)
    }

    /// check if atom is const atom?
    #[inline(always)]
    pub const fn is_const(&self) -> bool {
        is_const_atom(self.0)
    }

    /// const atom content, from the static table.
    #[inline]
    pub(crate) fn const_str(&self) -> &'static str {
        debug_assert!(!self.is_null() && self.is_const());
        ATOM_CONST_DEFS[self.id() as usize - 1].0
    }

    /// const atom kind, from the static table.
    #[inline]
    pub(crate) fn const_kind(&self) -> AtomKind {
        debug_assert!(!self.is_null() && self.is_const());
        ATOM_CONST_DEFS[self.id() as usize - 1].1
    }

    /// get atom descriptor kind
    pub(crate) fn desc_kind(&self, rt: &RuntimeImpl) -> AtomDescKind {
        if self.is_small_int() {
            AtomDescKind::JS_ATOM_KIND_STRING
        } else {
            match self.kind(rt) {
                AtomKind::String => AtomDescKind::JS_ATOM_KIND_STRING,
                AtomKind::Symbol | AtomKind::GlobalSymbol => AtomDescKind::JS_ATOM_KIND_SYMBOL,
                AtomKind::Private => AtomDescKind::JS_ATOM_KIND_PRIVATE,
            }
        }
    }

    /// get the atom kind (const table for const atoms, descriptor otherwise).
    #[inline]
    pub(crate) fn kind(&self, rt: &RuntimeImpl) -> AtomKind {
        if self.is_const() {
            self.const_kind()
        } else if self.0 == 0 || self.is_small_int() {
            AtomKind::String
        } else {
            rt.atom_desc_string(self.id())
                .unwrap()
                .atom_info()
                .unwrap()
                .kind()
        }
    }

    /// content of any atom as a string.
    #[inline]
    pub(crate) fn str(&self, rt: &RuntimeImpl) -> Cow<'_, str> {
        if self.is_null() {
            Cow::Borrowed("<null>")
        } else if let Some(n) = self.to_small_int() {
            Cow::Owned(format!("{n}"))
        } else if self.is_const() {
            Cow::Borrowed(self.const_str())
        } else {
            Cow::Owned(
                rt.atom_desc_string(self.id())
                    .unwrap()
                    .content()
                    .encode_utf8(false)
                    .into_owned(),
            )
        }
    }

    /// get the [`StringRef`] backing this atom, allocating a (static) copy for
    /// const atoms. `null` maps to a static `"<null>"`.
    #[inline]
    pub(crate) fn to_string_ref(self, ctx: &mut ScopedContext) -> Result<StringRef, i32> {
        if self.is_null() {
            StringRef::alloc_static(ctx, "<null>", None)
        } else if let Some(n) = self.to_small_int() {
            StringRef::from_utf8_bytes(ctx, format!("{n}").into_bytes())
        } else if self.is_const() {
            StringRef::alloc_static(ctx, self.const_str(), None)
        } else {
            Ok(ctx.rt().atom_desc_string(self.id()).unwrap())
        }
    }

    /// test if atom has *NORMAL* descriptor string.
    /// 'Normal' means string desc is not for anonymous-symbol or private-symbol.
    pub(crate) fn is_normal_desc_string(&self, rt: &RuntimeImpl) -> bool {
        debug_assert!(!self.is_null());

        if self.is_small_int() {
            false
        } else if self.is_const() {
            // Private_brand is the only const Symbol-kind atom that is not a
            // normal (well-known) symbol desc.
            *self != Self::CONST_Private_brand
                && matches!(self.const_kind(), AtomKind::GlobalSymbol | AtomKind::Symbol)
        } else {
            let s = rt.atom_desc_string(self.id()).unwrap();
            !s.content().is_ident() && {
                let a = s.atom_info().unwrap();
                let k = a.kind();
                k == AtomKind::GlobalSymbol
                    || (k == AtomKind::Symbol && a.hash() == JS_ATOM_HASH_SYMBOL)
            }
        }
    }

    /// check atom is still registered, string desc (if any) is still alive in gc.
    #[cfg(debug_assertions)]
    pub fn is_alive(&self, rt: &RuntimeImpl) -> bool {
        if !self.is_const() && !self.is_null() && !self.is_small_int() {
            rt.atom_desc_string(self.id()).is_some()
        } else {
            true
        }
    }

    /// if atom is integral and the int is in range \[0, JS_ATOM_MAX_INT\],
    /// returns the integer.
    #[inline]
    pub const fn to_small_int(&self) -> Option<u32> {
        if self.is_small_int() {
            Some(self.0 & !JS_ATOM_TAG_INT)
        } else {
            None
        }
    }

    /// Check if atom can be used as a numeric index property, if yes, returns the index.
    ///
    /// Try to interpret this atom as a canonical numeric index (ECMA CanonicalNumericIndexString).
    /// Fast‑path: if the atom is stored as a small integer, return it as Int64.
    /// Otherwise, if it is a string, accept only canonical numeric forms such as
    /// "0", "42", "-0", "Infinity", "-Infinity" and reject non‑canonical ones like
    /// "01", "1.0" or "1e2".
    /// Returns None if the atom is not a numeric index.
    pub fn to_numeric_index(&self, ctx: &mut ScopedContext) -> Result<Option<IntOrFloat>, i32> {
        if let Some(n) = self.to_small_int() {
            return Ok(Some(IntOrFloat::Int64(n as _)));
        }

        if self.is_null() || self.is_const() {
            // null; const atoms are keywords/names, never canonical numeric
            // index strings.
            return Ok(None);
        }

        let s = ctx.rt().atom_desc_string(self.id()).unwrap();
        let content = s.content();

        if content.is_empty() || content.is_ident() {
            return Ok(None);
        }

        // ECMA-262 CanonicalNumericIndexString special values: NaN, ±Infinity.
        if let Some(b) = content.ascii() {
            match b {
                b"NaN" => {
                    return Ok(Some(IntOrFloat::Float64(f64::NAN)));
                }
                b"Infinity" => {
                    return Ok(Some(IntOrFloat::Float64(f64::INFINITY)));
                }
                b"-Infinity" => {
                    return Ok(Some(IntOrFloat::Float64(f64::NEG_INFINITY)));
                }
                _ => {}
            }
        }

        if let AtomKind::String = s.atom_info().unwrap().kind() {
            let buf_len = content.len();
            let mut chars = content.chars();

            if let Some((_, c)) = chars.next() {
                if let Some(mut ch) = c.char() {
                    if ch == '-' {
                        if let Some((_, CharCode::Char(c2))) = chars.next() {
                            if c2 == '0' && buf_len == 2 {
                                return Ok(Some(IntOrFloat::Float64(-0.0)));
                            } else {
                                ch = c2;
                            }
                        } else {
                            return Ok(None);
                        }
                    }

                    // For decimal numbers (digit or leading sign then digit)
                    if !ch.is_ascii_digit() {
                        return Ok(None);
                    }
                } else {
                    return Ok(None);
                }

                // ECMA CanonicalNumericIndexString primitive
                let num = s.to_number(ctx);
                let str_val = JSValue::from(num).to_string(ctx)?;
                if s.content().eq(str_val.content()) {
                    return Ok(Some(num));
                }
            }
        }

        Ok(None)
    }

    #[inline]
    pub fn is_numeric_index(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        self.to_numeric_index(ctx).map(|x| x.is_some())
    }

    /// Convert atom to String or Symbol value.
    pub fn to_value(&self, ctx: &mut ScopedContext) -> Result<JSValue, i32> {
        if let Some(n) = self.to_small_int() {
            ctx.create_string_from_utf8(format!("{n}",).into_bytes())
                .map(JSValue::make_string)
        } else if self.is_null() {
            Ok(JSValue::NULL)
        } else if self.is_const() {
            if matches!(self.const_kind(), AtomKind::Symbol | AtomKind::GlobalSymbol) {
                ctx.rt_mut()
                    .const_symbol_value(self.id())
                    .map(JSValue::make_symbol)
            } else {
                self.to_string_ref(ctx).map(JSValue::make_string)
            }
        } else {
            let s = ctx.rt().atom_desc_string(self.id()).unwrap();
            match s.atom_info().unwrap().kind() {
                AtomKind::String => Ok(JSValue::make_string(s)),
                AtomKind::Private => Ok(JSValue::make_string(s)),
                AtomKind::Symbol | AtomKind::GlobalSymbol => Ok(JSValue::make_symbol(s)),
            }
        }
    }

    /// Force converting atom to StringRef.
    /// For anonymous symbol, ident convert to empty string value.
    pub fn force_string(&self, ctx: &mut ScopedContext) -> Result<StringRef, i32> {
        if let Some(n) = self.to_small_int() {
            StringRef::from_utf8_bytes(ctx, format!("{n}").into_bytes())
        } else if self.is_null() {
            StringRef::alloc_static(ctx, "<null>", None)
        } else if self.is_const() {
            self.to_string_ref(ctx)
        } else {
            let s = ctx.rt().atom_desc_string(self.id()).unwrap();
            if s.content().is_ident() {
                Self::CONST_empty_string.force_string(ctx)
            } else {
                Ok(s)
            }
        }
    }

    /// if atom can be used as array index, which is an int in range \[0, 2^32-1), returns the int.
    pub fn to_array_index(&self, rt: &RuntimeImpl) -> Option<u32> {
        if self.0 == 0 || self.is_const() {
            None
        } else if let Some(n) = self.to_small_int() {
            Some(n)
        } else {
            let s = rt.atom_desc_string(self.id()).unwrap();
            let atom_info = s.atom_info().unwrap();

            if let AtomKind::String = atom_info.kind() {
                // Per js specification, array length could be 2^32-1 in max,
                // thus array index must be less than u32::MAX.
                if let Some(n) = s.content().parse_u32()
                    && n < u32::MAX
                {
                    return Some(n);
                }
            }

            None
        }
    }

    /// if atom can be represented/parsed as an i64, returns the i64.
    pub fn to_i64(&self, rt: &RuntimeImpl) -> Option<i64> {
        if self.0 == 0 || self.is_const() {
            None
        } else if let Some(n) = self.to_small_int() {
            Some(n as i64)
        } else {
            let s = rt.atom_desc_string(self.id()).unwrap();
            if let AtomKind::String = s.atom_info().unwrap().kind() {
                return s.content().parse_i64();
            }
            None
        }
    }

    /// run closure with utf8 bytes converted from atom
    pub fn with_utf8<R>(&self, rt: &RuntimeImpl, f: impl FnOnce(Cow<'_, str>) -> R) -> R {
        if self.is_null() {
            f(Cow::Borrowed("<null>"))
        } else if let Some(n) = self.to_small_int() {
            f(Cow::Owned(format!("{n}")))
        } else if self.is_const() {
            f(Cow::Borrowed(self.const_str()))
        } else {
            f(rt.atom_desc_string(self.id())
                .unwrap()
                .content()
                .encode_utf8(false))
        }
    }

    /// convert atom to utf8 encoded String
    #[inline(always)]
    pub fn to_utf8(&self, rt: &RuntimeImpl) -> String {
        self.with_utf8(rt, |s| s.into_owned())
    }

    /// Return the JSValue for this const atom if it is a JSON primitive
    /// (true / false / null), otherwise None.
    #[inline(always)]
    pub fn const_value(&self) -> Option<JSValue> {
        if *self == Atom::CONST_true {
            Some(JSValue::TRUE)
        } else if *self == Atom::CONST_false {
            Some(JSValue::FALSE)
        } else if *self == Atom::CONST_null {
            Some(JSValue::NULL)
        } else {
            None
        }
    }
}

pub const JS_ATOM_HASH_SYMBOL: u32 = 0;
pub const JS_ATOM_HASH_PRIVATE: u32 = 1;
pub const JS_ATOM_HASH_MASK: u32 = (1 << 30) - 1;

pub const JS_ATOM_TAG_INT: u32 = 1 << 31;
pub const JS_ATOM_MAX_INT: u32 = JS_ATOM_TAG_INT - 1;
/// Max user/const atom id. Atom is a u32 whose highest bit is the small-int tag,
/// so a regular (registered) atom id occupies `1..=i32::MAX` (bit31 clear; 0 is Atom::NULL).
pub const JS_ATOM_MAX: u32 = i32::MAX as u32;

impl StringImpl {
    /// convert string to atom. if string is not registered as atom yet, do registration.
    pub(crate) fn to_atom(&self, ctx: &mut ScopedContext, kind: AtomKind) -> Result<Atom, i32> {
        if let Some(a) = self.atom_info() {
            // str already registered as atom
            Ok(Atom::from_id(a.id()))
        } else {
            // register as atom
            Atom::intern_content(ctx, self.gc_ref().into(), kind)
        }
    }
}

impl JSValue {
    /// convert value to atom. if value is not string or symbol, corece value to string then convert to atom.
    ///
    /// Note: returned atom is cloned, caller should free it after use.
    pub fn to_atom(&self, ctx: &mut ScopedContext) -> Result<Atom, i32> {
        let mut val = *self;

        loop {
            match val.tag() {
                JSValueTag::Undefined => {
                    return Ok(Atom::CONST_undefined);
                }
                JSValueTag::Null => {
                    return Ok(Atom::CONST_null);
                }
                JSValueTag::Bool => {
                    return Ok(if val.payload_i32() != 0 {
                        Atom::CONST_true
                    } else {
                        Atom::CONST_false
                    });
                }
                JSValueTag::Int => {
                    return Atom::new_i64(ctx, val.payload_i32() as i64);
                }
                JSValueTag::String => unsafe {
                    return val.as_string_unchecked().to_atom(ctx, AtomKind::String);
                },
                JSValueTag::Symbol => unsafe {
                    return val.as_string_unchecked().to_atom(ctx, AtomKind::Symbol);
                },
                JSValueTag::Object | JSValueTag::Float64 | JSValueTag::BigInt => {
                    val = val.to_string_or_symbol(ctx)?;
                    continue;
                }
                JSValueTag::Uninitialized
                | JSValueTag::CatchOffset
                | JSValueTag::Module
                | JSValueTag::FunctionBytecode => {
                    return Err(ctx.throw_type_error("cannot convert to atom"));
                }

                JSValueTag::StringRope | JSValueTag::Exception => {
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
pub(crate) fn js_atom_concat_str(
    ctx: &mut ScopedContext,
    atom: Atom,
    str1: &str,
) -> Result<Atom, i32> {
    let str_val = atom.force_string(ctx)?;
    let astr = str_val.content().encode_utf8(false);

    let mut buf = String::new();
    buf.try_reserve_exact(astr.len() + str1.len())
        .map_err(|_| ctx.throw_out_of_memory())?;

    buf.push_str(&astr);
    buf.push_str(str1);

    let s = ctx.create_string_from_utf8(buf.into_bytes())?;
    Atom::intern_content(ctx, s, AtomKind::String)
}

/// Note: returned atom should be freed by caller.
#[inline(always)]
pub fn js_atom_concat_num(ctx: &mut ScopedContext, atom: Atom, n: u32) -> Result<Atom, i32> {
    js_atom_concat_str(ctx, atom, &format!("{n}"))
}

#[inline(always)]
pub(crate) const fn is_const_atom(id: u32) -> bool {
    // const atoms live in `1..USER_ATOM_ID_START`; id 0 is Atom::NULL and
    // small-int atoms have the high tag bit set.
    id != 0 && id < USER_ATOM_ID_START
}

#[inline(always)]
pub(crate) const fn is_small_int_atom(id: u32) -> bool {
    (id & JS_ATOM_TAG_INT) != 0
}
