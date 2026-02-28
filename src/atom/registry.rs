// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::CStr,
    num::NonZeroU16,
};

use gc_lite::{GcLocal, GcNode};

use crate::{
    RuntimeImpl,
    atom::{is_const_atom, is_small_int_atom},
    number::parse_u32,
    runtime::RuntimePtr,
    string::{LocalString, StringData, StringImpl, StringRef},
    utils::hash_u32_from_base,
};

use super::{
    ATOM_CONST_CAPTIONS, ATOM_CONST_COUNT, ATOM_USER_BASE_ID, Atom, AtomInfo, AtomKind,
    JS_ATOM_HASH_PRIVATE, JS_ATOM_HASH_SYMBOL, JS_ATOM_MAX_INT, JS_ATOM_TAG_INT,
};

pub(crate) struct AtomRegistry {
    /// const atom descriptor slots.
    /// staring from index 0. #0 is for atom 1; #1 is for atom 2; ...
    const_atoms: Vec<StringRef>,

    /// user atom descriptor slots. None means the slot is unused.
    user_atoms: Vec<Option<StringRef>>,

    /// atom hash lookup table, is dict { atom_hash: [atom_id] },
    /// slot index = atom_id - 1
    hash_lut: HashMap<u32, Vec<u16>>,
}

impl AtomRegistry {
    /// create atom registry, setup with const atoms.
    pub(crate) fn new() -> Self {
        Self {
            const_atoms: Vec::new(),
            user_atoms: Vec::new(),
            hash_lut: HashMap::new(),
        }
    }
}

impl RuntimeImpl {
    /// init atom regisry: load const atoms
    pub(crate) fn init_const_atoms(&mut self) {
        debug_assert!(
            self.atom_registry.const_atoms.is_empty(),
            "must init atom registry only once"
        );

        let mut rt_ptr = unsafe { RuntimePtr::from_raw(self) };
        let root_scope = self.gc_part_hot;

        self.atom_registry
            .const_atoms
            .reserve(ATOM_CONST_COUNT as usize);

        self.atom_registry
            .hash_lut
            .reserve(ATOM_CONST_COUNT as usize + 64);

        let lst = &mut self.atom_registry.const_atoms;
        let lut = &mut self.atom_registry.hash_lut;

        let mut text = ATOM_CONST_CAPTIONS.as_ptr();

        for idx in 0..ATOM_CONST_COUNT {
            debug_assert_eq!(idx, lst.len() as u32);

            let atom_id = idx + 1;

            let mut atom_kind = {
                #[cfg(feature = "builtin_symbol")]
                if atom_id == Atom::CONST_Private_brand.id() {
                    AtomKind::Private
                } else if atom_id >= Atom::CONST_Symbol_toPrimitive.id() {
                    AtomKind::Symbol
                } else {
                    AtomKind::String
                }

                #[cfg(not(feature = "builtin_symbol"))]
                AtomKind::String
            };

            let cstr = unsafe { CStr::from_ptr(text.cast()) };
            let str = cstr.to_str().unwrap();
            let hash: u32 = {
                #[cfg(feature = "builtin_symbol")]
                match atom_kind {
                    AtomKind::String | AtomKind::GlobalSymbol => {
                        Atom::calc_atom_str_hash(str, atom_kind)
                    }
                    AtomKind::Symbol | AtomKind::Private => {
                        if atom_kind == AtomKind::Symbol {
                            JS_ATOM_HASH_SYMBOL
                        } else {
                            atom_kind = AtomKind::Symbol;
                            JS_ATOM_HASH_PRIVATE
                        }
                    }
                }

                #[cfg(not(feature = "builtin_symbol"))]
                Atom::calc_atom_str_hash(str, atom_kind)
            };

            let str = {
                let ss = StringImpl::new(
                    rt_ptr,
                    StringData::from_static(str),
                    Some(AtomInfo::new(
                        unsafe { NonZeroU16::new_unchecked(atom_id as u16) },
                        atom_kind,
                        hash,
                    )),
                );

                // const atom string is root gc node.
                // TODO: alloc const atom string in a dedicated top-level scope.
                let s = rt_ptr
                    .gc_heap_mut()
                    .with_new_scope(root_scope, |gc| gc.alloc_root(ss).unwrap());

                // let s = rt_ptr.gc_alloc_in(root_scope, ss).unwrap();
                // rt_ptr.gc_heap_mut().set_root(s, true);

                StringRef::from(s)
            };
            lst.push(str);

            match lut.entry(hash) {
                Entry::Vacant(kv) => {
                    kv.insert(vec![atom_id as u16]);
                }
                Entry::Occupied(kv) => {
                    kv.into_mut().push(atom_id as u16);
                }
            }

            text = unsafe { text.add(cstr.count_bytes() + 1) };
        }
    }

