use std::{borrow::Cow, ffi::CStr, hash::Hash, num::NonZeroU16, rc::Rc};

use crate::{
    context::JSContext,
    error::JSResult,
    mem::js_malloc_typed,
    regexp::is_digit,
    runtime::JSRuntime,
    string::{JSString, JSStringInner, StringContent},
    value::JSValue,
};

use super::consts::*;

/// atom descriptor kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum AtomDescKind {
    JS_ATOM_KIND_STRING,
    JS_ATOM_KIND_SYMBOL,
    JS_ATOM_KIND_PRIVATE,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum AtomTypeHint {
    String = 1,
    GlobalSymbol = 2,
    Symbol = 3,
    Private = 4,
}

/// atom id and type
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum AtomHintAndId {
    String(NonZeroU16),
    GlobalSymbol(NonZeroU16),
    Symbol(NonZeroU16),
    Private(NonZeroU16),
}

impl AtomHintAndId {
    /// Get atom type hint
    pub fn type_hint(self) -> AtomTypeHint {
        match self {
            Self::String(_) => AtomTypeHint::String,
            Self::Symbol(_) => AtomTypeHint::Symbol,
            Self::GlobalSymbol(_) => AtomTypeHint::GlobalSymbol,
            Self::Private(_) => AtomTypeHint::Private,
        }
    }

    /// Get atom id.
    pub fn id(self) -> u32 {
        match self {
            Self::String(a) | Self::Symbol(a) | Self::GlobalSymbol(a) | Self::Private(a) => {
                u16::from(a) as u32
            }
        }
    }

    /// Get atom.
    /// Returned atom is not duplicated, caller should clone to keep it referenced.
    #[inline(always)]
    pub fn atom(self) -> JSAtom {
        JSAtom(self.id())
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Hash)]
pub struct JSAtom(pub(crate) u32);

impl JSAtom {
    /// create atom for u32 integer
    #[inline(always)]
    pub fn new_u32(n: u32) -> Self {
        Self(n | JS_ATOM_TAG_INT)
    }

    /// check if is const atom?
    #[inline(always)]
    pub fn is_const(self) -> bool {
        // Hint: for u32 atoms, `self.0 as i32` should be minus.
        #[cfg(feature = "dump_leaks")]
        {
            self.0 as i32 <= 0
        }
        #[cfg(not(feature = "dump_leaks"))]
        {
            (self.0 as i32) < JS_ATOM_INIT_COUNT as i32
        }
    }

    /// test if the atom is int type
    #[inline(always)]
    pub fn is_int_type(self) -> bool {
        (self.0 & JS_ATOM_TAG_INT) != 0
    }

