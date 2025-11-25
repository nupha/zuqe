use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::CStr,
    num::NonZeroU16,
};

use crate::{
    atom::{is_const_atom, is_small_int_atom},
    number::parse_u32,
    resource::LocalResource,
    runtime::{RuntimeImpl, RuntimePtr},
    string::{StringData, StringRef},
    utils::hash_u32_from_base,
};

use super::{
    ATOM_CONST_CAPTIONS, ATOM_CONST_COUNT, Atom, AtomDesc, AtomInfo, AtomKind,
    JS_ATOM_HASH_PRIVATE, JS_ATOM_HASH_SYMBOL, JS_ATOM_MAX_INT, JS_ATOM_TAG_INT,
};

pub(crate) struct AtomRegistry {
    /// const atom description slots list.
    /// index #0 is reserved but unused;
    /// staring from index 1, #1 is for atom 1; #2 is for atom 2; ...
    const_atoms: Vec<AtomDesc>,

    /// list of user atom description. None means the slot is unused.
    user_atoms: Vec<Option<AtomDesc>>,

    /// StringImpl hash to atom vec dict.
    /// Value is list of atom indecies because StringImpl hash may have collison.
    atom_hash_lut: HashMap<u32, Vec<NonZeroU16>>,
}

impl Drop for AtomRegistry {
    fn drop(&mut self) {
        self.clear_user_atoms();
        self.clear_const_atoms();
        self.atom_hash_lut.clear();
    }
}

impl AtomRegistry {
    /// create atom registry, setup with const atoms.
    pub(crate) fn new() -> Self {
        let (consts, lut) = load_const_atoms();
        AtomRegistry {
            const_atoms: consts,
            user_atoms: Vec::with_capacity(128),
            atom_hash_lut: lut,
        }
    }