    /// register user atom with string desciptor.
    /// (in case of creating an anonymous symbol, str desc should be ident-type-string)
    /// Same `str` will return the matched existing atom.
    fn register_atom(&mut self, str: StringRef, mut atom_kind: AtomKind) -> Result<Atom, i32> {
        // match for existing atoms, don't register twice for same content.
        let (mut desc_str, hash): (LocalString, u32) = match atom_kind {
            #[cfg(feature = "builtin_symbol")]
            AtomKind::String | AtomKind::GlobalSymbol => {
                debug_assert!(!str.content().is_ident());

                let ss = if let Some(a) = str.atom_info() {
                    // already registerd as atom
                    if a.kind() == atom_kind {
                        return Ok(Atom(a.id()));
                    }

                    // make new string with same content under given runtime, without atom info
                    str.copy_to_new(self.into())?
                } else {
                    StringImpl::to_local(&str)
                };

                let hash = Atom::calc_atom_hash(ss.content(), atom_kind);
                let a_reg = &mut self.atom_registry;

                // match registered atoms for hash, type and content
                match a_reg.hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for &id in kv.get() {
                            let atom_id = id as u32;
                            if is_const_atom(atom_id) {
                                if a_reg.const_atoms[atom_id as usize - 1]
                                    .atom_info()
                                    .unwrap()
                                    .kind()
                                    == atom_kind
                                {
                                    return Ok(Atom(atom_id));
                                }
                            } else {
                                let ds = a_reg.user_atoms
                                    [atom_id as usize - ATOM_USER_BASE_ID as usize]
                                    .as_ref()
                                    .unwrap();
                                let at = ds.atom_info().unwrap();
                                if at.kind() == atom_kind && ss.eq(ds) {
                                    // found existing atom, reuse it.
                                    return Ok(Atom(at.id()));
                                }
                            }
                        }
                    }
                }

                (ss, hash)
            }

