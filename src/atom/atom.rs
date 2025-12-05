use std::borrow::Cow;

use gc_lite::GcTracable;

use crate::{
    atom::{ATOM_CONST_COUNT, AtomDescKind, AtomInfo, AtomKind},
    context::ContextImpl,
    number::IntOrFloat,
    runtime::RuntimePtr,
    string::{StringChar, StringRef},
    value::JSValue,
};

use super::AtomRegistry;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord)]
pub struct Atom(pub(super) u32);

unsafe impl GcTracable for Atom {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        if !self.is_const() && !self.is_null() && !self.is_small_int() {
            AtomRegistry::with_desc(self.0, |d| {
                d.string().trace(tracer);
            });
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
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// test if the atom is small int type
    #[inline(always)]
    pub const fn is_small_int(&self) -> bool {
        is_small_int_atom(self.0)
    }

    #[inline(always)]
    pub const fn id(&self) -> u32 {
        self.0
    }

    /// check if atom is const atom?
    #[inline(always)]
    pub const fn is_const(&self) -> bool {
        is_const_atom(self.0)
    }

    /// get atom info
    pub(crate) fn atom_info(&self) -> AtomInfo {
        AtomRegistry::with_string(self.0, |s| s.atom_info().cloned().unwrap()).unwrap()
    }

    /// get atom description string
    #[inline(always)]
    pub(crate) fn string(&self) -> StringRef {
        AtomRegistry::with_string(self.0, |s| *s).unwrap()
    }

    /// test if atom has normal string description
    pub(crate) fn has_string_desc(&self) -> bool {
        if self.is_small_int() {
            false
        } else {
            let s = self.string();
            !s.content().is_ident() && {
                let a = s.atom_info().unwrap();
                let k = a.kind();
                (k == AtomKind::Symbol && a.hash() == JS_ATOM_HASH_SYMBOL)
                    || k == AtomKind::GlobalSymbol
            }
        }
    }

    /// check atom is still registered, desc is still alive in gc.
    #[cfg(debug_assertions)]
    pub(crate) fn is_alive(&self) -> bool {
        if !self.is_const() && !self.is_null() && !self.is_small_int() {
            AtomRegistry::with_string(self.0, |_| true).unwrap_or(false)
        } else {
            true
        }
    }

    pub fn set_gc_root(&self, b: bool) {
        if !self.is_const() && !self.is_null() && !self.is_small_int() {
            let s = self.string();
            s.rt().unwrap().gc_heap_mut().set_root(s.gc_ref(), b);
        }
    }