    /// register a user atom with string desciption.
    /// (in case of creating an anonymous symbol, desc should be an ident-type-string)
    /// Same `str` will return the matched existing atom.
    fn register_internal(
        &mut self,
        rt: RuntimePtr,
        str: StringRef,
        mut atom_kind: AtomKind,
    ) -> Result<Atom, i32> {
        // match for existing atoms, don't register twice for same content.
        let (mut atom_str, hash): (StringRef, u32) = match atom_kind {
            AtomKind::String | AtomKind::GlobalSymbol => {
                debug_assert!(!str.content().is_ident());

                let ss = if let Some(at) = str.atom_info() {
                    // already registerd as atom
                    if at.kind() == atom_kind
                        && (is_const_atom(at.id()) || str.rt().is_some_and(|r| r == rt))
                    {
                        return Ok(Atom(at.id()));
                    }

                    // make new string with same content under given runtime,
                    // without atom info
                    str.copy_to_new(rt)?
                } else {
                    str
                };

                let hash = Atom::calc_atom_hash(ss.content(), atom_kind);

                // match registered atoms for hash, type and content
                match self.atom_hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for aid in kv.get() {
                            let id = aid.get() as u32;
                            if is_const_atom(id) {
                                if self.const_atoms[id as usize].atom_info().kind() == atom_kind {
                                    return Ok(Atom(id));
                                }
                            } else {
                                let d = self.user_atoms[id as usize - ATOM_CONST_COUNT as usize]
                                    .as_ref()
                                    .unwrap();
                                let at = d.str.atom_info().unwrap();
                                if at.kind() == atom_kind
                                    && d.str == ss
                                    && d.str.rt().unwrap() == rt
                                {
                                    // found existing atom, reuse it.
                                    return Ok(Atom(at.id()));
                                }
                            }
                        }
                    }
                }

                (ss, hash)
            }
            AtomKind::Symbol | AtomKind::Private => {
                // these atom types are distinct, even they have same string content.
                let ss = if str.content().is_ident() {
                    debug_assert_eq!(
                        atom_kind,
                        AtomKind::Symbol,
                        "ident string type is only for anonymous symbol"
                    );
                    str
                } else if str.atom_info().is_some() {
                    // clone content as a new jsstring
                    str.copy_to_new(rt)?
                } else {
                    str
                };

                let hash = if atom_kind == AtomKind::Symbol {
                    JS_ATOM_HASH_SYMBOL
                } else {
                    atom_kind = AtomKind::Symbol;
                    JS_ATOM_HASH_PRIVATE
                };

                (ss, hash)
            }
        };

        if atom_str.rt().is_none() {
            atom_str = atom_str.copy_to_new(rt)?
        }

        if self.atom_hash_lut.len() == self.atom_hash_lut.capacity() {
            self.atom_hash_lut.try_reserve(16).map_err(|_| -1)?;
        }

        // find a free slot
        let (idx, atom_id) = if let Some(i) = self.user_atoms.iter().position(|p| p.is_none()) {
            (
                Some(i),
                NonZeroU16::new(i as u16 + ATOM_CONST_COUNT as u16).unwrap(),
            )
        } else {
            if self.user_atoms.len() == self.user_atoms.capacity() {
                self.user_atoms.try_reserve(16).map_err(|_| -1)?;
            }
            (
                None,
                NonZeroU16::new(self.user_atoms.len() as u16 + ATOM_CONST_COUNT as u16).unwrap(),
            )
        };

        match self.atom_hash_lut.entry(hash) {
            Entry::Vacant(kv) => {
                kv.insert(vec![atom_id]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(atom_id);
            }
        }

        atom_str.set_atom_info(AtomInfo::new(atom_id, atom_kind, hash));

        let d = AtomDesc { str: atom_str };

        if let Some(i) = idx {
            debug_assert_eq!(i, atom_id.get() as usize - ATOM_CONST_COUNT as usize);
            debug_assert!(self.user_atoms[i].is_none(), "atom slot {i} is in use");
            self.user_atoms[i].replace(d);
        } else {
            self.user_atoms.push(Some(d));
            debug_assert_eq!(
                self.user_atoms.len() - 1,
                atom_id.get() as usize - ATOM_CONST_COUNT as usize
            );
        }

        Ok(Atom(atom_id.get() as u32))
    }

    /// register a user atom with description.
    ///
    /// Safety: will panic if no runtime is initialized
    fn register(rt: RuntimePtr, str: StringRef, kind: AtomKind) -> Result<Atom, i32> {
        LocalResource::with_atoms_mut(|reg| reg.register_internal(rt, str, kind))
    }

    /// unregister an atom by id
    /// this is called when the corresponding string is being dropped
    fn unregister_internal(&mut self, atom_id: u32) {
        debug_assert!(!is_const_atom(atom_id) && !is_small_int_atom(atom_id));

        if let Some(slot) = self
            .user_atoms
            .get_mut(atom_id as usize - ATOM_CONST_COUNT as usize)
            && let Some(desc) = slot.take()
        {
            // remove from hash lookup table
            let hash = desc.atom_info().hash();
            if let Some(mut ids) = self.atom_hash_lut.remove(&hash)
                && let Some(pos) = ids.iter().position(|x| x.get() == atom_id as u16)
            {
                ids.swap_remove(pos);
                if !ids.is_empty() {
                    self.atom_hash_lut.insert(hash, ids);
                }
            }
        }
    }

    pub fn unregister(id: u32) {
        LocalResource::with_atoms_mut(|reg| {
            reg.unregister_internal(id);
        });
    }

    /// run closure with atom description suggested by atom id.
    /// returns None if atom id was not found.
    #[inline(always)]
    pub(super) fn with_desc<R>(atom_id: u32, f: impl FnOnce(&AtomDesc) -> R) -> Option<R> {
        LocalResource::with_atoms(|reg| reg.with_desc_internal(atom_id, f))
    }

    fn with_desc_internal<R>(&self, atom_id: u32, f: impl FnOnce(&AtomDesc) -> R) -> Option<R> {
        debug_assert!(atom_id != 0 && !is_small_int_atom(atom_id));

        if is_const_atom(atom_id) {
            // const atom
            Some(f(&self.const_atoms[atom_id as usize]))
        } else {
            // user atom
            if let Some(Some(d)) = self
                .user_atoms
                .get(atom_id as usize - ATOM_CONST_COUNT as usize)
            {
                Some(f(d))
            } else {
                None
            }
        }
    }

    #[inline]
    pub(super) fn with_string<R>(atom_id: u32, f: impl FnOnce(&StringRef) -> R) -> Option<R> {
        debug_assert!(atom_id != 0);
        debug_assert!(!is_small_int_atom(atom_id));
        LocalResource::with_atoms(|reg| reg.with_desc_internal(atom_id, |d| f(&d.str)))
    }

    /// find atom desc by str and type
    fn find_by_str_internal(&self, str: &str, atom_kind: AtomKind) -> Option<AtomDesc> {
        self.atom_hash_lut
            .get(&calc_atom_hash_str(str, atom_kind))
            .and_then(|ids| {
                ids.iter().find_map(|id| {
                    let id = id.get() as u32;
                    if id < ATOM_CONST_COUNT {
                        // const atom
                        Some(self.const_atoms[id as usize].clone())
                    } else {
                        // user atom
                        let d = self
                            .user_atoms
                            .get(id as usize - ATOM_CONST_COUNT as usize)
                            .cloned()
                            .flatten()
                            .unwrap();

                        if d.atom_info().kind() == atom_kind && d.str.content().eq_str(str) {
                            Some(d)
                        } else {
                            None
                        }
                    }
                })
            })
    }

    /// clear all const atoms
    pub(crate) fn clear_const_atoms(&mut self) {
        self.const_atoms.clear();
    }

    /// clear all user atoms
    pub(crate) fn clear_user_atoms(&mut self) {
        self.user_atoms.clear();

        // purge lut to contain only const atoms
        let mut remove = Vec::<u32>::with_capacity(self.atom_hash_lut.len());
        for (k, ids) in self.atom_hash_lut.iter_mut() {
            ids.retain(|x| x.get() < ATOM_CONST_COUNT as u16);
            if ids.is_empty() {
                remove.push(*k);
            }
        }
        for k in remove {
            self.atom_hash_lut.remove(&k);
        }
    }
}