            #[cfg(not(feature = "builtin_symbol"))]
            AtomKind::String => {
                debug_assert!(!str.content().is_ident());

                let ss = if let Some(at) = str.atom_info() {
                    // already registerd as atom
                    if at.kind() == atom_kind {
                        return Ok(Atom(at.id()));
                    }

                    // make new string with same content under given runtime,
                    // without atom info
                    str.copy_to_new(self.into())?
                } else {
                    str
                };

                let hash = Atom::calc_atom_hash(ss.content(), atom_kind);
                let a_reg = &mut self.atom_registry;

                // match registered atoms for hash, type and content
                match a_reg.atom_hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for aid in kv.get() {
                            let id = aid.get() as u32;
                            if is_const_atom(id) {
                                if a_reg.const_atoms[id as usize].atom_info().kind() == atom_kind {
                                    return Ok(Atom(id));
                                }
                            } else {
                                let d = a_reg.user_atoms[id as usize - ATOM_USER_BASE_ID as usize]
                                    .as_ref()
                                    .unwrap();
                                let at = d.str.atom_info().unwrap();
                                if at.kind() == atom_kind && d.str == ss {
                                    // found existing atom, reuse it.
                                    return Ok(Atom(at.id()));
                                }
                            }
                        }
                    }
                }

                (ss, hash)
            }

            #[cfg(feature = "builtin_symbol")]
            AtomKind::Symbol | AtomKind::Private => {
                // these atom types are distinct, even they have same string content.
                let ss: LocalString = if str.content().is_ident() {
                    debug_assert_eq!(
                        atom_kind,
                        AtomKind::Symbol,
                        "ident string type is only for anonymous symbol"
                    );
                    StringImpl::to_local(&str)
                } else if str.atom_info().is_some() {
                    str.copy_to_new(self.into())? // clone content as a new jsstring
                } else {
                    StringImpl::to_local(&str)
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

        desc_str = desc_str.copy_to_new(self.into())?;

        if self.atom_registry.hash_lut.len() == self.atom_registry.hash_lut.capacity() {
            self.atom_registry
                .hash_lut
                .try_reserve(16)
                .map_err(|_| -1)?;
        }

        // find a free slot
        let (idx, atom_id) = if let Some(i) = self
            .atom_registry
            .user_atoms
            .iter()
            .position(|p| p.is_none())
        {
            (Some(i), i as u16 + ATOM_USER_BASE_ID as u16)
        } else {
            if self.atom_registry.user_atoms.len() == self.atom_registry.user_atoms.capacity() {
                self.atom_registry
                    .user_atoms
                    .try_reserve(16)
                    .map_err(|_| -1)?;
            }
            (
                None,
                self.atom_registry.user_atoms.len() as u16 + ATOM_USER_BASE_ID as u16,
            )
        };

        match self.atom_registry.hash_lut.entry(hash) {
            Entry::Vacant(kv) => {
                kv.insert(vec![atom_id]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(atom_id);
            }
        }

        desc_str.gc_ref().set_atom_info(AtomInfo::new(
            unsafe { NonZeroU16::new_unchecked(atom_id) },
            atom_kind,
            hash,
        ));

        let xref = self.gc_part_id();
        self.gc_heap_mut().set_xref(xref, desc_str.gc_head_ptr());

        if let Some(i) = idx {
            debug_assert_eq!(i, atom_id as usize - ATOM_CONST_COUNT as usize - 1);
            debug_assert!(
                self.atom_registry.user_atoms[i].is_none(),
                "atom slot {i} is in use"
            );
            self.atom_registry.user_atoms[i].replace(desc_str.gc_ref().into());
        } else {
            self.atom_registry
                .user_atoms
                .push(Some(desc_str.gc_ref().into()));

            debug_assert_eq!(
                self.atom_registry.user_atoms.len(),
                atom_id as usize - ATOM_CONST_COUNT as usize
            );
        }

        Ok(Atom(atom_id as u32))
    }

    /// remove atom from registry.
    /// this is called when the string descriptor is being dropped,
    /// so don't trigger string drop again.
    pub(crate) fn remove_atom(&mut self, atom_info: &AtomInfo) {
        let atom_id = atom_info.id();
        let atom_hash = atom_info.hash();
        debug_assert!(!is_const_atom(atom_id) && !is_small_int_atom(atom_id));

        let a_reg = &mut self.atom_registry;

        // Note: atom_id associated descriptor string is now an invalid gc pointer.
        if let Some(slot) = a_reg
            .user_atoms
            .get_mut(atom_id as usize - ATOM_USER_BASE_ID as usize)
            && slot.take().is_some()
        {
            let purge = if let Some(ids) = a_reg.hash_lut.get_mut(&atom_hash) {
                if ids.len() == 1 {
                    debug_assert_eq!(ids[0] as u32, atom_id);
                    true
                } else {
                    let i = {
                        #[cfg(debug_assertions)]
                        {
                            ids.iter().position(|&x| x == atom_id as u16).unwrap()
                        }
                        #[cfg(not(debug_assertions))]
                        unsafe {
                            ids.iter()
                                .position(|&x| x == atom_id as u16)
                                .unwrap_unchecked()
                        }
                    };

                    ids.swap_remove(i);
                    false
                }
            } else {
                false
            };

            if purge {
                a_reg.hash_lut.remove(&atom_hash);
            }
        }
    }

    /// get atom descriptor string by atom id.
    /// returns None if atom id is invalid.
    pub(super) fn atom_desc_string(&self, atom_id: u32) -> Option<&StringRef> {
        debug_assert!(
            atom_id != 0 && !is_small_int_atom(atom_id),
            "not for small-int-atom or null-atom"
        );

        if is_const_atom(atom_id) {
            self.atom_registry.const_atoms.get(atom_id as usize - 1)
        } else {
            self.atom_registry
                .user_atoms
                .get(atom_id as usize - ATOM_USER_BASE_ID as usize)
                .and_then(|d| d.as_ref())
        }
    }
}

impl Atom {
    #[inline(always)]
    pub(crate) fn from_string_as_is(
        mut rt: RuntimePtr,
        str: StringRef,
        kind: AtomKind,
    ) -> Result<Self, i32> {
        rt.register_atom(str, kind)
    }

    pub(crate) fn from_string(
        mut rt: RuntimePtr,
        str: StringRef,
        kind: AtomKind,
    ) -> Result<Self, i32> {
        if str.is_empty() {
            Ok(Atom::CONST_empty_string)
        } else if let Some(i) = str.parse_u32()
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            rt.register_atom(str, kind)
        }
    }

    /// make atom from `str`. if `str` is a safe-integer representation, create int-atom instead.
    pub(crate) fn from_str(rt: RuntimePtr, str: &str, kind: AtomKind) -> Result<Self, i32> {
        if str.is_empty() {
            Ok(Atom::CONST_empty_string)
        } else if let Some(i) = parse_u32(str)
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            let s = StringRef::create_str(rt, str)?;
            Self::from_string(rt, (&s).into(), kind)
        }
    }

    pub(crate) fn from_i64(rt: RuntimePtr, n: i64) -> Result<Self, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&n) {
            Ok(Self::new_small_int(n as u32))
        } else {
            let s = StringRef::create_utf8(rt, format!("{n}").into_bytes())?;
            Self::from_string(rt, (&s).into(), AtomKind::String)
        }
    }

    /// Create const atom
    #[inline(always)]
    pub const fn new_const(id: u32) -> Self {
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
    pub(crate) fn new(rt: RuntimePtr, str: &str) -> Result<Self, i32> {
        // if str.is_empty() || !str.chars().next().unwrap().is_ascii_digit() {
        //     // find atom in registry
        //     if let Some(atom) = Self::find(str, AtomKind::String) {
        //         return Ok(atom);
        //     }
        // }
        Self::from_str(rt, str, AtomKind::String)
    }

    /// calc atom hash with str and kind
    pub(crate) fn calc_atom_hash(str: &StringData, kind: AtomKind) -> u32 {
        let mut h = kind as u32;
        if !str.is_empty() && !str.is_ident() {
            for (_, c) in str.chars() {
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
}
