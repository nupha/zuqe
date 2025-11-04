use std::{
    cell::RefCell,
    collections::{HashMap, hash_map::Entry},
    ffi::CStr,
    num::NonZeroU16,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use once_cell::unsync::Lazy;

use crate::{
    atom::{Atom, JS_ATOM_HASH_PRIVATE, JS_ATOM_HASH_SYMBOL, JS_ATOM_TAG_INT, is_const_atom},
    number::parse_u32,
    runtime::JSRuntime,
    string::{JSString, JSStringInner, StringContent},
    utils::hash_u32_from_base,
};

use super::{
    AtomHintAndId, AtomRef, AtomTypeHint, JS_ATOM_MAX_INT,
    consts::{
        JS_ATOM_INIT_COUNT, JS_ATOM_Private_brand, JS_ATOM_Symbol_toPrimitive,
        JS_DEFAULT_ATOMS_TEXT,
    },
};

/// The thread global atom registry
pub(crate) struct AtomRegistry<'a> {
    /// const atom description slots list.
    /// index 0 is reserved for unused;
    /// staring from index 1 is for atom 1; index 2 is for atom 2, ...
    const_atom_slots: Vec<JSString<'a>>,

    /// atom description slots list, null indicates the slot is unused.
    /// index 0 is reserved for unused;
    /// staring from index 1 is for atom 1; index 2 is for atom 2, ...
    atom_slots: Vec<*mut JSStringInner<'a>>,

    /// JSString hash to atom vec dict.
    /// Value is list of atom indecies because JSString hash may have collison.
    atom_hash_lut: HashMap<u32, Vec<NonZeroU16>>,
}

impl<'a> Drop for AtomRegistry<'a> {
    fn drop(&mut self) {
        debug_assert!(self.atom_slots.is_empty());

        self.const_atom_slots.clear();
        self.atom_hash_lut.clear();
    }
}

thread_local! {
    /// Thread global atom registry.
    static REGISTRY: Lazy<RefCell<AtomRegistry<'static>>> = Lazy::new(|| {
        let (lst, lut) = load_const_atoms();

        let reg = AtomRegistry {
            const_atom_slots: lst,
            atom_slots: Vec::with_capacity(128),
            atom_hash_lut: lut,
        };

        RefCell::new(reg)
    });
}