    /// get atom descriptor kind
    pub fn kind(self, rt: &JSRuntime) -> AtomDescKind {
        if self.is_int_type() {
            return AtomDescKind::JS_ATOM_KIND_STRING;
        }

        let p = rt.get_atom_desc(self).expect("INVALID ATOM");
        match p.atom_info().unwrap() {
            AtomHintAndId::String(_) => AtomDescKind::JS_ATOM_KIND_STRING,
            AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_) => {
                AtomDescKind::JS_ATOM_KIND_SYMBOL
            }
            AtomHintAndId::Private(_) => AtomDescKind::JS_ATOM_KIND_PRIVATE,
        }
    }

    /// test if atom has non-null string description
    pub fn has_description(self, ctx: &JSContext) -> bool {
        if self.is_int_type() {
            return false;
        }

        ctx.rt()
            .get_atom_desc(self)
            .map(|p| {
                let ai = p.atom_info().unwrap();
                !p.is_null()
                    && ((matches!(ai, AtomHintAndId::Symbol(_)) && p.hash() == JS_ATOM_HASH_SYMBOL)
                        || matches!(ai, AtomHintAndId::GlobalSymbol(_)))
            })
            .unwrap()
    }

    /// increase atom description ref count.
    #[inline]
    pub fn dup(self, rt: &JSRuntime) -> JSAtom {
        if !self.is_const() {
            let p = rt
                .get_atom_desc(self)
                .expect(&format!("INVALID ATOM {}", self.0));

            // leak JSStringInner: duplicated.
            let _ = Rc::into_raw(p.0);
        }

        self
    }

    /// get atom description ref count, for const atom, returns None
    #[cfg(debug_assertions)]
    pub fn ref_count(self, rt: &JSRuntime) -> Option<usize> {
        if !self.is_const() {
            Some(Rc::strong_count(&rt.get_atom_desc(self).unwrap().0))
        } else {
            None
        }
    }

    /// Convert atom to String or Symbol value.
    /// Note: atom is not consumed.
    pub fn to_string_or_symbol(self, ctx: &JSContext, force_string: bool) -> Result<JSValue, i32> {
        if self.is_int_type() {
            ctx.create_string_from_utf8(format!("{}", __JS_AtomToUInt32(self)).into_bytes())
                .map(JSValue::String)
        } else {
            let ss = ctx.rt().get_atom_desc(self).unwrap();
            Ok(if force_string {
                ss.into()
            } else if let AtomHintAndId::String(_) = ss.atom_info().unwrap() {
                ss.into()
            } else {
                JSValue::Symbol(ss)
            })
        }
    }

    /// Convert atom to JSString, a short-cut to `Self::to_string_or_symbol(s, true)`.
    /// Note: atom is not consumed.
    pub fn to_string(self, ctx: &JSContext) -> Result<JSString, i32> {
        match self.to_string_or_symbol(ctx, true)? {
            JSValue::String(s) | JSValue::Symbol(s) => Ok(s),
            _ => unreachable!(),
        }
    }

    /// if the atom can be used as an array index (in 0..u32::MAX range), returns the index.
    pub fn to_array_index(self, rt: &JSRuntime) -> Option<usize> {
        if self == JS_ATOM_NULL {
            None
        } else if self.is_int_type() {
            Some(__JS_AtomToUInt32(self) as usize)
        } else {
            if let Some(p) = rt.get_atom_desc(self) {
                if let Some(AtomHintAndId::String(_)) = p.atom_info() {
                    if let Ok(n) = p.content().parse_u32() {
                        if n != u32::MAX {
                            return Some(n as usize);
                        }
                    }
                }
            }
            None
        }
    }

    /// run closure with utf8 bytes converted from atom
    pub fn with_utf8<R>(self, rt: &JSRuntime, f: impl FnOnce(Cow<'_, str>) -> R) -> R {
        if self.eq(&JS_ATOM_NULL) {
            f(Cow::Borrowed("<null>"))
        } else if self.is_int_type() {
            f(Cow::Owned(format!("{}", __JS_AtomToUInt32(self))))
        } else {
            f(rt.get_atom_desc(self)
                .expect("INVALID ATOM")
                .content()
                .to_str())
        }
    }

    /// convert atom to utf8 encoded String
    #[inline]
    pub fn utf8(self, rt: &JSRuntime) -> String {
        self.with_utf8(rt, |s| s.into_owned())
    }

    /// convert atom to utf8 Cow str
    #[deprecated(note = "use JSAtom::utf8() or JSAtom::with_utf8() instead")]
    pub fn to_str(self, rt: &JSRuntime) -> Cow<'_, str> {
        if self.is_int_type() {
            Cow::Owned(format!("{}", __JS_AtomToUInt32(self)))
        } else {
            if self.eq(&JS_ATOM_NULL) {
                Cow::Borrowed("<null>")
            } else {
                rt.get_atom_desc(self)
                    .expect("INVALID ATOM")
                    .content()
                    .to_str()
                    .into_owned()
                    .into()
            }
        }
    }

    /// convert atom to utf8 owned string
    #[deprecated(note = "may equivelent to `JSAtom::utf8()`")]
    pub fn to_utf8_string(&self, ctx: &JSContext) -> Result<String, i32> {
        // XXX: same result as following?
        Ok(self.utf8(ctx.rt()))

        // XXX: same result as following?
        // self.to_value(ctx, true)
        //     .check_exception()?
        //     .coerce_to_utf8(ctx)
        //     .map(|x| x.to_string())
    }
}

pub const JS_ATOM_NULL: JSAtom = JSAtom(0);

pub const JS_ATOM_HASH_SYMBOL: u32 = 0;
pub const JS_ATOM_HASH_PRIVATE: u32 = 1;
pub const JS_ATOM_HASH_MASK: u32 = (1 << 30) - 1;

pub const JS_ATOM_TAG_INT: u32 = 1 << 31;
pub const JS_ATOM_MAX_INT: u32 = JS_ATOM_TAG_INT - 1;
pub const JS_ATOM_MAX: u32 = u16::MAX as u32 - 1;

pub const ATOM_GET_STR_BUF_SIZE: usize = 64;

#[inline(always)]
pub fn __JS_AtomToUInt32(atom: JSAtom) -> u32 {
    atom.0 & !JS_ATOM_TAG_INT
}

