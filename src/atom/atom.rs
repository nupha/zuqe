use std::{borrow::Cow, ffi::CStr, os::raw::c_char};

use crate::{
    context::JSContext,
    mem::{js_free_rt, js_malloc_typed, js_mallocz_rt, js_realloc_rt},
    regexp::is_digit,
    runtime::JSRuntime,
    types::likely,
    value::{
        object::JS_ToPropertyKey, JSString, JSTag, JSValue, JSValueConst, JS_DumpString,
        JS_FreeValue, JS_IsException, JS_IsUndefined, JS_ToNumber, JS_ToString, StringContent,
        JS_TAG_SYMBOL, JS_UNDEFINED, JS_VALUE_GET_INT, JS_VALUE_GET_PTR, JS_VALUE_GET_STRING,
        JS_VALUE_GET_TAG,
    },
    JS_MKPTR,
};

use super::{consts::*, JSAtomStruct};

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Hash)]
pub struct JSAtom(pub(crate) u32);

impl JSAtom {
    /// NOTE: str will be consumed once.
    /// if str is still alive after calling, it will be returned as Some(_)
    pub(crate) fn new(
        rt: &mut JSRuntime,
        mut str: Option<Box<JSString>>,
        atom_type: JSAtomType,
    ) -> Result<(JSAtom, Option<Box<JSString>>), (i32, Option<Box<JSString>>)> {
        debug_assert!(atom_type != JSAtomType::None);

        macro_rules! fail {
            () => {
                return Err((-1, str.and_then(|s| s.free(rt))));
            };
        }

        // calc atom hash, search for existing.
        let h1: u32;
        let h: u32;

        match atom_type {
            JSAtomType::String | JSAtomType::GlobalSymbol => {
                let ss = str.as_deref_mut().unwrap();

                if ss.atom_type() == atom_type {
                    unsafe {
                        let i = js_get_atom_index(rt, ss);
                        /* reduce string refcount and increase atom's unless constant */
                        return Ok((
                            i,
                            if __JS_AtomIsConst(i) {
                                str.and_then(|s| s.free(rt))
                            } else {
                                str
                            },
                        ));
                    }
                }

                /* try and locate an already registered atom */
                h = unsafe { ss.content().make_hash(atom_type as u32) & JS_ATOM_HASH_MASK };
                h1 = h & (rt.atom_hash_size as u32 - 1);

                unsafe {
                    let mut i = *rt.atom_hash.offset(h1 as isize);
                    while i != 0 {
                        let p = *rt.atom_array.offset(i as isize);
                        if (*p).hash() == h
                            && (*p).atom_type() == atom_type
                            && (*p).content() == ss.content()
                        {
                            let a = JSAtom(i);
                            if !__JS_AtomIsConst(a) {
                                (*p).header.ref_count += 1;
                            }
                            return Ok((a, str.and_then(|s| s.free(rt))));
                        }
                        i = (*p).hash_next;
                    }
                }
            }
            JSAtomType::Symbol => {
                h = JS_ATOM_HASH_SYMBOL;
                h1 = 0;
            }
            JSAtomType::Private => {
                h = JS_ATOM_HASH_PRIVATE;
                h1 = 0;
            }
            JSAtomType::None => {
                unreachable!()
            }
        }

        if rt.atom_free_index == 0 {
            /* allow new atom entries */

            /* alloc new with size progression 3/2:
              4 6 9 13 19 28 42 63 94 141 211 316 474 711 1066 1599 2398 3597 5395 8092
              preallocating space for predefined atoms (at least 195).
            */
            let new_size = std::cmp::max(211, rt.atom_size * 3 / 2);
            if new_size > JS_ATOM_MAX as i32 {
                fail!();
            }

            /* XXX: should use realloc2 to use slack space */
            let new_array = unsafe {
                js_realloc_rt(
                    rt,
                    rt.atom_array.cast(),
                    std::mem::size_of::<*mut JSAtomStruct>() * new_size as usize,
                )
                .cast::<*mut JSAtomStruct>()
            };
            if new_array.is_null() {
                fail!();
            }

            /* Note: the atom 0 is not used */
            let mut start = rt.atom_size;
            if start == 0 {
                unsafe {
                    /* JS_ATOM_NULL entry */
                    let p =
                        js_mallocz_rt(rt, std::mem::size_of::<JSAtomStruct>()) as *mut JSAtomStruct;
                    if p.is_null() {
                        js_free_rt(rt, new_array as _);
                        fail!();
                    }
                    (*p).header.ref_count = 1;
                    (*p).set_atom_type(JSAtomType::Symbol);

                    #[cfg(feature = "dump_leaks")]
                    crate::types::list_add_tail(&mut (*p).link, &mut rt.string_list);

                    *new_array.add(0) = p;
                }

                rt.atom_count += 1;
                start = 1;
            }

            rt.atom_size = new_size;
            rt.atom_array = new_array;
            rt.atom_free_index = start;

            for idx in start..new_size {
                let next = if idx == new_size - 1 { 0 } else { idx + 1 };
                unsafe {
                    *new_array.add(idx as usize) = atom_set_free(next as usize);
                }
            }
        }

        let atom_str = if let Some(mut ss) = str.take() {
            if ss.atom_type() == JSAtomType::None {
                ss.set_atom_type(atom_type);
                ss
            } else {
                if let Ok(s2) = JSString::copy_from(rt, &ss) {
                    str = ss.free(rt);
                    s2
                } else {
                    return Err((-1, ss.free(rt)));
                }
            }
        } else {
            // empty string
            match JSString::from_content(rt, StringContent::Empty) {
                Ok(s) => s,
                Err(_) => {
                    return Err((-1, None));
                }
            }
        };

        let p = Box::into_raw(atom_str);

        /* use an already free entry */
        let i = rt.atom_free_index as u32;

        unsafe {
            rt.atom_free_index = atom_get_free(*rt.atom_array.add(i as usize)) as i32;
            *rt.atom_array.offset(i as isize) = p;

            (*p).set_hash(h);
            (*p).hash_next = i;
            (*p).set_atom_type(atom_type);
            rt.atom_count += 1;

            if atom_type != JSAtomType::Symbol {
                (*p).hash_next = *rt.atom_hash.offset(h1 as isize);
                *rt.atom_hash.offset(h1 as isize) = i;
                if rt.atom_count >= rt.atom_count_resize {
                    JS_ResizeAtomHash(rt, rt.atom_hash_size * 2);
                }
            }
        }

        Ok((JSAtom(i), str))
    }