/// build const atoms list and hash lut
fn load_const_atoms() -> (Vec<JSString<'static>>, HashMap<u32, Vec<NonZeroU16>>) {
    let mut lst = Vec::<JSString>::with_capacity(JS_ATOM_INIT_COUNT as usize + 1);
    let mut lut = HashMap::<u32, Vec<NonZeroU16>>::with_capacity(JS_ATOM_INIT_COUNT as usize);

    // slot #0 is unused
    lst.push(JSString::from_static("<unused>"));

    let mut p = JS_DEFAULT_ATOMS_TEXT.as_ptr();

    for i in 1..JS_ATOM_INIT_COUNT {
        let cstr = unsafe { CStr::from_ptr(p.cast()) };
        let atom_str = JSString::from_static(cstr.to_str().unwrap());

        let mut atom_type = if i == JS_ATOM_Private_brand.0 {
            AtomTypeHint::Private
        } else if i >= JS_ATOM_Symbol_toPrimitive.0 {
            AtomTypeHint::Symbol
        } else {
            AtomTypeHint::String
        };

        let hash: u32;
        match atom_type {
            AtomTypeHint::String | AtomTypeHint::GlobalSymbol => {
                hash = Atom::calc_atom_hash(atom_str.content(), atom_type);
                atom_str.set_hash(hash);
            }
            AtomTypeHint::Symbol | AtomTypeHint::Private => {
                hash = if atom_type == AtomTypeHint::Symbol {
                    JS_ATOM_HASH_SYMBOL
                } else {
                    atom_type = AtomTypeHint::Symbol;
                    JS_ATOM_HASH_PRIVATE
                };
                atom_str.set_hash(hash);
            }
        };

        let id = NonZeroU16::new(lst.len() as u16).unwrap();
        atom_str.set_atom_type(match atom_type {
            AtomTypeHint::String => AtomHintAndId::String(id),
            AtomTypeHint::GlobalSymbol => AtomHintAndId::GlobalSymbol(id),
            AtomTypeHint::Symbol => AtomHintAndId::Symbol(id),
            AtomTypeHint::Private => AtomHintAndId::Private(id),
        });

        lst.push(atom_str);

        match lut.entry(hash) {
            Entry::Vacant(kv) => {
                kv.insert(vec![id]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(id);
            }
        }

        p = unsafe { p.add(cstr.count_bytes() + 1) };
    }

    (lst, lut)
}

impl<'a> AtomRegistry<'a> {
    /// register an atom with atom desciption.
    /// (in case of creating an anonymous symbol, desc should be an ident-type-string)
    /// returns atom with ownership, caller should free it after use.
    /// Same `str` will return the matched existing atom.
    fn register_internal(
        reg: &mut Self,
        str: JSString,
        mut atom_type: AtomTypeHint,
    ) -> Result<Atom, i32> {
        // match for existing atoms, don't register twice for same content.
        let atom_str = match atom_type {
            AtomTypeHint::String | AtomTypeHint::GlobalSymbol => {
                debug_assert!(!str.content().is_ident());

                let ss = if let Some(at) = str.atom_info() {
                    if at.type_hint() == atom_type {
                        if !is_const_atom(at.id()) {
                            let _ = Rc::into_raw(str.0); // leak string strong count
                        }
                        return Ok(Atom(at.id()));
                    } else {
                        // make new string with same content but without atom info
                        str.copy_to_new()
                    }
                } else {
                    str
                };

                let hash = Atom::calc_atom_hash(ss.content(), atom_type);

                // match registered atoms for hash, type and content
                match reg.atom_hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for aid in kv.get() {
                            let id = aid.get() as u32;
                            if id < JS_ATOM_INIT_COUNT {
                                let at = reg.const_atom_slots[id as usize].atom_info().unwrap();
                                if at.type_hint() == atom_type {
                                    return Ok(Atom(at.id()));
                                }
                            } else {
                                let p = reg.atom_slots[id as usize - JS_ATOM_INIT_COUNT as usize];
                                debug_assert!(!p.is_null());

                                let d = unsafe { &*p };
                                let at = d.atom_info().unwrap();
                                if at.type_hint() == atom_type && d.content() == ss.content() {
                                    // found existing atom, should reuse it.
                                    unsafe {
                                        let d = Rc::from_raw(d);
                                        let s = d.clone();
                                        let _ = Rc::into_raw(d);
                                        let _ = Rc::into_raw(s);
                                    }
                                    return Ok(Atom(at.id()));
                                }
                            }
                        }
                    }
                }

                ss.set_hash(hash);
                ss
            }
            AtomTypeHint::Symbol | AtomTypeHint::Private => {
                // these atom types are distinct, even they have same string content.
                let ss = if str.content().is_ident() {
                    // ident string type is only for anonymous symbol
                    debug_assert_eq!(atom_type, AtomTypeHint::Symbol);
                    str
                } else if str.atom_info().is_none() {
                    str
                } else {
                    // clone content as a new jsstring
                    str.copy_to_new()
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

        if reg.atom_hash_lut.len() == reg.atom_hash_lut.capacity() {
            reg.atom_hash_lut.try_reserve(16).map_err(|_| -1)?;
        }

        // find a free slot
        let (idx, atom_id) = if let Some(idx) = reg.atom_slots.iter().position(|p| p.is_null()) {
            (
                Some(idx),
                NonZeroU16::new(idx as u16 + JS_ATOM_INIT_COUNT as u16).unwrap(),
            )
        } else {
            if reg.atom_slots.len() == reg.atom_slots.capacity() {
                reg.atom_slots.try_reserve(16).map_err(|_| -1)?;
            }
            (
                None,
                NonZeroU16::new(reg.atom_slots.len() as u16 + JS_ATOM_INIT_COUNT as u16).unwrap(),
            )
        };

        atom_str.set_atom_type(match atom_type {
            AtomTypeHint::String => AtomHintAndId::String(atom_id),
            AtomTypeHint::GlobalSymbol => AtomHintAndId::GlobalSymbol(atom_id),
            AtomTypeHint::Symbol => AtomHintAndId::Symbol(atom_id),
            AtomTypeHint::Private => AtomHintAndId::Private(atom_id),
        });

        match reg.atom_hash_lut.entry(atom_str.hash()) {
            Entry::Vacant(kv) => {
                kv.insert(vec![atom_id]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(atom_id);
            }
        }

        // IMPORTANT: leak a reference for the returning atom
        {
            let d = atom_str.clone();
            let _ = Rc::into_raw(d.0);
        }

        // IMPORTANT: leak a reference to atom desc in slot
        let d = Rc::into_raw(atom_str.0) as *mut JSStringInner;

        if let Some(i) = idx {
            debug_assert_eq!(i, atom_id.get() as usize - JS_ATOM_INIT_COUNT as usize);
            debug_assert!(reg.atom_slots[i].is_null(), "atom slot {i} is in use");
            reg.atom_slots[i] = d;
        } else {
            reg.atom_slots.push(d);
            debug_assert_eq!(
                reg.atom_slots.len() - 1,
                atom_id.get() as usize - JS_ATOM_INIT_COUNT as usize
            );
        }

        Ok(Atom(atom_id.get() as u32))
    }

    /// register an atom with description.
    ///
    /// Safety: will panic if no runtime is initialized
    fn register(str: JSString<'a>, atom_type: AtomTypeHint) -> Result<Atom, i32> {
        REGISTRY.with(|reg| Self::register_internal(reg.borrow_mut().deref_mut(), str, atom_type))
    }

    /// get atom description by atom id.
    /// returned atom description is cloned.
    /// returns None if atom id was not registerd.
    pub(super) fn get_desc(atom_id: u32) -> Option<JSString<'a>> {
        debug_assert!(atom_id != 0);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();

            if atom_id < JS_ATOM_INIT_COUNT {
                // const atom
                Some(reg.const_atom_slots[atom_id as usize].clone())
            } else {
                reg.atom_slots
                    .get(atom_id as usize - JS_ATOM_INIT_COUNT as usize)
                    .and_then(|x| unsafe {
                        if let Some(p) = x.as_ref() {
                            let d = Rc::from_raw(p);
                            let d2 = d.clone();
                            let _ = Rc::into_raw(d);
                            Some(JSString(d2))
                        } else {
                            None
                        }
                    })
            }
        })
    }

    /// find atom desc by str and type
    fn find_desc_internal(
        reg: &'a Self,
        _rt: &'a JSRuntime<'a>,
        str: &'a str,
        atom_type: AtomTypeHint,
    ) -> Option<&'a JSStringInner<'a>> {
        reg.atom_hash_lut
            .get(&calc_atom_hash_str(str, atom_type))
            .and_then(|ids| {
                ids.iter().find_map(|id| {
                    let id = id.get() as u32;

                    if id < JS_ATOM_INIT_COUNT {
                        // const atom
                        let s = reg.const_atom_slots[id as usize].0.as_ref();
                        Some(s)
                    } else {
                        let d = unsafe {
                            reg.atom_slots[id as usize - JS_ATOM_INIT_COUNT as usize]
                                .as_ref()
                                .unwrap()
                        };
                        if d.atom_info().unwrap().type_hint() == atom_type
                            && d.content().eq_str(str)
                        {
                            Some(d)
                        } else {
                            None
                        }
                    }
                })
            })
    }

    /// retrive atom by str and type, returned atom has its refcount increased by 1.
    fn find(rt: &JSRuntime, str: &str, atom_type: AtomTypeHint) -> Option<Atom> {
        REGISTRY.with(|reg| {
            Self::find_desc_internal(reg.borrow().deref(), rt, str, atom_type).map(|x| unsafe {
                let id = x.atom_info().unwrap().id();

                // clone and leak a reference for returned atom
                let d = Rc::from_raw(x);
                let d2 = d.clone();
                let _ = Rc::into_raw(d);
                let _ = Rc::into_raw(d2);

                Atom(id)
            })
        })
    }

    /// free an non-const atom, descrease its ref count.
    /// when the atom has no more user reference (refcount==2 in this case),
    /// the atom desc will be unregistered and dropped.
    pub(super) fn free(aid: u32) {
        debug_assert!(aid >= JS_ATOM_INIT_COUNT);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            let idx = aid as usize - JS_ATOM_INIT_COUNT as usize;

            if let Some(p) = reg.atom_slots.get(idx).and_then(|x| unsafe { x.as_ref() }) {
                let d = unsafe { Rc::from_raw(p) };

                if Rc::strong_count(&d) == 2 {
                    // description strong ref count is now: 1 user ref + 1 desc ref,
                    // that means the last atom was free'd.
                    // we should drop the atom desc
                    reg.atom_slots[idx] = std::ptr::null_mut();

                    let hash = d.hash();
                    let mut ids = reg.atom_hash_lut.remove(&hash).unwrap();
                    debug_assert!(!ids.is_empty());

                    if ids.len() == 1 {
                        debug_assert_eq!(u16::from(ids[0]), aid as u16);
                    } else {
                        let i = ids.iter().position(|&x| x.get() == aid as u16).unwrap();
                        ids.swap_remove(i);

                        reg.atom_hash_lut.insert(hash, ids);
                    }
                }

                // TODO: to decrease memory usage
                // let rt=d.rt();
                // rt.mem_usage.dec(..)
            }
        });
    }

    /// to drop non-const atoms under given runtime
    pub(crate) fn purge_for_rt(rt: &JSRuntime) {
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            let mut removed = Vec::<(u32, u32)>::new();

            for i in 0..reg.atom_slots.len() {
                if let Some(d) = unsafe { reg.atom_slots.get_unchecked(i).as_ref() }
                    && std::ptr::eq(d.rt_ptr(), rt)
                {
                    let d = unsafe { Rc::from_raw(d) };
                    reg.atom_slots[i] = std::ptr::null_mut();

                    // remove the desc
                    let hash = d.hash();
                    removed.push((i as u32 + JS_ATOM_INIT_COUNT, hash));
                }
            }

            for (atom_id, hash) in removed {
                let mut ids = reg.atom_hash_lut.remove(&hash).unwrap();
                if ids.len() == 1 {
                    debug_assert_eq!(ids[0].get() as u32, atom_id);
                } else {
                    ids.retain(|id| id.get() as u32 != atom_id);
                    reg.atom_hash_lut.insert(hash, ids);
                }
            }
        });
    }

    /// clear all user atoms under all rt(s)
    pub(crate) fn clear() {
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();

            #[cfg(debug_assertions)]
            for (i, p) in reg.atom_slots.drain(..).enumerate() {
                unsafe {
                    if let Some(s) = p.as_ref() {
                        let d = Rc::from_raw(s);

                        let id = i as u32 + JS_ATOM_INIT_COUNT;
                        println!("[atom registry clear] Atom({id}): {d:?}");
                    }
                }
            }
            #[cfg(not(debug_assertions))]
            for p in reg.atom_slots.drain(..) {
                unsafe {
                    if let Some(s) = p.as_ref() {
                        let _ = Rc::from_raw(s);
                    }
                }
            }

            // ensure lut contains only const atoms
            let mut remove = Vec::<u32>::with_capacity(reg.atom_hash_lut.len());
            for (k, ids) in reg.atom_hash_lut.iter_mut() {
                ids.retain(|x| x.get() < JS_ATOM_INIT_COUNT as u16);
                if ids.is_empty() {
                    remove.push(*k);
                }
            }
            for k in remove {
                reg.atom_hash_lut.remove(&k);
            }
        });
    }
}