impl JSContext {
    /// create new or reuse existing atom for `str`.
    pub fn create_atom(&self, str: &str) -> Result<JSAtom, i32> {
        // format [xxxx]
        if str.starts_with('[') {
            let name2 = &str[1..str.len() - 1]; // chop heading '[', tailing ']'
            let rt = self.rt_mut();

            // We assume 8 bit non null strings, which is the case for these symbols
            for i in JS_ATOM_Symbol_toPrimitive.0..JS_ATOM_INIT_COUNT {
                if let Some(astr) = rt
                    .atom_slots
                    .get(i as usize)
                    .and_then(|p| unsafe { p.as_ref() })
                {
                    if astr.content().eq_str(name2) {
                        return Ok(JSAtom(i).dup(rt));
                    }
                }
            }

            self.throw_internal_error(&format!("unknown atom '{str}'"));
            return Err(-1);
        }

        if str.is_empty() || !is_digit(str.as_bytes()[0]) {
            if let Some(atom) = __JS_FindAtom(&self.rt(), str, AtomTypeHint::String) {
                return Ok(atom);
            }
        }

        let s = self.create_string(str)?;
        self.rt_mut()
            .register_atom_jsstring(s, AtomTypeHint::String)
    }

    pub fn create_atom_string(&self, str: &str) -> Result<JSString, i32> {
        let atom = self.create_atom(str)?;
        let val = atom.to_string(self);
        JS_FreeAtom(self, atom);
        val
    }
}

impl JSString {
    /// convert string to atom
    pub fn to_atom(&self, ctx: &JSContext) -> Result<JSAtom, i32> {
        if let Some(a) = self.atom_info() {
            // str alreay registered as atom
            Ok(a.atom())
        } else {
            // register str as atom
            ctx.rt_mut()
                .register_atom_jsstring(self.clone(), AtomTypeHint::String)
        }
    }
}

impl JSValue {
    /// coerce value to atom.
    /// returned atom is cloned, caller should free it after use.
    pub fn coerce_to_atom(&self, ctx: &JSContext) -> Result<JSAtom, i32> {
        match self {
            JSValue::Int(i) if *i as u32 <= JS_ATOM_MAX_INT => Ok(JSAtom::new_u32(*i as u32)),
            JSValue::Bool(b) => Ok(if *b { JS_ATOM_true } else { JS_ATOM_false }),
            JSValue::Symbol(s) => Ok(s.atom_info().unwrap().atom()),
            JSValue::String(s) => s.to_atom(ctx),
            JSValue::Object(_) | JSValue::Float64(_) | JSValue::Int(_) => {
                match self.coerce_to_propkey(ctx)? {
                    Self::Symbol(s) => Ok(s.atom_info().unwrap().atom()),
                    Self::String(s) => {
                        if let Some(a) = s.atom_info() {
                            // str alreay registered as atom
                            Ok(a.atom())
                        } else {
                            // register str as atom
                            ctx.rt_mut().register_atom_jsstring(s, AtomTypeHint::String)
                        }
                    }
                    _ => unreachable!(),
                }
            }

            JSValue::UNDEFINED => Ok(JS_ATOM_undefined),
            JSValue::NULL => Ok(JS_ATOM_null),

            JSValue::UNINITIALIZED
            | JSValue::CatchOffset(_)
            | JSValue::Module(_)
            | JSValue::Bytecode(_)
            | JSValue::AsyncStateResume(_) => {
                ctx.throw_type_error(&format!("cannot convert {} to atom", self));
                Err(-1)
            }
            JSValue::EXCEPTION => Err(-1),
        }
    }
}

/// find atom by str
pub fn __JS_FindAtom(rt: &JSRuntime, str: &str, atom_type: AtomTypeHint) -> Option<JSAtom> {
    let hash = StringContent::calc_hash_for_str(str, atom_type);

    if let Some(ids) = rt.atom_hash_lut.get(&hash) {
        for &aid in ids {
            let id = u16::from(aid) as u32;
            let p = rt.atom_slots[id as usize];
            debug_assert!(!p.is_null());

            if unsafe {
                (*p).atom_info().unwrap().type_hint() == atom_type && (*p).content().eq_str(str)
            } {
                let a = JSAtom(id);

                if !a.is_const() {
                    #[cfg(debug_assertions)]
                    let _refcnt = a.ref_count(rt);

                    // atom will be pass out, dup atom description then leak to keep reference.
                    unsafe {
                        let d = Rc::from_raw(p);
                        let d2 = d.clone();
                        let _ = Rc::into_raw(d);
                        let _ = Rc::into_raw(d2);
                    }

                    #[cfg(debug_assertions)]
                    debug_assert_eq!(a.ref_count(rt).unwrap(), _refcnt.unwrap() + 1);
                }

                return Some(a);
            }
        }
    }

    None
}