    /// create atom from str
    pub fn from_str(rt: &mut JSRuntime, str: &str, atom_type: JSAtomType) -> Result<JSAtom, i32> {
        let p = JSString::from_str(rt, str)?;
        match JSAtom::new(rt, Some(p), atom_type) {
            Ok((a, _s)) => {
                debug_assert!(_s.is_none());
                Ok(a)
            }
            Err((e, _s)) => {
                debug_assert!(_s.is_none());
                Err(e)
            }
        }
    }

    #[inline(always)]
    pub fn from_u32(n: u32) -> JSAtom {
        JSAtom(n | JS_ATOM_TAG_INT)
    }

    /// test if an atom is int type
    #[inline(always)]
    pub fn is_int_type(&self) -> bool {
        (self.0 & JS_ATOM_TAG_INT) != 0
    }

    /// add atom ref count.
    #[inline]
    pub unsafe fn dup(&self, rt: &mut JSRuntime) -> JSAtom {
        let v = *self;
        if !__JS_AtomIsConst(v) {
            let p = rt.atom_array.add(v.0 as _);
            (*(*p)).header.ref_count += 1;
        }
        v
    }

    /// convert atom to str
    pub unsafe fn to_str(&self, rt: &JSRuntime) -> Cow<'_, str> {
        if self.is_int_type() {
            Cow::Owned(format!("{}", __JS_AtomToUInt32(*self)))
        } else {
            debug_assert!(
                self.0 < rt.atom_size as u32,
                "atom={self:?}, atom_size={}",
                rt.atom_size
            );

            if self.eq(&JS_ATOM_NULL) {
                Cow::Borrowed("<null>")
            } else {
                let str = *rt.atom_array.offset(self.0 as isize);
                debug_assert!(!str.is_null());
                debug_assert!(!atom_is_free(str), "atom was already freed");

                (*str).content().to_str()
            }
        }
    }

    /// convert atom to JSValue.
    /// will return JSValue::EXCEPTION if failed.
    pub unsafe fn to_value(self, ctx: &mut JSContext, force_string: bool) -> JSValue {
        if self.is_int_type() {
            ctx.create_string(&format!("{}", __JS_AtomToUInt32(self)))
        } else {
            let rt = ctx.rt();
            debug_assert!(self.0 < rt.atom_size as u32);

            let mut p = *rt.atom_array.add(self.0 as usize);
            if (*p).atom_type() == JSAtomType::String {
                JS_MKPTR!(JSTag::String, p.cast()).dup()
            } else if force_string {
                if (*p).len() == 0 && (*p).is_wide_char() {
                    // wide empty string as empty string
                    p = *rt.atom_array.offset(JS_ATOM_empty_string.0 as isize);
                }
                JS_MKPTR!(JSTag::String, p.cast()).dup()
            } else {
                JS_MKPTR!(JSTag::Symbol, p.cast()).dup()
            }
        }
    }

    /// convert atom to string or str
    pub unsafe fn to_utf8_string(self, ctx: &mut JSContext) -> Result<Cow<'_, str>, i32> {
        let str_val = self.to_value(ctx, true);
        if JS_IsException(str_val) {
            return Err(-1);
        }

        let cx = ctx as *mut JSContext;
        let res = str_val.to_utf8_string(ctx);
        JS_FreeValue(cx, str_val);

        res
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum JSAtomType {
    None = 0, // 内部使用
    String = 1,
    GlobalSymbol = 2,
    Symbol = 3,
    Private = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum JSAtomKindEnum {
    JS_ATOM_KIND_STRING,
    JS_ATOM_KIND_SYMBOL,
    JS_ATOM_KIND_PRIVATE,
}

pub const JS_ATOM_NULL: JSAtom = JSAtom(0);

pub const JS_ATOM_HASH_SYMBOL: u32 = 0;
pub const JS_ATOM_HASH_PRIVATE: u32 = 1;
pub const JS_ATOM_HASH_MASK: u32 = (1 << 30) - 1;

pub const JS_ATOM_TAG_INT: u32 = 1 << 31;
pub const JS_ATOM_MAX_INT: u32 = JS_ATOM_TAG_INT - 1;
pub const JS_ATOM_MAX: u32 = (1 << 30) - 1;

impl JSRuntime {
    /// initialize default atoms
    pub unsafe fn init_default_atoms(&mut self) -> Result<(), i32> {
        self.atom_hash_size = 0;
        self.atom_hash = std::ptr::null_mut();
        self.atom_count = 0;
        self.atom_size = 0;
        self.atom_free_index = 0;

        if JS_ResizeAtomHash(self, 256) != 0 {
            /* there are at least 195 predefined atoms */
            return Err(-1);
        }

        let mut p = JS_DEFAULT_ATOMS_TEXT.as_ptr();
        for i in 1..JS_ATOM_INIT_COUNT {
            let atom_type = if i == JS_ATOM_Private_brand.0 {
                JSAtomType::Private
            } else if i >= JS_ATOM_Symbol_toPrimitive.0 {
                JSAtomType::Symbol
            } else {
                JSAtomType::String
            };
            let cstr = CStr::from_ptr(p.cast());
            JSAtom::from_str(self, cstr.to_str().unwrap(), atom_type)?;

            p = p.add(cstr.count_bytes() + 1);
        }

        Ok(())
    }

    /// get atom ref count, for const atom, returns None
    #[cfg(debug_assertions)]
    pub unsafe fn atom_ref_count(&self, v: JSAtom) -> Option<i32> {
        debug_assert!((v.0 as i32) < self.atom_size);
        if !__JS_AtomIsConst(v) {
            let p = self.atom_array.add(v.0 as _);
            Some((*(*p)).header.ref_count)
        } else {
            // None
            let p = self.atom_array.add(v.0 as _);
            Some((*(*p)).header.ref_count)
        }
    }
}

#[inline(always)]
fn JS_ATOM_COUNT_RESIZE(n: i32) -> i32 {
    n * 2
}

/// 调整原子哈希表大小
pub unsafe fn JS_ResizeAtomHash(rt: *mut JSRuntime, new_hash_size: i32) -> i32 {
    assert!(new_hash_size > 0 && (new_hash_size & (new_hash_size - 1)) == 0);
    let new_hash_mask = (new_hash_size as u32) - 1;

    let new_hash =
        js_mallocz_rt(rt, std::mem::size_of::<u32>() * new_hash_size as usize) as *mut u32;
    if new_hash.is_null() {
        return -1;
    }

    let atom_hash_size = (*rt).atom_hash_size as usize;
    for i in 0..atom_hash_size {
        let mut h = *(*rt).atom_hash.add(i);
        while h != 0 {
            let p = *(*rt).atom_array.add(h as usize) as *mut JSAtomStruct;
            let hash_next1 = (*p).hash_next;
            let j = ((*p).hash() & new_hash_mask) as usize;

            (*p).hash_next = *new_hash.add(j);
            *new_hash.add(j) = h;

            h = hash_next1;
        }
    }

    js_free_rt(rt, (*rt).atom_hash.cast());
    (*rt).atom_hash = new_hash;
    (*rt).atom_hash_size = new_hash_size;
    (*rt).atom_count_resize = JS_ATOM_COUNT_RESIZE(new_hash_size);

    0
}

#[inline(always)]
unsafe fn atom_get_free(p: *const JSAtomStruct) -> usize {
    p as usize >> 1
}

#[inline(always)]
pub unsafe fn atom_set_free(v: usize) -> *mut JSAtomStruct {
    ((v << 1) | 1) as *mut JSAtomStruct
}

pub unsafe fn atom_is_free(p: *const JSAtomStruct) -> bool {
    (p as usize & 1) != 0
}

pub unsafe fn js_get_atom_index(rt: &JSRuntime, p: *mut JSAtomStruct) -> JSAtom {
    let mut i: u32 = (*p).hash_next; /* atom_index */

    if (*p).atom_type() != JSAtomType::Symbol {
        i = *rt
            .atom_hash
            .add(((*p).hash() & (rt.atom_hash_size as u32 - 1)) as usize);
        let mut p1 = *rt.atom_array.add(i as usize);

        while p1 != p {
            debug_assert!(i != 0);
            i = (*p1).hash_next;
            p1 = *rt.atom_array.add(i as usize);
        }
    }

    JSAtom::from_u32(i)
}

#[inline(always)]
pub unsafe fn __JS_AtomIsConst(v: JSAtom) -> bool {
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
    JSAtom::from_u32(n)
}

#[inline(always)]
pub fn __JS_AtomToUInt32(atom: JSAtom) -> u32 {
    atom.0 & !JS_ATOM_TAG_INT
}

#[deprecated(note = "use JSRuntime::new_atom() instead")]
/* string case (internal). Return Err if error. 'str' is freed. */
pub unsafe fn __JS_NewAtom(
    rt: *mut JSRuntime,
    str: *mut JSString,
    atom_type: JSAtomType,
) -> Result<JSAtom, i32> {
    match JSAtom::new(
        rt.as_mut().unwrap(),
        if str.is_null() {
            None
        } else {
            Some(Box::from_raw(str))
        },
        atom_type,
    ) {
        Ok((a, _ss)) => {
            if let Some(ss) = _ss {
                std::mem::forget(ss);
            }
            Ok(a)
        }
        Err((e, _ss)) => {
            if let Some(ss) = _ss {
                std::mem::forget(ss);
            }
            Err(e)
        }
    }
}

#[deprecated(note = "use JSAtom::from_str() instead")]
pub unsafe fn __JS_NewAtomInit(
    rt: &mut JSRuntime,
    str: &str,
    atom_type: JSAtomType,
) -> Result<JSAtom, i32> {
    JSAtom::from_str(rt, str, atom_type)
}

#[deprecated(note = "use JSAtom::dup() instead")]
pub unsafe fn JS_DupAtomRT(rt: *mut JSRuntime, v: JSAtom) -> JSAtom {
    v.dup(rt.as_mut().unwrap())
}

#[deprecated(note = "use JSAtom::dup() instead")]
#[inline(always)]
pub unsafe fn JS_DupAtom(ctx: *mut JSContext, v: JSAtom) -> JSAtom {
    v.dup((*ctx).rt.as_mut())
}

pub unsafe fn JS_AtomGetKind(ctx: *mut JSContext, v: JSAtom) -> JSAtomKindEnum {
    let rt = (*ctx).rt.as_ref();
    if v.is_int_type() {
        return JSAtomKindEnum::JS_ATOM_KIND_STRING;
    }
    let p = *rt.atom_array.offset(v.0 as isize);
    match (*p).atom_type() {
        JSAtomType::String => JSAtomKindEnum::JS_ATOM_KIND_STRING,
        JSAtomType::GlobalSymbol => JSAtomKindEnum::JS_ATOM_KIND_SYMBOL,
        JSAtomType::Symbol => match (*p).hash() {
            JS_ATOM_HASH_SYMBOL => JSAtomKindEnum::JS_ATOM_KIND_SYMBOL,
            JS_ATOM_HASH_PRIVATE => JSAtomKindEnum::JS_ATOM_KIND_PRIVATE,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

pub const ATOM_GET_STR_BUF_SIZE: usize = 64;

#[deprecated(note = "use JSRuntime::atom_to_str() instead")]
pub unsafe fn JS_AtomGetStrRT(
    rt: *mut JSRuntime,
    buf: *mut c_char,
    buf_size: usize,
    atom: JSAtom,
) -> *const c_char {
    unreachable!("use JSRuntime::atom_to_str() instead")

    // (*rt)
    //     .atom_to_str(
    //         std::slice::from_raw_parts_mut(buf.cast::<u8>(), buf_size),
    //         atom,
    //     )
    //     .as_ptr()
}

#[deprecated(note = "use JSAtom::to_str() instead")]
pub unsafe fn JS_AtomGetStr(
    ctx: *mut JSContext,
    buf: *mut c_char,
    buf_size: usize,
    atom: JSAtom,
) -> *const c_char {
    unreachable!("unused");
}

/// debug helper: print atom, don't use in production code
pub unsafe fn print_atom(rt: *const JSRuntime, atom: JSAtom) {
    print!("{}", atom.to_str(rt.as_ref().unwrap()));
}

impl JSContext {
    /// create new or reuse existing atom for `str`.
    pub fn create_atom(&mut self, str: &str) -> Result<JSAtom, i32> {
        // format [xxxx]
        if str.starts_with('[') {
            let name2 = &str[1..str.len() - 1]; // chop heading '[', tailing ']'

            unsafe {
                // We assume 8 bit non null strings, which is the case for these symbols
                let rt = self.rt_mut();

                for i in JS_ATOM_Symbol_toPrimitive.0..JS_ATOM_INIT_COUNT {
                    let astr = *rt.atom_array.add(i as usize);
                    if (*astr).content().eq_str(name2) {
                        return Ok(JSAtom(i).dup(rt));
                    }
                }

                self.JS_ThrowInternalError(CStr::from_ptr(
                    format!("unknown atom '{str}'\0").as_ptr().cast(),
                ));
                return Err(-1);
            }
        }

        if str.len() == 0 || !is_digit(str.as_bytes()[0]) {
            if let Some(atom) = unsafe { __JS_FindAtom(self.rt(), str, JSAtomType::String) } {
                return Ok(atom);
            }
        }

        let val = self.create_string(str).check_exception()?;

        #[cfg(debug_assertions)]
        {
            let (a, s) = unsafe { Box::from_raw(JS_VALUE_GET_STRING(val)).into_atom(self) };
            debug_assert!(s.is_none());
            Ok(a)
        }

        #[cfg(not(debug_assertions))]
        unsafe {
            Ok(Box::from_raw(JS_VALUE_GET_STRING(val)).into_atom(self).0)
        }
    }
}

impl JSString {
    /// convert jsstring to atom. jsstring will be freed.
    /// if jsstring is still alive after free, it will be returned as Some(_).
    pub unsafe fn into_atom(self: Box<Self>, ctx: &mut JSContext) -> (JSAtom, Option<Box<Self>>) {
        if let Ok(n) = self.content().to_u32() {
            if n <= JS_ATOM_MAX_INT {
                let ss = self.free(ctx.rt_mut());
                return (JSAtom::from_u32(n), ss);
            }
        }

        // if is_num_string(&mut n, self.as_ref()) {
        //     if n <= JS_ATOM_MAX_INT {
        //         let ss = self.free(ctx.rt.as_mut().unwrap());
        //         return (__JS_AtomFromUInt32(n), ss);
        //     }
        // }

        /* TODO: should generate an exception */
        match JSAtom::new(ctx.rt_mut(), Some(self), JSAtomType::String) {
            Ok((a, s)) => (a, s),
            Err((_, s)) => (JS_ATOM_NULL, s),
        }
    }
}

#[deprecated(note = "use JSAtom::to_value() instead")]
pub unsafe fn __JS_AtomToValue(ctx: *mut JSContext, atom: JSAtom, force_string: bool) -> JSValue {
    atom.to_value(ctx.as_mut().unwrap(), force_string)

    // if atom.is_int_type() {
    //     (*ctx).create_string(&format!("{}", __JS_AtomToUInt32(atom)))
    // } else {
    //     let rt = (*ctx).rt.as_ref();
    //     debug_assert!(atom.0 < rt.atom_size as u32);

    //     let mut p = *rt.atom_array.add(atom.0 as usize);
    //     if (*p).atom_type() == JSAtomType::String {
    //         JS_DupValue(ctx, JS_MKPTR!(JSTag::String, p.cast()))
    //     } else if force_string {
    //         if (*p).len() == 0 && (*p).is_wide_char() {
    //             // wide empty string as empty string
    //             p = *rt.atom_array.offset(JS_ATOM_empty_string.0 as isize);
    //         }
    //         JS_DupValue(ctx, JS_MKPTR!(JSTag::String, p.cast()))
    //     } else {
    //         JS_DupValue(ctx, JS_MKPTR!(JSTag::Symbol, p.cast()))
    //     }
    // }
}

#[deprecated(note = "use JSAtom::to_value() instead")]
pub unsafe fn JS_AtomToValue(ctx: *mut JSContext, atom: JSAtom) -> JSValue {
    __JS_AtomToValue(ctx, atom, false)
}

#[deprecated(note = "use JSAtom::to_value() instead")]
pub unsafe fn JS_AtomToString(ctx: *mut JSContext, atom: JSAtom) -> JSValue {
    __JS_AtomToValue(ctx, atom, true)
}

/// Warning: 'p' is freed
#[deprecated(note = "use JSString::into_atom() instead")]
pub unsafe fn JS_NewAtomStr(ctx: *mut JSContext, p: *mut JSString) -> JSAtom {
    let ss = Box::from_raw(p); // take ownership

    let (a, s) = ss.into_atom(ctx.as_mut().unwrap());
    if let Some(s) = s {
        // JSString is still alive, release ownership
        std::mem::forget(s);
    }

    a
}

#[deprecated(note = "use JSContext::create_atom() instead")]
pub unsafe fn JS_NewAtomLen(ctx: *mut JSContext, str: *const u8, len: usize) -> JSAtom {
    (*ctx)
        .create_atom(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
            str, len,
        )))
        .unwrap_or(JS_ATOM_NULL)
}

#[deprecated(note = "use JSContext::create_atom() instead")]
pub unsafe fn JS_NewAtom(ctx: *mut JSContext, str: &CStr) -> JSAtom {
    (*ctx)
        .create_atom(str.to_string_lossy().as_ref())
        .unwrap_or(JS_ATOM_NULL)
}

pub unsafe fn JS_NewAtomString(ctx: *mut JSContext, str: &CStr) -> JSValue {
    let atom = JS_NewAtom(ctx, str);
    if atom == JS_ATOM_NULL {
        return JSValue::EXCEPTION;
    }
    let val = atom.to_value(ctx.as_mut().unwrap(), true);
    JS_FreeAtom(ctx, atom);
    val
}

pub unsafe fn JS_NewAtomUInt32(ctx: *mut JSContext, n: u32) -> JSAtom {
    if n <= JS_ATOM_MAX_INT {
        JSAtom::from_u32(n)
    } else {
        let val = (*ctx).create_string(&format!("{n}"));
        if JS_IsException(val) {
            return JS_ATOM_NULL;
        }

        let str = JS_VALUE_GET_STRING(val);
        match JSAtom::new(
            (*ctx).rt_mut(),
            if str.is_null() {
                None
            } else {
                Some(Box::from_raw(str))
            },
            JSAtomType::String,
        ) {
            Ok((a, _ss)) => {
                if let Some(ss) = _ss {
                    std::mem::forget(ss);
                }
                a
            }
            Err((e, _ss)) => {
                if let Some(ss) = _ss {
                    std::mem::forget(ss);
                }
                JS_ATOM_NULL
            }
        }
    }
}

pub unsafe fn JS_NewAtomInt64(ctx: *mut JSContext, n: i64) -> JSAtom {
    if (n as u64) <= JS_ATOM_MAX_INT as u64 {
        JSAtom::from_u32(n as u32)
    } else {
        let val = (*ctx).create_string(&format!("{n}"));
        if JS_IsException(val) {
            return JS_ATOM_NULL;
        }

        let str = JS_VALUE_GET_STRING(val);
        match JSAtom::new(
            (*ctx).rt_mut(),
            if str.is_null() {
                None
            } else {
                Some(Box::from_raw(str))
            },
            JSAtomType::String,
        ) {
            Ok((a, _ss)) => {
                if let Some(ss) = _ss {
                    std::mem::forget(ss);
                }
                a
            }
            Err((e, _ss)) => {
                if let Some(ss) = _ss {
                    std::mem::forget(ss);
                }
                JS_ATOM_NULL
            }
        }
    }
}

pub unsafe fn __JS_FindAtom(
    rt: *const JSRuntime,
    str: &str,
    atom_type: JSAtomType,
) -> Option<JSAtom> {
    let h = StringContent::hash_str(str, JSAtomType::String as _) & JS_ATOM_HASH_MASK;
    let h1 = h & (*rt).atom_hash_size as u32 - 1;
    let mut i = (*rt).atom_hash.offset(h1 as isize).read();
    debug_assert!(i < (*rt).atom_count as u32);

    while i != 0 {
        let p = (*rt).atom_array.offset(i as isize).read();
        if (*p).hash() == h
            && (*p).atom_type() == JSAtomType::String
            && !(*p).is_wide_char()
            && (*p).content().eq_str(str)
        {
            let a = JSAtom(i);
            if !__JS_AtomIsConst(a) {
                (*p).header.ref_count += 1;
            }
            return Some(a);
        }
        i = (*p).hash_next;
    }

    None
}

#[deprecated(note = "use JSContext::create_atom() instead")]
pub unsafe fn find_atom(ctx: *mut JSContext, name: &str) -> JSAtom {
    (*ctx).create_atom(name).unwrap_or(JS_ATOM_NULL)

    // if name.starts_with('[') {
    //     let name2 = &name[1..name.len() - 1]; // chop heading '[', tailing ']'

    //     // We assume 8 bit non null strings, which is the case for these symbols
    //     for a in JS_ATOM_Symbol_toPrimitive.0..JS_ATOM_INIT_COUNT {
    //         let astr = *(*(*ctx).rt).atom_array.add(a as usize);
    //         if (*astr).content().eq_str(name2) {
    //             return JS_DupAtom(ctx, a.into());
    //         }
    //     }

    //     unreachable!();
    // } else {
    //     (*ctx).create_atom(name).unwrap_or(JS_ATOM_NULL)
    // }
}

pub unsafe fn __JS_FreeAtom(rt: *mut JSRuntime, i: JSAtom) {
    let p = *(*rt).atom_array.offset(i.0 as _);

    debug_assert!(p.is_aligned(), "{p:p}");

    (*p).header.ref_count -= 1;
    if (*p).header.ref_count <= 0 {
        JS_FreeAtomStruct(rt, p);
    }
}

pub unsafe fn JS_FreeAtom(ctx: *mut JSContext, v: JSAtom) {
    if !__JS_AtomIsConst(v) {
        __JS_FreeAtom((*ctx).rt.as_mut(), v);
    }
}

pub unsafe fn JS_FreeAtomRT(rt: *mut JSRuntime, v: JSAtom) {
    if !__JS_AtomIsConst(v) {
        __JS_FreeAtom(rt, v);
    }
}

pub unsafe fn JS_FreeAtomStruct(rt: *mut JSRuntime, p: *mut JSAtomStruct) {
    debug_assert_eq!((*p).header.ref_count, 0);

    // JS_ATOM_NULL is not refcounted: __JS_AtomIsConst() includes 0
    // if unlikely(i == JS_ATOM_NULL) {
    //     (*p).header.ref_count = i32::MAX / 2;
    //     return;
    // }

    let mut i = (*p).hash_next; // atom_index
    if (*p).atom_type() != JSAtomType::Symbol {
        let mut p0: *mut JSAtomStruct;
        let mut p1: *mut JSAtomStruct;
        let h0: u32;

        h0 = (*p).hash() & ((*rt).atom_hash_size as u32 - 1);
        i = *(*rt).atom_hash.offset(h0 as isize);
        p1 = *(*rt).atom_array.offset(i as isize);
        if p1 == p {
            *(*rt).atom_hash.offset(h0 as isize) = (*p1).hash_next;
        } else {
            loop {
                assert!(i != 0);
                p0 = p1;
                i = (*p1).hash_next;
                p1 = *(*rt).atom_array.offset(i as isize);
                if p1 == p {
                    (*p0).hash_next = (*p1).hash_next;
                    break;
                }
            }
        }
    }

    // insert in free atom list
    *(*rt).atom_array.offset(i as isize) = atom_set_free((*rt).atom_free_index as _);
    (*rt).atom_free_index = i as i32;

    #[cfg(feature = "dump_leaks")]
    crate::types::list_del(&mut (*p).link);

    // drop JSString
    drop(Box::from_raw(p));

    (*rt).atom_count -= 1;
    debug_assert!((*rt).atom_count >= 0);
}

#[deprecated(note = "use JSAtom::to_utf8_string() instead")]
pub unsafe fn JS_AtomToCString<'a>(ctx: *mut JSContext, atom: JSAtom) -> *const c_char {
    unreachable!("use JSAtom::to_utf8_string() instead");
}

/// test if the `atom` can be used as an array index (in 0..u32::MAX range).
pub unsafe fn JS_AtomIsArrayIndex(ctx: *mut JSContext, pval: *mut u32, atom: JSAtom) -> bool {
    if atom.is_int_type() {
        *pval = __JS_AtomToUInt32(atom);
        true
    } else {
        *pval = 0;

        let p = *(*ctx).rt().atom_array.offset(atom.0 as isize);
        if (*p).atom_type() == JSAtomType::String {
            if let Ok(n) = (*p).content().to_u32() {
                if n != u32::MAX {
                    *pval = n;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    }
}

pub unsafe fn JS_ValueToAtom(ctx: *mut JSContext, val: JSValueConst) -> JSAtom {
    let tag = JS_VALUE_GET_TAG(val);

    if tag == JSTag::Int as i32 && JS_VALUE_GET_INT(val) as u32 <= JS_ATOM_MAX_INT {
        JSAtom::from_u32(JS_VALUE_GET_INT(val) as u32)
    } else if tag == JSTag::Symbol as i32 {
        let p: *mut JSAtomStruct = JS_VALUE_GET_PTR(val).cast();
        JS_DupAtom(ctx, js_get_atom_index((*ctx).rt(), p))
    } else {
        let str = JS_ToPropertyKey(ctx, val);
        if JS_IsException(str) {
            JS_ATOM_NULL
        } else if JS_VALUE_GET_TAG(str) == JS_TAG_SYMBOL {
            js_symbol_to_atom(ctx, str)
        } else {
            JS_NewAtomStr(ctx, JS_VALUE_GET_STRING(str))
        }
    }
}

/* val must be a symbol */
pub unsafe fn js_symbol_to_atom(ctx: *mut JSContext, val: JSValue) -> JSAtom {
    let p = JS_VALUE_GET_PTR(val).cast::<JSAtomStruct>();
    js_get_atom_index((*ctx).rt(), p)
}

// This test must be fast if atom is not a numeric index (e.g. a
// method name). Return JS_UNDEFINED if not a numeric
// index. JS_EXCEPTION can also be returned.
pub unsafe fn JS_AtomIsNumericIndex1(ctx: *mut JSContext, atom: JSAtom) -> JSValue {
    let rt = (*ctx).rt();
    if atom.is_int_type() {
        return JSValue::from_i32(__JS_AtomToUInt32(atom) as _);
    }
    assert!(atom.0 < rt.atom_size as u32);

    let p1 = *rt.atom_array.offset(atom.0 as isize);
    if (*p1).atom_type() != JSAtomType::String {
        return JS_UNDEFINED;
    }

    let p = p1;
    let len = (*p).len();

    if (*p).content().is_empty() {
        return JS_UNDEFINED;
    }

    // check for: -0, Infinity, -Infinity
    let mut cc = (*p).content().chars();
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
                return JS_UNDEFINED;
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
                            return JS_UNDEFINED;
                        }
                    }
                }
            } else {
                return JS_UNDEFINED;
            }
        }
    }

    // if (*p).is_wide_char() {
    //     let r: *const u16 = (*p).as_str16();
    //     let r_end: *const u16 = (*p).as_str16().add(len as usize);
    //     if r >= r_end {
    //         return JS_UNDEFINED;
    //     }
    //     c = *r as i32;
    //     if c == '-' as i32 {
    //         if r >= r_end {
    //             return JS_UNDEFINED;
    //         }
    //         let new_r = r.offset(1);
    //         c = *new_r as i32;
    //         // -0 case is specific
    //         if c == '0' as i32 && len == 2 {
    //             return __JS_NewFloat64(ctx, -0.0);
    //         }
    //     }
    //     // XXX: should test NaN, but the tests do not check it
    //     if !is_num(c) {
    //         // XXX: String should be normalized, therefore 8-bit only
    //         let nfinity16: [u16; 7] = [
    //             'n' as u16, 'f' as u16, 'i' as u16, 'n' as u16, 'i' as u16, 't' as u16, 'y' as u16,
    //         ];
    //         if !(c == 'I' as i32
    //             && (r_end as usize - r as usize) == 8
    //             && std::ptr::eq(r.offset(1), nfinity16.as_ptr()))
    //         {
    //             return JS_UNDEFINED;
    //         }
    //     }
    // } else {
    //     let r = (*p).as_str8();
    //     let r_end: *const u8 = (*p).as_str8().add(len as usize);
    //     if r >= r_end {
    //         return JS_UNDEFINED;
    //     }
    //     c = *r as i32;
    //     if c == '-' as i32 {
    //         if r >= r_end {
    //             return JS_UNDEFINED;
    //         }
    //         let new_r = r.offset(1);
    //         c = *new_r as i32;
    //         // -0 case is specific
    //         if c == '0' as i32 && len == 2 {
    //             return __JS_NewFloat64(ctx, -0.0);
    //         }
    //     }
    //     if !is_num(c) {
    //         if !(c == 'I' as i32
    //             && (r_end as usize - r as usize) == 8
    //             && std::ptr::eq(r.offset(1), b"nfinity".as_ptr()))
    //         {
    //             return JS_UNDEFINED;
    //         }
    //     }
    // }

    // XXX: bignum: would be better to only accept integer to avoid
    // relying on current floating point precision
    // this is ECMA CanonicalNumericIndexString primitive
    let num = JS_ToNumber(ctx, JS_MKPTR!(JSTag::String, p.cast()));
    if JS_IsException(num) {
        return num;
    }

    let str = JS_ToString(ctx, num);
    if JS_IsException(str) {
        JS_FreeValue(ctx, num);
        JSValue::EXCEPTION
    } else {
        let same = (*p1).content().eq((*JS_VALUE_GET_STRING(str)).content());
        JS_FreeValue(ctx, str);

        if same {
            num
        } else {
            JS_FreeValue(ctx, num);
            JS_UNDEFINED
        }
    }
}

