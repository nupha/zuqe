use std::{collections::hash_map::Entry, ffi::CStr, num::NonZeroU16, rc::Rc};

use crate::{
    atom::{JS_ATOM_HASH_PRIVATE, JS_ATOM_HASH_SYMBOL, JS_ATOM_TAG_INT},
    number::parse_u32,
    runtime::JSRuntime,
    string::{JSString, StringContent},
    utils::hash_u32_with_base,
};

use super::{
    AtomHintAndId, AtomTypeHint, JS_ATOM_MAX_INT, JSAtom,
    consts::{
        JS_ATOM_INIT_COUNT, JS_ATOM_Private_brand, JS_ATOM_Symbol_toPrimitive,
        JS_DEFAULT_ATOMS_TEXT,
    },
};

impl JSAtom {
    /// register an atom from `str`. `str` could be None when atom has no description.
    /// returns atom with ownership, caller should free it after use.
    /// Same `str` will return the matched existing atom.
    pub(crate) fn new_internal(
        rt: &mut JSRuntime,
        mut str: Option<JSString>,
        mut atom_type: AtomTypeHint,
    ) -> Result<Self, i32> {
        // match for existing atoms, don't register twice for same content.
        let atom_desc = match atom_type {
            AtomTypeHint::String | AtomTypeHint::GlobalSymbol => {
                let ss = str.take().unwrap();
                debug_assert!(!ss.content().is_ident());

                if let Some(at) = ss.atom_info() {
                    if at.type_hint() == atom_type {
                        let a = at.atom();
                        if !a.is_const() {
                            let _ = Rc::into_raw(ss.0); // leak string strong count
                        }
                        return Ok(a);
                    }
                }

                let hash = Self::calc_atom_hash_content(ss.content(), atom_type);

                // match registered atoms for hash, type and content
                match rt.atom_hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for &aid in kv.get() {
                            let aptr = rt.atom_slots[u16::from(aid) as usize];
                            if !aptr.is_null() {
                                let at = unsafe { (*aptr).atom_info().unwrap() };
                                if at.type_hint() == atom_type
                                    && unsafe { (&*aptr).content() } == ss.content()
                                {
                                    // found existing atom, clone and reuse it.
                                    let a = at.atom();
                                    if !a.is_const() {
                                        unsafe {
                                            let d = Rc::from_raw(aptr);
                                            let s = d.clone();
                                            let _ = Rc::into_raw(d);
                                            let _ = Rc::into_raw(s);
                                            // now strong ref count increased by 1.
                                        }
                                    }
                                    return Ok(a);
                                }
                            }
                        }
                    }
                }

                ss.set_hash(hash);
                ss
            }
            AtomTypeHint::Symbol | AtomTypeHint::Private => {
                // these atom types are unique, even for same string content.
                let ss = if let Some(s) = str.take() {
                    if s.atom_info().is_none() {
                        s
                    } else {
                        // clone to a new jsstring
                        JSString::new(rt, s.content().clone())
                    }
                } else {
                    // anonymous symbol use identical atom
                    JSString::new(rt, StringContent::Ident)
                };

                let hash = if atom_type == AtomTypeHint::Symbol {
                    JS_ATOM_HASH_SYMBOL
                } else {
                    atom_type = AtomTypeHint::Symbol;
                    JS_ATOM_HASH_PRIVATE
                };
                ss.set_hash(hash);
                ss
            }
        };

        //
        // register new atom
        //
        if rt.atom_hash_lut.len() == rt.atom_hash_lut.capacity() {
            rt.atom_hash_lut.try_reserve(16).map_err(|_| -1)?;
        }

        // find free atom slot
        let (idx, aid) = if let Some(idx) = rt
            .atom_slots
            .iter()
            .enumerate()
            .skip(1) // index 0 is not usable
            .find(|(_, p)| p.is_null())
            .map(|(i, _)| i)
        {
            (Some(idx), NonZeroU16::new(idx as u16).unwrap())
        } else {
            if rt.atom_slots.len() == rt.atom_slots.capacity() {
                rt.atom_slots.try_reserve(16).map_err(|_| -1)?;
            }
            (None, NonZeroU16::new(rt.atom_slots.len() as u16).unwrap())
        };

        atom_desc.set_atom_type(match atom_type {
            AtomTypeHint::String => AtomHintAndId::String(aid),
            AtomTypeHint::GlobalSymbol => AtomHintAndId::GlobalSymbol(aid),
            AtomTypeHint::Symbol => AtomHintAndId::Symbol(aid),
            AtomTypeHint::Private => AtomHintAndId::Private(aid),
        });

        match rt.atom_hash_lut.entry(atom_desc.hash()) {
            Entry::Vacant(kv) => {
                kv.insert(vec![aid]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(aid);
            }
        }

        if let Some(i) = idx {
            debug_assert!(i != 0, "atom slot #0 is not usable");
            debug_assert!(rt.atom_slots[i].is_null(), "atom slot {i} is in use");
            debug_assert_eq!(i, u16::from(aid) as usize);
            rt.atom_slots[i] = Rc::into_raw(atom_desc.0);
        } else {
            rt.atom_slots.push(Rc::into_raw(atom_desc.0));
            debug_assert_eq!(rt.atom_slots.len() - 1, u16::from(aid) as usize);
        }

        Ok(JSAtom(aid.get() as u32))
    }

    /// make atom from given JSString.
    /// Note: caller should free the result atom.
    pub fn from_string(
        rt: &mut JSRuntime,
        str: JSString,
        type_hint: AtomTypeHint,
    ) -> Result<JSAtom, i32> {
        if let Ok(i) = str.content().parse_u32()
            && i <= JS_ATOM_MAX_INT
        {
            Ok(JSAtom::new_small_int(i))
        } else {
            Self::new_internal(rt, Some(str), type_hint)
        }
    }

    /// make atom from `str`
    pub fn from_str(rt: &mut JSRuntime, str: &str, type_hint: AtomTypeHint) -> Result<Self, i32> {
        if let Ok(i) = parse_u32(str)
            && i <= JS_ATOM_MAX_INT
        {
            Ok(JSAtom::new_small_int(i))
        } else {
            let s = JSString::from_str(rt, str)?;
            Self::from_string(rt, s, type_hint)
        }
    }

    pub fn from_i64(rt: &mut JSRuntime, n: i64) -> Result<Self, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&n) {
            Ok(Self::new_small_int(n as u32))
        } else {
            let s = JSString::from_utf8(rt, format!("{n}").into_bytes())?;
            Self::from_string(rt, s, AtomTypeHint::String)
        }
    }

    #[inline]
    pub fn from_u32(rt: &mut JSRuntime, n: u32) -> Result<Self, i32> {
        if n <= JS_ATOM_MAX_INT {
            Ok(Self::new_small_int(n))
        } else {
            Self::from_i64(rt, n as i64)
        }
    }

    /// Make an small integral atom.
    /// Small integral atom is primitive, and do not need to register.
    #[inline(always)]
    pub(crate) fn new_small_int(n: u32) -> Self {
        debug_assert!(n <= JS_ATOM_MAX_INT);
        Self(n | JS_ATOM_TAG_INT)
    }

    /// create new or reuse existing atom for `str`.
    pub fn new(rt: &mut JSRuntime, str: &str) -> Result<Self, i32> {
        if str.is_empty() || !str.chars().next().unwrap().is_ascii_digit() {
            // find atom in registry
            if let Some(atom) = Self::find(rt, str, AtomTypeHint::String) {
                return Ok(atom);
            }
        }

        // str is possibally an integer
        Self::from_str(rt, str, AtomTypeHint::String)
    }

    /// find atom by str
    pub fn find(rt: &JSRuntime, str: &str, atom_type: AtomTypeHint) -> Option<Self> {
        let hash = Self::calc_atom_hash_str(str, atom_type);

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

    /// calc hash for content when using as an atom descriptor
    pub(crate) fn calc_atom_hash_content(desc: &StringContent, atom_type: AtomTypeHint) -> u32 {
        let mut h = atom_type as u32;
        match &desc {
            StringContent::Empty | StringContent::Ident => h,
            StringContent::Ascii(s) => {
                for c in s.iter() {
                    h = hash_u32_with_base(h, (*c) as u32);
                }
                h
            }
            StringContent::Utf16(s) => {
                for c in s.iter() {
                    h = hash_u32_with_base(h, (*c) as u32);
                }
                h
            }
        }
    }

    /// calc hash for str when using as an atom descriptor
    #[inline]
    pub(crate) fn calc_atom_hash_str(str: &str, atom_type: AtomTypeHint) -> u32 {
        let mut h = atom_type as u32;
        for c in str.encode_utf16() {
            h = hash_u32_with_base(h, c as u32);
        }
        h
    }
}