#[deprecated(note = "use JSContext::create_atom() instead")]
pub unsafe fn find_atom(ctx: &JSContext, name: &str) -> JSAtom {
    ctx.create_atom(name).unwrap_or(JS_ATOM_NULL)
}

unsafe fn __JS_FreeAtom(rt: *mut JSRuntime, i: JSAtom) {
    if let Some(p) = (*rt).get_atom_desc(i) {
        if Rc::strong_count(&p.0) > 2 {
            drop(p);
        } else {
            // drop atom description
            drop(p);
            // now atom description has ref count 1
            JS_FreeAtomStruct(rt, i);
        }
    }
}

pub fn JS_FreeAtom(ctx: &JSContext, v: JSAtom) {
    if !v.is_const() {
        unsafe {
            __JS_FreeAtom(ctx.rt_mut(), v);
        }
    }
}

pub fn JS_FreeAtomRT(rt: *mut JSRuntime, v: JSAtom) {
    if !v.is_const() {
        unsafe {
            __JS_FreeAtom(rt, v);
        }
    }
}

pub unsafe fn JS_FreeAtomStruct(rt: *mut JSRuntime, atom: JSAtom) {
    let aid = atom.0;
    debug_assert_ne!(aid, 0);

    let aptr = unsafe {
        std::ptr::replace(
            &mut (*rt).atom_slots[aid as usize],
            std::ptr::null::<JSStringInner>(),
        )
    };
    let adesc = Rc::from_raw(aptr);
    debug_assert_eq!(Rc::strong_count(&adesc), 1);

    let hash = adesc.hash();
    let mut ids = (*rt).atom_hash_lut.remove(&hash).unwrap();
    debug_assert!(!ids.is_empty());

    if ids.len() == 1 {
        debug_assert_eq!(u16::from(ids[0]), aid as u16);
    } else {
        let i = ids
            .iter()
            .position(|&x| u16::from(x) == aid as u16)
            .unwrap();
        ids.swap_remove(i);
        (*rt).atom_hash_lut.insert(hash, ids);
    }
}

/// test if the `atom` can be used as an array index (in 0..u32::MAX range).
#[deprecated(note = "use JSAtom::to_array_index() instead")]
pub fn JS_AtomIsArrayIndex(ctx: &JSContext, pval: *mut u32, atom: JSAtom) -> bool {
    if atom.is_int_type() {
        unsafe {
            *pval = __JS_AtomToUInt32(atom);
        }
        true
    } else {
        if let Some(p) = ctx.rt().get_atom_desc(atom) {
            if let Some(AtomHintAndId::String(_)) = p.atom_info() {
                if let Ok(n) = p.content().parse_u32() {
                    if n != u32::MAX {
                        unsafe {
                            *pval = n;
                        }
                        return true;
                    }
                }
            }
        }

        unsafe {
            *pval = 0;
        }
        false
    }
}

// This test must be fast if atom is not a numeric index (e.g. a
// method name). Return JS_UNDEFINED if not a numeric
// index. JS_EXCEPTION can also be returned.
pub fn JS_AtomIsNumericIndex1(ctx: &JSContext, atom: JSAtom) -> JSValue {
    if atom.is_int_type() {
        return JSValue::Int(__JS_AtomToUInt32(atom) as _);
    }

    let p = ctx.rt().get_atom_desc(atom).expect("INVLID ATOM");
    if p.is_empty_or_null() {
        return JSValue::UNDEFINED;
    }

    if let AtomHintAndId::String(_) = p.atom_info().unwrap() {
        let len = p.len();

        // check for: -0, Infinity, -Infinity
        let mut cc = p.content().chars();
        if let Some(mut c) = cc.next() {
            if c == '-' {
                if let Some(c2) = cc.next() {
                    // -0 case is specific
                    if c2 == '0' && len == 2 {
                        return JSValue::from_f64_as_is(-0.0);
                    } else {
                        c = c2;
                    }
                } else {
                    return JSValue::UNDEFINED;
                }
            }

            if !c.is_digit(10) {
                // XXX: should test NaN, but the tests do not check it
                if c == 'I' {
                    let mut nfinity = "nfinity".chars();
                    loop {
                        match (cc.next(), nfinity.next()) {
                            (Some(a), Some(b)) if a == b => {}
                            (None, None) => {
                                break;
                            }
                            _ => {
                                return JSValue::UNDEFINED;
                            }
                        }
                    }
                } else {
                    return JSValue::UNDEFINED;
                }
            }
        }

        // XXX: bignum: would be better to only accept integer to avoid
        // relying on current floating point precision
        // this is ECMA CanonicalNumericIndexString primitive
        if let Ok(num) = JSValue::String(p.clone()).coerce_to_number(ctx) {
            // test num as string is same to original value
            if let Ok(str) = num.coerce_to_string(ctx) {
                if p.content().eq(str.content()) {
                    num
                } else {
                    JSValue::EXCEPTION
                }
            } else {
                return JSValue::EXCEPTION;
            }
        } else {
            return JSValue::EXCEPTION;
        }
    } else {
        JSValue::UNDEFINED
    }
}