// return -1 if exception or TRUE/FALSE
pub unsafe fn JS_AtomIsNumericIndex(ctx: *mut JSContext, atom: JSAtom) -> i32 {
    let num = JS_AtomIsNumericIndex1(ctx, atom);
    if likely(JS_IsUndefined(num)) {
        return 0;
    }
    if JS_IsException(num) {
        return -1;
    }
    JS_FreeValue(ctx, num);
    return 1;
}

#[deprecated(note = "use JSRuntime::init_default_atoms() instead")]
pub unsafe fn JS_InitAtoms(rt: &mut JSRuntime) -> Result<(), i32> {
    (*rt).init_default_atoms()
}

// return a string atom containing name concatenated with str1
pub unsafe fn js_atom_concat_str(ctx: *mut JSContext, name: JSAtom, str1: *const u8) -> JSAtom {
    let str_val = name.to_value(ctx.as_mut().unwrap(), true);
    if JS_IsException(str_val) {
        return JS_ATOM_NULL;
    }

    let astr = if let Ok(s) = str_val.to_utf8_string(ctx.as_mut().unwrap()) {
        s
    } else {
        JS_FreeValue(ctx, str_val);
        return JS_ATOM_NULL;
    };

    let len = astr.len();
    let len1 = CStr::from_ptr(str1.cast()).count_bytes();
    let ttl_len = len + len1 + 1;

    let res = if let Some(p) = js_malloc_typed::<u8>(ctx, ttl_len, false) {
        let mut new_str = Vec::from_raw_parts(p.as_ptr(), ttl_len, ttl_len);
        std::ptr::copy_nonoverlapping(astr.as_ptr().cast(), new_str.as_mut_ptr(), len);
        std::ptr::copy_nonoverlapping(str1, new_str.as_mut_ptr().offset(len as isize), len1);

        (*ctx)
            .create_atom(std::str::from_utf8_unchecked(&new_str))
            .unwrap_or(JS_ATOM_NULL)
    } else {
        JS_ATOM_NULL
    };

    JS_FreeValue(ctx, str_val);
    res
}

pub unsafe fn js_atom_concat_num(ctx: *mut JSContext, name: JSAtom, n: u32) -> JSAtom {
    js_atom_concat_str(ctx, name, format!("{n}\0").as_ptr())
}

pub unsafe fn JS_DumpAtoms(rt: *mut JSRuntime) {
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
    for i in JS_ATOM_INIT_COUNT as i32..(*rt).atom_size {
        let p = *(*rt).atom_array.offset(i as isize);
        if !atom_is_free(p) {
            print!(
                "  {}: {{ {} {:08x} ",
                i,
                (*p).atom_type() as u8,
                (*p).hash()
            );
            if !((*p).len() == 0 && (*p).is_wide_char()) {
                JS_DumpString(rt, p);
            }
            println!(" {} }}", (*p).hash_next);
        }
    }
    println!("}}");
}
