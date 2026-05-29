// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::borrow::Cow;

use gc_lite::{GcNode, GcTrace, GcTraceCtx};

use crate::{
    RuntimeImpl,
    atom::{ATOM_CONST_COUNT, AtomDescKind, AtomKind},
    context::ScopedContext,
    number::IntOrFloat,
    runtime::RuntimePtr,
    string::{CharCode, StringData, StringImpl, StringRef},
    value::JSValue,
};

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord)]
pub struct Atom(pub(super) u32);

impl GcTrace for Atom {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if !self.is_const() && !self.is_small_int() {
            let rt = tr.opaque().cast::<RuntimeImpl>();
            debug_assert!(!rt.is_null());

            let s = self.descriptor(unsafe { &*rt });
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

    /// get atom descriptor string.
    #[inline]
    pub(crate) fn descriptor(&self, rt: &RuntimeImpl) -> StringRef {
        rt.atom_desc_string(self.id()).unwrap()
    }

    /// get atom descriptor kind
    pub(crate) fn desc_kind(&self, rt: &RuntimeImpl) -> AtomDescKind {
        if self.is_small_int() {
            AtomDescKind::JS_ATOM_KIND_STRING
        } else {
            match self.descriptor(rt).atom_info().unwrap().kind() {
                AtomKind::String => AtomDescKind::JS_ATOM_KIND_STRING,
                #[cfg(feature = "builtin_symbol")]
                AtomKind::Symbol | AtomKind::GlobalSymbol => AtomDescKind::JS_ATOM_KIND_SYMBOL,
                #[cfg(feature = "builtin_symbol")]
                AtomKind::Private => AtomDescKind::JS_ATOM_KIND_PRIVATE,
            }
        }
    }

