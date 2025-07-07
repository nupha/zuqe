use std::{borrow::Cow, ffi::CStr, hash::Hash, num::NonZeroU16, rc::Rc};

use crate::{
    context::JSContext,
    error::JSResult,
    mem::js_malloc_typed,
    regexp::is_digit,
    runtime::JSRuntime,
    string::{JSStringInner, StringContent},
    value::JSValue,
};

use super::{JSAtomStruct, consts::*};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum JSAtomKindEnum {
    JS_ATOM_KIND_STRING,
    JS_ATOM_KIND_SYMBOL,
    JS_ATOM_KIND_PRIVATE,
}

// TODO: merge this with JSAtomKindEnum
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum JSAtomTypeHint {
    None = 0,
    String = 1,
    GlobalSymbol = 2,
    Symbol = 3,
    Private = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub enum JSAtomInfo {
    String(NonZeroU16),
    GlobalSymbol(NonZeroU16),
    Symbol(NonZeroU16),
    Private(NonZeroU16),
}

impl JSAtomInfo {
    /// Get atom type hint
    pub fn hint(self) -> JSAtomTypeHint {
        match self {
            Self::String(_) => JSAtomTypeHint::String,
            Self::Symbol(_) => JSAtomTypeHint::Symbol,
            Self::GlobalSymbol(_) => JSAtomTypeHint::GlobalSymbol,
            Self::Private(_) => JSAtomTypeHint::Private,
        }
    }

    /// Get atom
    pub fn atom(self) -> JSAtom {
        match self {
            Self::String(a) | Self::Symbol(a) | Self::GlobalSymbol(a) | Self::Private(a) => {
                JSAtom(u16::from(a) as u32)
            }
        }
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

    #[inline(always)]
    pub fn is_const(self) -> bool {
        // NOTE: for u32 atoms, `v as i32` should be minus.

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

    /// get atom kind
    pub fn kind(self, rt: &JSRuntime) -> JSAtomKindEnum {
        if self.is_int_type() {
            return JSAtomKindEnum::JS_ATOM_KIND_STRING;
        }

        let p = rt.get_atom_desc(self).expect("INVALID ATOM");
        match p.atom_info().unwrap() {
            JSAtomInfo::String(_) => JSAtomKindEnum::JS_ATOM_KIND_STRING,
            JSAtomInfo::Symbol(_) | JSAtomInfo::GlobalSymbol(_) => {
                JSAtomKindEnum::JS_ATOM_KIND_SYMBOL
            }
            JSAtomInfo::Private(_) => JSAtomKindEnum::JS_ATOM_KIND_PRIVATE,
        }
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

    /// convert atom to str
    pub fn to_str(self, rt: &JSRuntime) -> Cow<'_, str> {
        if self.is_int_type() {
            Cow::Owned(format!("{}", __JS_AtomToUInt32(self)))
        } else {
            if self.eq(&JS_ATOM_NULL) {
                Cow::Borrowed("<null>")
            } else {
                let str = rt.get_atom_desc(self).expect("INVALID ATOM");
                let s = str.content().to_str();
                s.into_owned().into()
            }
        }
    }

    /// Convert atom to JSValue, returns EXCEPTION on failure.
    /// Note: atom is not consumed.
    pub fn to_value(self, ctx: &JSContext, force_string: bool) -> JSValue {
        if self.is_int_type() {
            ctx.create_string_from_utf8(format!("{}", __JS_AtomToUInt32(self)).into_bytes())
        } else {
            let ss = ctx.rt().get_atom_desc(self).expect("INVALID ATOM").clone();
            if let JSAtomInfo::String(_) = ss.atom_info().unwrap() {
                ss.into()
            } else if force_string {
                ss.into()
            } else {
                JSValue::Symbol(ss)
            }
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
                if let Some(JSAtomInfo::String(_)) = p.atom_info() {
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

    /// convert atom to string
    pub fn to_utf8_string(&self, ctx: &JSContext) -> Result<String, i32> {
        let v = self.to_value(ctx, true).check_exception()?;
        v.coerce_to_utf8(ctx).map(|x| x.to_string())
    }
}

pub const JS_ATOM_NULL: JSAtom = JSAtom(0);

pub const JS_ATOM_HASH_SYMBOL: u32 = 0;
pub const JS_ATOM_HASH_PRIVATE: u32 = 1;
pub const JS_ATOM_HASH_MASK: u32 = (1 << 30) - 1;

pub const JS_ATOM_TAG_INT: u32 = 1 << 31;
pub const JS_ATOM_MAX_INT: u32 = JS_ATOM_TAG_INT - 1;
pub const JS_ATOM_MAX: u32 = u16::MAX as u32 - 1;

#[inline(always)]
fn JS_ATOM_COUNT_RESIZE(n: i32) -> i32 {
    n * 2
}

#[inline(always)]
fn atom_get_free(p: *const JSAtomStruct) -> usize {
    p as usize >> 1
}

#[inline(always)]
pub fn atom_set_free(v: usize) -> *mut JSAtomStruct {
    ((v << 1) | 1) as *mut JSAtomStruct
}

pub fn atom_is_free(p: *const JSAtomStruct) -> bool {
    (p as usize & 1) != 0
}

#[deprecated(note = "use JSAtom::is_const() instead")]
#[inline(always)]
pub fn __JS_AtomIsConst(v: JSAtom) -> bool {
    // NOTE: for u32 atoms, `v as i32` should be minus.

    #[cfg(feature = "dump_leaks")]
    {
        v.0 as i32 <= 0
    }
    #[cfg(not(feature = "dump_leaks"))]
    {
        (v.0 as i32) < JS_ATOM_INIT_COUNT as i32
    }
}

#[deprecated(note = "use JSAtom::is_int_type() instead")]
pub fn __JS_AtomIsTaggedInt(v: JSAtom) -> bool {
    v.is_int_type()
}

#[deprecated(note = "use JSAtom::from_u32() instead")]
pub fn __JS_AtomFromUInt32(n: u32) -> JSAtom {
    JSAtom::new_u32(n)
}

#[inline(always)]
pub fn __JS_AtomToUInt32(atom: JSAtom) -> u32 {
    atom.0 & !JS_ATOM_TAG_INT
}

#[deprecated(note = "use JSAtom::dup() instead")]
pub unsafe fn JS_DupAtomRT(rt: *mut JSRuntime, v: JSAtom) -> JSAtom {
    v.dup(rt.as_ref().unwrap())
}

#[deprecated(note = "use JSAtom::dup() instead")]
#[inline(always)]
pub unsafe fn JS_DupAtom(ctx: &JSContext, v: JSAtom) -> JSAtom {
    v.dup(ctx.rt())
}

#[deprecated(note = "use JSAtom::kind() instead")]
pub unsafe fn JS_AtomGetKind(ctx: &JSContext, v: JSAtom) -> JSAtomKindEnum {
    v.kind(ctx.rt())
}

pub const ATOM_GET_STR_BUF_SIZE: usize = 64;

/// debug helper: print atom, don't use in production code
#[deprecated(note = "use JSAtom::to_str() instead")]
pub unsafe fn print_atom(rt: &JSRuntime, atom: JSAtom) {
    print!("{}", atom.to_str(rt));
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

            self.JS_ThrowInternalError(&format!("unknown atom '{str}'"));
            return Err(-1);
        }

        if str.is_empty() || !is_digit(str.as_bytes()[0]) {
            if let Some(atom) = __JS_FindAtom(&self.rt(), str, JSAtomTypeHint::String) {
                return Ok(atom);
            }
        }

        let s = self.create_string(str).check_exception()?;
        self.rt_mut()
            .register_atom_jsstring(unsafe { s.into_string_unchecked() }, JSAtomTypeHint::String)
    }

    pub fn create_atom_string(&self, str: &str) -> JSResult {
        let atom = self.create_atom(str)?;
        let val = atom.to_value(self, true).check_exception();
        JS_FreeAtom(self, atom);
        val
    }
}

/// find atom by str
pub fn __JS_FindAtom(rt: &JSRuntime, str: &str, atom_type: JSAtomTypeHint) -> Option<JSAtom> {
    let hash = StringContent::calc_hash_for_str(str, atom_type);

    if let Some(ids) = rt.atom_hash_lut.get(&hash) {
        for &aid in ids {
            let id = u16::from(aid) as u32;
            let p = rt.atom_slots[id as usize];
            debug_assert!(!p.is_null());

            if unsafe {
                (*p).atom_info().unwrap().hint() == atom_type && (*p).content().eq_str(str)
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
            &mut (&mut (*rt).atom_slots)[aid as usize],
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
        let idx = ids
            .iter()
            .position(|&x| u16::from(x) == aid as u16)
            .unwrap();
        ids.swap_remove(idx);
        (*rt).atom_hash_lut.insert(hash, ids);
    }

    // #[cfg(feature = "dump_leaks")]
    // {
    //     debug_assert!(list_find(
    //         &mut p.as_mut().link,
    //         &mut (*rt).debug_string_list
    //     ));

    //     crate::types::list_del(&mut p.as_mut().link);
    // }
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
            if let Some(JSAtomInfo::String(_)) = p.atom_info() {
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

/// `val` is not consumed
pub fn JS_ValueToAtom(ctx: &JSContext, val: &JSValue) -> JSAtom {
    match val {
        JSValue::Int(i) if *i as u32 <= JS_ATOM_MAX_INT => JSAtom::new_u32(*i as u32),
        JSValue::Symbol(_) => js_symbol_to_atom(val).dup(ctx.rt()),
        _ => {
            if let Ok(tmp) = val.coerce_to_propkey(ctx) {
                if let JSValue::Symbol(_) = tmp {
                    js_symbol_to_atom(&tmp)
                } else {
                    ctx.rt_mut()
                        .register_atom_jsstring(
                            unsafe { tmp.into_string_unchecked() },
                            JSAtomTypeHint::String,
                        )
                        .unwrap_or(JS_ATOM_NULL)
                }
            } else {
                JS_ATOM_NULL
            }

            // let tmp = JS_ToPropertyKey(ctx, val);
            // if tmp.is_exception() {
            //     JS_ATOM_NULL
            // } else if JS_VALUE_GET_TAG(tmp) == JSTag::Symbol as i32 {
            //     js_symbol_to_atom(ctx, tmp)
            // } else {
            //     ctx
            //         .rt_mut()
            //         .add_atom_jsstring(
            //             Rc::from_raw(tmp.string()), // <-- `tmp` is consumed here
            //             JSAtomTypeHint::String,
            //         )
            //         .unwrap_or(JS_ATOM_NULL)
            // }
        }
    }
}

/// returned atom is not cloned. if caller want keep reference, should clone it.
pub fn js_symbol_to_atom(val: &JSValue) -> JSAtom {
    if let JSValue::Symbol(s) = val {
        match s.atom_info() {
            Some(JSAtomInfo::Symbol(a)) | Some(JSAtomInfo::GlobalSymbol(a)) => {
                JSAtom(u16::from(a) as u32)
            }
            _ => unreachable!(),
        }
    } else {
        unreachable!()
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
    if p.is_empty() {
        return JSValue::UNDEFINED;
    }

    if let JSAtomInfo::String(_) = p.atom_info().unwrap() {
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
        if let Ok(num) = JSValue::String(p.clone()).coerce_to_number_free(ctx) {
            // test num as string is same to original value
            if let Ok(JSValue::String(str)) = num.coerce_to_string(ctx) {
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
    let str_val = name.to_value(ctx, true);
    if str_val.is_exception() {
        return JS_ATOM_NULL;
    }

    let astr = if let Ok(s) = str_val.coerce_to_utf8(ctx.into()) {
        s
    } else {
        return JS_ATOM_NULL;
    };

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
                p.atom_info().unwrap().hint() as u8,
                p.content().calc_hash(JSAtomTypeHint::String)
            );
            p.content().dump();
            // JS_DumpString(rt, p);
            println!(" }}");
        }
    }

    println!("}}");
}