    /// get atom descriptor kind
    pub(crate) fn kind(&self) -> AtomDescKind {
        if self.is_small_int() {
            AtomDescKind::JS_ATOM_KIND_STRING
        } else {
            match self.atom_info().kind() {
                AtomKind::String => AtomDescKind::JS_ATOM_KIND_STRING,
                AtomKind::Symbol | AtomKind::GlobalSymbol => AtomDescKind::JS_ATOM_KIND_SYMBOL,
                AtomKind::Private => AtomDescKind::JS_ATOM_KIND_PRIVATE,
            }
        }
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
    pub fn to_numeric_index(&self, ctx: &ContextImpl) -> Result<Option<IntOrFloat>, i32> {
        if let Some(n) = self.to_small_int() {
            return Ok(Some(IntOrFloat::Int64(n as _)));
        }

        let s = self.string();
        let atom_info = s.atom_info().unwrap();
        let content = s.content();

        if content.is_empty() || content.is_ident() {
            return Ok(None);
        }

        if let AtomKind::String = atom_info.kind() {
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

            if !self.is_const() {
                // XXX: bignum: would be better to only accept integer to avoid
                // relying on current floating point precision
                // this is ECMA CanonicalNumericIndexString primitive
                let num = s.to_number(ctx);

                // test num as string is same to original value
                let str = num.to_string(ctx)?;
                if content.eq(str.content()) {
                    return match num {
                        JSValue::Int(i) => Ok(Some(IntOrFloat::Int64(i as _))),
                        JSValue::Float64(d) => Ok(Some(IntOrFloat::Float64(d))),
                        _ => unreachable!(),
                    };
                }
            }
        }

        Ok(None)
    }

    pub fn is_numeric_index(&self, ctx: &ContextImpl) -> Result<bool, i32> {
        self.to_numeric_index(ctx).map(|x| x.is_some())
    }

    /// Convert atom to String or Symbol value.
    pub fn string_or_symbol(&self, ctx: &ContextImpl) -> Result<JSValue, i32> {
        if let Some(n) = self.to_small_int() {
            ctx.create_string_from_utf8(format!("{n}",).into_bytes())
                .map(JSValue::String)
        } else {
            let s = self.string();
            let atom_info = s.atom_info().unwrap();

            match atom_info.kind() {
                AtomKind::String | AtomKind::Private => Ok(JSValue::String(s)),
                AtomKind::Symbol | AtomKind::GlobalSymbol => Ok(JSValue::Symbol(*self)),
            }
        }
    }

    /// Force convert atom to String.
    /// If atom is Symbol kind, returns its string descriptor.
    pub fn force_string(&self, ctx: &ContextImpl) -> Result<StringRef, i32> {
        match self.string_or_symbol(ctx)? {
            JSValue::String(s) => {
                debug_assert!(!s.content().is_ident());
                Ok(s)
            }
            JSValue::Symbol(a) => {
                // force symbol to string
                let s = a.string();
                if s.content().is_ident() {
                    Self::CONST_empty_string.force_string(ctx)
                } else {
                    Ok(s)
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
            let s = self.string();
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
    pub fn to_i64(&self) -> Option<i64> {
        if self.0 == 0 {
            None
        } else if let Some(n) = self.to_small_int() {
            Some(n as i64)
        } else {
            let s = self.string();
            if let AtomKind::String = s.atom_info().unwrap().kind() {
                return s.content().parse_i64();
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
            f(self.string().content().to_utf8(false))
        }
    }

    /// convert atom to utf8 encoded String
    #[inline(always)]
    pub fn to_utf8(&self) -> String {
        self.with_utf8(|s| s.into_owned())
    }
}

pub const JS_ATOM_HASH_SYMBOL: u32 = 0;
pub const JS_ATOM_HASH_PRIVATE: u32 = 1;
pub const JS_ATOM_HASH_MASK: u32 = (1 << 30) - 1;

pub const JS_ATOM_TAG_INT: u32 = 1 << 31;
pub const JS_ATOM_MAX_INT: u32 = JS_ATOM_TAG_INT - 1;
pub const JS_ATOM_MAX: u32 = u16::MAX as u32 - 1;

pub const ATOM_GET_STR_BUF_SIZE: usize = 64;

impl StringRef {
    /// convert string to atom. if string is not registered as atom yet, do registration.
    pub(crate) fn to_atom(self, rt: RuntimePtr) -> Result<Atom, i32> {
        if let Some(a) = self.atom_info()
            && self.rt().is_none_or(|x| x == rt)
        {
            // str already registered as atom
            Ok(Atom::from_id(a.id()))
        } else {
            // register as string atom
            Atom::from_string(rt, self, AtomKind::String)
        }
    }
}

impl ContextImpl {
    /// create or reuse an Atom from `str`
    pub fn create_atom_string(&self, str: &str) -> Result<StringRef, i32> {
        let atom = Atom::new(self.rt(), str)?;
        atom.force_string(self)
    }
}

impl JSValue {
    /// convert value to atom. if value is not string or symbol, corece value to string then convert to atom.
    ///
    /// Note: returned atom is cloned, caller should free it after use.
    pub fn to_atom(&self, ctx: &ContextImpl) -> Result<Atom, i32> {
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
                    return Atom::from_i64(ctx.rt(), i as i64);
                }
                JSValue::String(s) => {
                    return s.to_atom(ctx.rt());
                }
                JSValue::Symbol(a) => {
                    return Ok(a);
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
// fn js_atom_concat_str_v0(ctx: &ContextImpl, name: Atom, str1: *const u8) -> Result<Atom, i32> {
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
//     buf.push_chars_iter(str.to_string_lossy().chars().map(StringChar::Char))
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
pub(crate) fn js_atom_concat_str(ctx: &ContextImpl, atom: Atom, str1: &str) -> Result<Atom, i32> {
    let str_val = atom.force_string(ctx)?;
    let astr = str_val.content().to_utf8(false);

    let mut buf = String::new();
    buf.try_reserve_exact(astr.len() + str1.len())
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

    buf.push_str(&astr);
    buf.push_str(str1);

    Atom::new(ctx.rt(), &buf)
}

/// Note: returned atom should be freed by caller.
#[inline(always)]
pub fn js_atom_concat_num(ctx: &ContextImpl, atom: Atom, n: u32) -> Result<Atom, i32> {
    js_atom_concat_str(ctx, atom, &format!("{n}"))
}

#[inline(always)]
pub(crate) const fn is_const_atom(id: u32) -> bool {
    // Hint: for u32 atoms, `self.0 as i32` should be minus.
    (id as i32) < ATOM_CONST_COUNT as i32
}

#[inline(always)]
pub(crate) const fn is_small_int_atom(id: u32) -> bool {
    (id & JS_ATOM_TAG_INT) != 0
}