    /// test if atom has *NORMAL* descriptor string.
    /// 'Normal' means string desc is not for anonymous-symbol or private-symbol.
    pub(crate) fn is_normal_desc_string(&self, rt: &RuntimeImpl) -> bool {
        debug_assert!(!self.is_null());

        if self.is_small_int() {
            false
        } else {
            #[cfg(feature = "builtin_symbol")]
            {
                let s = self.descriptor(rt);
                !s.content().is_ident() && {
                    let a = s.atom_info().unwrap();
                    let k = a.kind();
                    k == AtomKind::GlobalSymbol
                        || (k == AtomKind::Symbol && a.hash() == JS_ATOM_HASH_SYMBOL)
                }
            }

            #[cfg(not(feature = "builtin_symbol"))]
            {
                debug_assert!(!self.desc_string(rt).content().is_ident());
                true
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

        let s = self.descriptor(ctx.rt());
        let atom_info = s.atom_info().unwrap();
        let content = s.content();

        if content.is_empty() || content.is_ident() {
            return Ok(None);
        }

        if let AtomKind::String = atom_info.kind() {
            let buf_len = content.len();

            // check for: -0, Infinity, -Infinity
            let mut chars = content.chars();

            if let Some((_, c)) = chars.next() {
                if let Some(mut ch) = c.char() {
                    if ch == '-' {
                        if let Some((_, CharCode::Char(c2))) = chars.next() {
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
                                    (Some((_, CharCode::Char(a))), Some(b)) if a == b => {}
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

            if !self.is_const() {
                // XXX: bignum: would be better to only accept integer to avoid
                // relying on current floating point precision
                // this is ECMA CanonicalNumericIndexString primitive
                let num = s.to_number(ctx);

                // test num as string is same to original value
                let str = JSValue::from(num).to_string(ctx)?;
                if content.eq(str.content()) {
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
                .map(JSValue::from)
        } else {
            let s = self.descriptor(ctx.rt());
            match s.atom_info().unwrap().kind() {
                AtomKind::String => Ok(JSValue::from(s)),
                #[cfg(feature = "builtin_symbol")]
                AtomKind::Private => Ok(JSValue::from(s)),
                #[cfg(feature = "builtin_symbol")]
                AtomKind::Symbol | AtomKind::GlobalSymbol => Ok(JSValue::Symbol(s)),
            }
        }
    }

    /// Force converting atom to StringRef.
    /// For anonymous symbol, ident convert to empty string value.
    pub fn force_string(&self, ctx: &mut ScopedContext) -> Result<StringRef, i32> {
        if let Some(n) = self.to_small_int() {
            StringRef::create_utf8(ctx, format!("{n}").into_bytes())
        } else {
            let s = self.descriptor(ctx.rt());
            if s.content().is_ident() {
                Self::CONST_empty_string.force_string(ctx)
            } else {
                Ok(s)
            }
        }
    }

    /// if atom can be used as array index, which is an int in range \[0, 2^32-1), returns the int.
    pub fn to_array_index(&self, rt: &RuntimeImpl) -> Option<u32> {
        if self.0 == 0 {
            None
        } else if let Some(n) = self.to_small_int() {
            Some(n)
        } else {
            let s = self.descriptor(rt);
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
        if self.0 == 0 {
            None
        } else if let Some(n) = self.to_small_int() {
            Some(n as i64)
        } else {
            let s = self.descriptor(rt);
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
        } else {
            f(self.descriptor(rt).content().encode_utf8(false))
        }
    }

    /// convert atom to utf8 encoded String
    #[inline(always)]
    pub fn to_utf8(&self, rt: &RuntimeImpl) -> String {
        self.with_utf8(rt, |s| s.into_owned())
    }
}

pub const JS_ATOM_HASH_SYMBOL: u32 = 0;
pub const JS_ATOM_HASH_PRIVATE: u32 = 1;
pub const JS_ATOM_HASH_MASK: u32 = (1 << 30) - 1;

pub const JS_ATOM_TAG_INT: u32 = 1 << 31;
pub const JS_ATOM_MAX_INT: u32 = JS_ATOM_TAG_INT - 1;
pub const JS_ATOM_MAX: u32 = u16::MAX as u32 - 1;

impl StringImpl {
    /// convert string to atom. if string is not registered as atom yet, do registration.
    pub(crate) fn to_atom(&self, ctx: &mut ScopedContext, kind: AtomKind) -> Result<Atom, i32> {
        if let Some(a) = self.atom_info() {
            // str already registered as atom
            Ok(Atom::from_id(a.id()))
        } else {
            // register as atom
            Atom::from_string(ctx, self.gc_ref().into(), kind)
        }
    }
}

impl<'c> ScopedContext<'c> {
    /// create or reuse an Atom from `str`
    pub fn create_atom_string(&mut self, str: &str) -> Result<StringRef, i32> {
        let atom = Atom::from_str(self, str)?;
        atom.force_string(self)
    }
}

impl JSValue {
    /// convert value to atom. if value is not string or symbol, corece value to string then convert to atom.
    ///
    /// Note: returned atom is cloned, caller should free it after use.
    pub fn to_atom(&self, ctx: &mut ScopedContext) -> Result<Atom, i32> {
        let mut val = *self;

        loop {
            match val {
                JSValue::UNDEFINED => {
                    return Ok(Atom::CONST_undefined);
                }
                JSValue::NULL => {
                    return Ok(Atom::CONST_null);
                }
                JSValue::Bool(b) => {
                    return Ok(if b {
                        Atom::CONST_true
                    } else {
                        Atom::CONST_false
                    });
                }
                JSValue::Int(i) => {
                    return Atom::from_i64(ctx, i as i64);
                }
                JSValue::String(s) => {
                    return s.to_atom(ctx, AtomKind::String);
                }
                #[cfg(feature = "builtin_symbol")]
                JSValue::Symbol(s) => {
                    return s.to_atom(ctx, AtomKind::Symbol);
                }

                JSValue::Object(_) | JSValue::Float64(_) => {
                    val = self.to_string_or_symbol(ctx)?;
                    continue;
                }

                JSValue::UNINITIALIZED
                | JSValue::CatchOffset(_)
                | JSValue::Module(_)
                | JSValue::Bytecode(_) => {
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

// /// return a string atom containing name concatenated with str1.
// /// Note: returned atom should be freed by caller.
// fn js_atom_concat_str_v0(ctx: &mut ContextImpl, name: Atom, str1: *const u8) -> Result<Atom, i32> {
//     let str_val = name.force_string(ctx)?;
//     let astr = str_val.content().to_utf8(false);
//     let len = astr.len();

//     let str = unsafe { CStr::from_ptr(str1.cast()) };
//     let len1 = str.count_bytes();
//     let ttl_len = len + len1 + 1;

//     let mut buf = StringBuf::with_capacity(ttl_len, str_val.is_wide()).map_err(|_| {
//         ctx.throw_out_of_memory();
//         -1
//     })?;

//     buf.push_content(str_val.content()).unwrap();
//     buf.push_chars_iter(str.to_string_lossy().chars().map(CharCode::Char))
//         .map_err(|_| {
//             ctx.throw_out_of_memory();
//             -1
//         })?;
//     let s = StringRef::new(ctx.rt(), StringData::from_buf(buf)).map_err(|_| {
//         ctx.throw_out_of_memory();
//         -1
//     })?;
//     Atom::from_string(ctx.rt(), s, AtomKind::String)

//     // // FIXME: use Vec::try_reserve_exact()
//     // if let Some(p) = js_malloc_typed::<u8>(ctx, ttl_len, false) {
//     //     let mut new_str = Vec::from_raw_parts(p.as_ptr(), ttl_len, ttl_len);
//     //     std::ptr::copy_nonoverlapping(astr.as_ptr().cast(), new_str.as_mut_ptr(), len);
//     //     std::ptr::copy_nonoverlapping(str1, new_str.as_mut_ptr().offset(len as isize), len1);

//     //     Atom::new(ctx.rt_ref(), std::str::from_utf8_unchecked(&new_str)).unwrap_or(Atom::NULL)
//     // } else {
//     //     Atom::NULL
//     // }
// }

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
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

    buf.push_str(&astr);
    buf.push_str(str1);

    let s = ctx.create_string_from_utf8(buf.into_bytes())?;
    Atom::from_string(ctx, s, AtomKind::String)
}

/// Note: returned atom should be freed by caller.
#[inline(always)]
pub fn js_atom_concat_num(ctx: &mut ScopedContext, atom: Atom, n: u32) -> Result<Atom, i32> {
    js_atom_concat_str(ctx, atom, &format!("{n}"))
}

#[inline(always)]
pub(crate) const fn is_const_atom(id: u32) -> bool {
    // Hint: for u32 atoms, `self.0 as i32` could be minus,
    // in which case, suggests a small int atom.
    (id as i32) <= ATOM_CONST_COUNT as i32
}

#[inline(always)]
pub(crate) const fn is_small_int_atom(id: u32) -> bool {
    (id & JS_ATOM_TAG_INT) != 0
}