impl Atom {
    #[inline(always)]
    pub(crate) fn from_string_as_is(str: JSString, type_hint: AtomTypeHint) -> Result<Self, i32> {
        AtomRegistry::register(str, type_hint)
    }

    /// make atom from given JSString.
    /// Note: caller should free the result atom.
    pub fn from_string(str: JSString, type_hint: AtomTypeHint) -> Result<Self, i32> {
        if let Some(i) = str.content().parse_u32()
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            AtomRegistry::register(str, type_hint)
        }
    }

    /// make atom from `str`
    pub fn from_str(rt: &JSRuntime, str: &str, type_hint: AtomTypeHint) -> Result<Self, i32> {
        if let Some(i) = parse_u32(str)
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            Self::from_string(JSString::from_str(rt, str)?, type_hint)
        }
    }

    pub fn from_i64(rt: &JSRuntime, n: i64) -> Result<Self, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&n) {
            Ok(Self::new_small_int(n as u32))
        } else {
            Self::from_string(
                JSString::from_utf8(rt, format!("{n}").into_bytes())?,
                AtomTypeHint::String,
            )
        }
    }

    /// Make builtin const atom
    #[inline(always)]
    pub(crate) fn new_const(n: AtomRef) -> Self {
        debug_assert!(n.is_const());
        Self(n.id())
    }

    /// Make an small integral atom.
    /// Small integral atom is primitive, and do not need to register.
    #[inline(always)]
    pub(crate) fn new_small_int(n: u32) -> Self {
        debug_assert!(n <= JS_ATOM_MAX_INT);
        Self(n | JS_ATOM_TAG_INT)
    }

    /// create new or reuse existing atom for `str`.
    pub fn new(rt: &JSRuntime, str: &str) -> Result<Self, i32> {
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
    #[inline(always)]
    pub fn find(rt: &JSRuntime, str: &str, atom_type: AtomTypeHint) -> Option<Self> {
        AtomRegistry::find(rt, str, atom_type)
    }

    /// calc atom hash for descriptor and type
    pub(crate) fn calc_atom_hash(str: &StringContent, atom_type: AtomTypeHint) -> u32 {
        let mut h = atom_type as u32;
        if !str.is_empty() && !str.is_ident() {
            for (_, c) in str.iter() {
                h = hash_u32_from_base(h, c.to_u32());
            }
        }
        h
    }

    /// Safety: will panic if atom id does not exists
    pub(crate) fn free(id: u32) {
        if !is_const_atom(id) {
            AtomRegistry::free(id);
        }
    }
}

impl AtomRef {
    /// Make an small integral atom.
    /// Small integral atom is primitive, and do not need to register.
    #[inline(always)]
    pub(crate) fn new_small_int(n: u32) -> Self {
        debug_assert!(n <= JS_ATOM_MAX_INT);
        Self::from(n | JS_ATOM_TAG_INT)
    }

    #[inline(always)]
    pub(crate) const fn from(n: u32) -> Self {
        Self(n)
    }
}

/// calc hash for str when using as an atom descriptor
#[inline(always)]
fn calc_atom_hash_str(str: &str, atom_type: AtomTypeHint) -> u32 {
    let mut h = atom_type as u32;
    for c in str.encode_utf16() {
        h = hash_u32_from_base(h, c as u32);
    }
    h
}