// return -1 if exception or TRUE/FALSE
pub unsafe fn JS_AtomIsNumericIndex(ctx: &JSContext, atom: JSAtom) -> i32 {
    let num = JS_AtomIsNumericIndex1(ctx, atom);
    if num.is_undefined() {
        return 0;
    }
    if num.is_exception() {
        return -1;
    }
    return 1;
}

#[deprecated(note = "use JSRuntime::init_default_atoms() instead")]
pub unsafe fn JS_InitAtoms(rt: &mut JSRuntime) -> Result<(), i32> {
    (*rt).init_default_atoms()
}

// return a string atom containing name concatenated with str1
pub unsafe fn js_atom_concat_str(ctx: &JSContext, name: JSAtom, str1: *const u8) -> JSAtom {
    let str_val = if let Ok(s) = name.to_string(ctx) {
        s
    } else {
        return JS_ATOM_NULL;
    };
    let astr = str_val.content().to_str();
    let len = astr.len();
    let len1 = CStr::from_ptr(str1.cast()).count_bytes();
    let ttl_len = len + len1 + 1;

    // FIXME: use Vec::try_reserve_exact()
    let res = if let Some(p) = js_malloc_typed::<u8>(ctx, ttl_len, false) {
        let mut new_str = Vec::from_raw_parts(p.as_ptr(), ttl_len, ttl_len);
        std::ptr::copy_nonoverlapping(astr.as_ptr().cast(), new_str.as_mut_ptr(), len);
        std::ptr::copy_nonoverlapping(str1, new_str.as_mut_ptr().offset(len as isize), len1);

        ctx.create_atom(std::str::from_utf8_unchecked(&new_str))
            .unwrap_or(JS_ATOM_NULL)
    } else {
        JS_ATOM_NULL
    };

    res
}

pub unsafe fn js_atom_concat_num(ctx: &JSContext, name: JSAtom, n: u32) -> JSAtom {
    js_atom_concat_str(ctx, name, format!("{n}\0").as_ptr())
}

pub unsafe fn JS_DumpAtoms(rt: &JSRuntime) {
    // This only dumps hashed atoms, not JS_ATOM_TYPE_SYMBOL atoms
    // println!(
    //     "JSAtom count={} size={} hash_size={}:",
    //     (*rt).atom_count,
    //     (*rt).atom_size,
    //     (*rt).atom_hash_size
    // );

    // println!("JSAtom hash table: {{");
    // for i in 0..(*rt).atom_hash_size {
    //     let mut h = *(*rt).atom_hash.offset(i as isize);
    //     if h != 0 {
    //         print!("  {i}:");
    //         while h != 0 {
    //             let p = *(*rt).atom_array.offset(h as isize);
    //             print!(" ");
    //             JS_DumpString(rt, p);
    //             h = (*p).hash_next;
    //         }
    //         println!();
    //     }
    // }
    // println!("}}");

    println!("JSAtom table (non-const): {{");
    for (i, astr) in (*rt).atom_slots[JS_ATOM_INIT_COUNT as usize..]
        .iter()
        .enumerate()
    {
        if let Some(p) = astr.as_ref() {
            print!(
                "  {}: {{ {} {:08x} ",
                i + JS_ATOM_INIT_COUNT as usize,
                p.atom_info().unwrap().type_hint() as u8,
                p.content().calc_hash(AtomTypeHint::String)
            );
            p.content().dump();
            // JS_DumpString(rt, p);
            println!(" }}");
        }
    }

    println!("}}");
}