impl JSRuntime {
    /// find atom by jsstring.
    /// TODO: move this to JSAtom impl
    pub fn find_atom_for(&self, jsstr: &JSString) -> Option<JSAtom> {
        if let Some(at) = jsstr.atom_info() {
            Some(at.atom())
        } else {
            // normally, this won't happen
            unreachable!();

            // // find in registry
            // if let Some(ids) = self.atom_hash_lut.get(&jsstr.hash()) {
            //     for &aid in ids {
            //         let idx = u16::from(aid) as usize - 1;
            //         if self.atom_slots[idx].unwrap().as_ptr() as *const JSString
            //             == jsstr as *const JSString
            //         {
            //             return Some(JSAtom(u16::from(aid) as u32));
            //         }
            //     }
            // }
            // None
        }
    }

    /// get atom description for atom.
    /// returns None if atom does not exist.
    /// TODO: move this to JSAtom impl
    pub fn get_atom_desc(&self, atom: JSAtom) -> Option<JSString> {
        debug_assert!(atom.0 != 0);

        if let Some(s) = self
            .atom_slots
            .get(atom.0 as usize)
            .and_then(|p| unsafe { p.as_ref() })
        {
            // increament strong ref count and leak.
            let d = unsafe { Rc::from_raw(s) };
            let s = JSString(d.clone());
            let _ = Rc::into_raw(d);
            Some(s)
        } else {
            None
        }
    }

    /// initialize default constant atoms
    pub fn init_default_atoms(&mut self) -> Result<(), i32> {
        let mut p = JS_DEFAULT_ATOMS_TEXT.as_ptr();
        for i in 1..JS_ATOM_INIT_COUNT {
            let atom_type = if i == JS_ATOM_Private_brand.0 {
                AtomTypeHint::Private
            } else if i >= JS_ATOM_Symbol_toPrimitive.0 {
                AtomTypeHint::Symbol
            } else {
                AtomTypeHint::String
            };

            let cstr = unsafe { CStr::from_ptr(p.cast()) };
            JSAtom::from_str(self, cstr.to_str().unwrap(), atom_type)?;

            p = unsafe { p.add(cstr.count_bytes() + 1) };
        }

        Ok(())
    }
}