/// build const atoms list and hash lut
fn load_const_atoms() -> (Vec<AtomDesc>, HashMap<u32, Vec<NonZeroU16>>) {
    let mut lst = Vec::<AtomDesc>::with_capacity(ATOM_CONST_COUNT as usize + 1);
    let mut lut = HashMap::<u32, Vec<NonZeroU16>>::with_capacity(ATOM_CONST_COUNT as usize);

    // slot #0 is unused
    lst.push(AtomDesc {
        str: StringRef::from_static(None, "", None).unwrap(),
    });

    let mut p = ATOM_CONST_CAPTIONS.as_ptr();

    for i in 1..ATOM_CONST_COUNT {
        let mut atom_kind = if i == Atom::CONST_Private_brand.id() {
            AtomKind::Private
        } else if i >= Atom::CONST_Symbol_toPrimitive.id() {
            AtomKind::Symbol
        } else {
            AtomKind::String
        };

        let cstr = unsafe { CStr::from_ptr(p.cast()) };
        let str = cstr.to_str().unwrap();
        let hash: u32 = match atom_kind {
            AtomKind::String | AtomKind::GlobalSymbol => Atom::calc_atom_str_hash(str, atom_kind),
            AtomKind::Symbol | AtomKind::Private => {
                if atom_kind == AtomKind::Symbol {
                    JS_ATOM_HASH_SYMBOL
                } else {
                    atom_kind = AtomKind::Symbol;
                    JS_ATOM_HASH_PRIVATE
                }
            }
        };

        let id = unsafe { NonZeroU16::new_unchecked(lst.len() as u16) };

        let str =
            StringRef::from_static(None, str, Some(AtomInfo::new(id, atom_kind, hash))).unwrap();
        lst.push(AtomDesc { str });

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

impl Atom {
    #[inline(always)]
    pub(crate) fn from_string_as_is(
        rt: RuntimePtr,
        str: StringRef,
        kind: AtomKind,
    ) -> Result<Self, i32> {
        AtomRegistry::register(rt, str, kind)
    }

    pub(crate) fn from_string(rt: RuntimePtr, str: StringRef, kind: AtomKind) -> Result<Self, i32> {
        if str.is_empty() {
            Ok(Atom::CONST_empty_string)
        } else if let Some(i) = str.parse_u32()
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            AtomRegistry::register(rt, str, kind)
        }
    }

    /// make atom from `str`. if `str` is a safe-integer representation, create int-atom instead.
    pub fn from_str(rt: &RuntimeImpl, str: &str, kind: AtomKind) -> Result<Self, i32> {
        if str.is_empty() {
            Ok(Atom::CONST_empty_string)
        } else if let Some(i) = parse_u32(str)
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            Self::from_string(rt.into(), StringRef::from_str(rt, str)?, kind)
        }
    }

    pub fn from_i64(rt: &RuntimeImpl, n: i64) -> Result<Self, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&n) {
            Ok(Self::new_small_int(n as u32))
        } else {
            Self::from_string(
                rt.into(),
                StringRef::from_utf8(rt, format!("{n}").into_bytes())?,
                AtomKind::String,
            )
        }
    }

    /// Create const atom
    #[inline(always)]
    pub fn new_const(id: u32) -> Self {
        debug_assert!(is_const_atom(id));
        Self(id)
    }

    /// Make an small integral atom.
    /// Small integral atom is primitive, and do not need to register.
    #[inline(always)]
    pub(crate) fn new_small_int(n: u32) -> Self {
        debug_assert!(n <= JS_ATOM_MAX_INT);
        Self(n | JS_ATOM_TAG_INT)
    }

    /// create new or reuse existing atom for `str`.
    #[inline(always)]
    pub fn new(rt: &RuntimeImpl, str: &str) -> Result<Self, i32> {
        // if str.is_empty() || !str.chars().next().unwrap().is_ascii_digit() {
        //     // find atom in registry
        //     if let Some(atom) = Self::find(str, AtomKind::String) {
        //         return Ok(atom);
        //     }
        // }

        Self::from_str(rt, str, AtomKind::String)
    }

    /// find atom by str
    #[inline(always)]
    pub fn find_by_str(str: &str, kind: AtomKind) -> Option<Self> {
        LocalResource::with_atoms(|reg| {
            reg.find_by_str_internal(str, kind)
                .map(|d| Atom(d.atom_info().id()))
        })
    }

    /// calc atom hash with str and kind
    pub(crate) fn calc_atom_hash(str: &StringData, kind: AtomKind) -> u32 {
        let mut h = kind as u32;
        if !str.is_empty() && !str.is_ident() {
            for (_, c) in str.iter() {
                h = hash_u32_from_base(h, c.to_u32());
            }
        }
        h
    }

    /// calc atom hash for str and type.
    pub(super) fn calc_atom_str_hash(str: &str, kind: AtomKind) -> u32 {
        let mut h = kind as u32;
        if !str.is_empty() {
            for c in str.chars() {
                h = hash_u32_from_base(h, c as _);
            }
        }
        h
    }

    /// unregister an atom by id
    #[inline(always)]
    pub fn unregister(id: u32) {
        AtomRegistry::unregister(id);
    }
}

/// calc hash for str when using as an atom descriptor
#[inline(always)]
fn calc_atom_hash_str(str: &str, atom_kind: AtomKind) -> u32 {
    let mut h = atom_kind as u32;
    for c in str.encode_utf16() {
        h = hash_u32_from_base(h, c as u32);
    }
    h
}
