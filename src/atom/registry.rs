use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::CStr,
    num::NonZeroU16,
};

use gc_lite::GcRef;

use crate::{
    RuntimeImpl,
    atom::{is_const_atom, is_small_int_atom},
    number::parse_u32,
    runtime::RuntimePtr,
    string::{StringData, StringImpl, StringRef},
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
    pub(super) const_atoms: Vec<AtomDesc>,

    /// list of user atom description. None means the slot is unused.
    pub(super) user_atoms: Vec<Option<AtomDesc>>,

    /// string hash to atom ids dict.
    /// Value is list of atom indecies because string hash may have collison.
    pub(super) atom_hash_lut: HashMap<u32, Vec<NonZeroU16>>,
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
        Self {
            const_atoms: Vec::new(),
            user_atoms: Vec::with_capacity(128),
            atom_hash_lut: HashMap::new(),
        }
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

// /// build const atoms list and hash lut
// fn load_const_atoms(heap: &mut GcHeap) -> (Vec<AtomDesc>, HashMap<u32, Vec<NonZeroU16>>) {
//     let mut lst = Vec::<AtomDesc>::with_capacity(ATOM_CONST_COUNT as usize + 1);
//     let mut lut = HashMap::<u32, Vec<NonZeroU16>>::with_capacity(ATOM_CONST_COUNT as usize);

//     // slot #0 is unused
//     lst.push(AtomDesc {
//         str: StringRef::from_static(None, "", None).unwrap(),
//     });

//     let mut p = ATOM_CONST_CAPTIONS.as_ptr();

//     for i in 1..ATOM_CONST_COUNT {
//         let mut atom_kind = {
//             #[cfg(feature = "builtin_symbol")]
//             if i == Atom::CONST_Private_brand.id() {
//                 AtomKind::Private
//             } else if i >= Atom::CONST_Symbol_toPrimitive.id() {
//                 AtomKind::Symbol
//             } else {
//                 AtomKind::String
//             }

//             #[cfg(not(feature = "builtin_symbol"))]
//             AtomKind::String
//         };

//         let cstr = unsafe { CStr::from_ptr(p.cast()) };
//         let str = cstr.to_str().unwrap();
//         let hash: u32 = {
//             #[cfg(feature = "builtin_symbol")]
//             match atom_kind {
//                 AtomKind::String | AtomKind::GlobalSymbol => {
//                     Atom::calc_atom_str_hash(str, atom_kind)
//                 }
//                 AtomKind::Symbol | AtomKind::Private => {
//                     if atom_kind == AtomKind::Symbol {
//                         JS_ATOM_HASH_SYMBOL
//                     } else {
//                         atom_kind = AtomKind::Symbol;
//                         JS_ATOM_HASH_PRIVATE
//                     }
//                 }
//             }

//             #[cfg(not(feature = "builtin_symbol"))]
//             Atom::calc_atom_str_hash(str, atom_kind)
//         };

//         let id = unsafe { NonZeroU16::new_unchecked(lst.len() as u16) };
//         let atom_info = AtomInfo::new(id, atom_kind, hash);

//         // let str =
//         //     StringRef::from_static(None, str, Some(AtomInfo::new(id, atom_kind, hash))).unwrap();

//         let str = {
//             let ss = StringImpl::new(None, StringData::from_static(str), Some(atom_info));
//             heap.rt
//                 .gc_alloc(ss)
//                 .map_err(|_| {
//                     eprintln!("[O.o] alloc error");
//                     -1
//                 })
//                 .map(Self)
//         };

//         lst.push(AtomDesc { str });

//         match lut.entry(hash) {
//             Entry::Vacant(kv) => {
//                 kv.insert(vec![id]);
//             }
//             Entry::Occupied(kv) => {
//                 kv.into_mut().push(id);
//             }
//         }

//         p = unsafe { p.add(cstr.count_bytes() + 1) };
//     }

//     (lst, lut)
// }

impl RuntimeImpl {
    /// init atom regisry: load const atoms
    pub(crate) fn init_const_atoms(&mut self) {
        debug_assert!(
            self.atom_registry.const_atoms.is_empty(),
            "must init atom registry only once"
        );

        let mut rt_ptr = unsafe { RuntimePtr::from_raw(self) };
        let root_scope = self.gc_root_scope;

        self.atom_registry
            .const_atoms
            .reserve(ATOM_CONST_COUNT as usize + 1);

        self.atom_registry
            .atom_hash_lut
            .reserve(ATOM_CONST_COUNT as usize + 64);

        let lst = &mut self.atom_registry.const_atoms;
        let lut = &mut self.atom_registry.atom_hash_lut;

        // slot #0 is unused, and is not accessable.
        lst.push(AtomDesc {
            str: StringRef::from(GcRef::<StringImpl>::dangling()),
        });

        let mut text = ATOM_CONST_CAPTIONS.as_ptr();

        for i in 1..ATOM_CONST_COUNT {
            let mut atom_kind = {
                #[cfg(feature = "builtin_symbol")]
                if i == Atom::CONST_Private_brand.id() {
                    AtomKind::Private
                } else if i >= Atom::CONST_Symbol_toPrimitive.id() {
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

            let id = unsafe { NonZeroU16::new_unchecked(lst.len() as u16) };
            let atom_info = AtomInfo::new(id, atom_kind, hash);

            let str = {
                let ss =
                    StringImpl::new(Some(rt_ptr), StringData::from_static(str), Some(atom_info));
                let s = rt_ptr.gc_alloc_in(root_scope, ss).unwrap();
                // const atom string is root node.
                // TODO: alloc const atom string in a dedicated top-level scope.
                rt_ptr.gc_heap_mut().set_root(s, true);

                StringRef::from(s)
            };
            lst.push(AtomDesc { str });

            match lut.entry(hash) {
                Entry::Vacant(kv) => {
                    kv.insert(vec![id]);
                }
                Entry::Occupied(kv) => {
                    kv.into_mut().push(id);
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
        let (mut desc_str, hash): (StringRef, u32) = match atom_kind {
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
                                let d = a_reg.user_atoms[id as usize - ATOM_CONST_COUNT as usize]
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
                                let d = a_reg.user_atoms[id as usize - ATOM_CONST_COUNT as usize]
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
                let ss = if str.content().is_ident() {
                    debug_assert_eq!(
                        atom_kind,
                        AtomKind::Symbol,
                        "ident string type is only for anonymous symbol"
                    );
                    str
                } else if str.atom_info().is_some() {
                    // clone content as a new jsstring
                    str.copy_to_new(self.into())?
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

        if desc_str.rt().is_none() {
            desc_str = desc_str.copy_to_new(self.into())?
        }

        if self.atom_registry.atom_hash_lut.len() == self.atom_registry.atom_hash_lut.capacity() {
            self.atom_registry
                .atom_hash_lut
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
            (
                Some(i),
                NonZeroU16::new(i as u16 + ATOM_CONST_COUNT as u16).unwrap(),
            )
        } else {
            if self.atom_registry.user_atoms.len() == self.atom_registry.user_atoms.capacity() {
                self.atom_registry
                    .user_atoms
                    .try_reserve(16)
                    .map_err(|_| -1)?;
            }
            (
                None,
                NonZeroU16::new(
                    self.atom_registry.user_atoms.len() as u16 + ATOM_CONST_COUNT as u16,
                )
                .unwrap(),
            )
        };

        match self.atom_registry.atom_hash_lut.entry(hash) {
            Entry::Vacant(kv) => {
                kv.insert(vec![atom_id]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(atom_id);
            }
        }

        desc_str.set_atom_info(AtomInfo::new(atom_id, atom_kind, hash));

        let d = AtomDesc { str: desc_str };

        if let Some(i) = idx {
            debug_assert_eq!(i, atom_id.get() as usize - ATOM_CONST_COUNT as usize);
            debug_assert!(
                self.atom_registry.user_atoms[i].is_none(),
                "atom slot {i} is in use"
            );
            self.atom_registry.user_atoms[i].replace(d);
        } else {
            self.atom_registry.user_atoms.push(Some(d));
            debug_assert_eq!(
                self.atom_registry.user_atoms.len() - 1,
                atom_id.get() as usize - ATOM_CONST_COUNT as usize
            );
        }

        Ok(Atom(atom_id.get() as u32))
    }

    /// remove atom from registry.
    /// this is called when the string descriptor is being dropped,
    /// so don't trigger string drop again.
    pub(crate) fn remove_atom(&mut self, atom_info: &AtomInfo) {
        let atom_id = atom_info.id();
        let atom_hash = atom_info.hash();
        let a_reg = &mut self.atom_registry;

        debug_assert!(!is_const_atom(atom_id) && !is_small_int_atom(atom_id));

        // Note: AtomDesc for `atom_id` is now a invalid gc pointer.
        if let Some(slot) = a_reg
            .user_atoms
            .get_mut(atom_id as usize - ATOM_CONST_COUNT as usize)
            && slot.take().is_some()
        {
            let purge = if let Some(ids) = a_reg.atom_hash_lut.get_mut(&atom_hash) {
                if ids.len() == 1 {
                    debug_assert_eq!(ids[0].get() as u32, atom_id);
                    true
                } else {
                    #[cfg(debug_assertions)]
                    let i = ids.iter().position(|x| x.get() == atom_id as u16).unwrap();
                    #[cfg(not(debug_assertions))]
                    unsafe {
                        let i = ids
                            .iter()
                            .position(|x| x.get() == atom_id as u16)
                            .unwrap_unchecked();
                    }

                    ids.swap_remove(i);
                    false
                }
            } else {
                false
            };

            if purge {
                a_reg.atom_hash_lut.remove(&atom_hash);
            }
        }
    }

    /// get atom descriptor by atom id.
    /// returns None if atom id is invalid.
    pub(super) fn atom_desc(&self, atom_id: u32) -> Option<&AtomDesc> {
        debug_assert!(
            atom_id != 0 && !is_small_int_atom(atom_id),
            "not for small-int-atom or null-atom"
        );

        if is_const_atom(atom_id) {
            self.atom_registry.const_atoms.get(atom_id as usize)
        } else {
            self.atom_registry
                .user_atoms
                .get(atom_id as usize - ATOM_CONST_COUNT as usize)
                .and_then(|d| d.as_ref())
        }
    }

    // /// find atom descriptor by str and kind
    // pub(super) fn find_atom_desc(&self, str: &str, atom_kind: AtomKind) -> Option<AtomDesc> {
    //     self.atom_registry
    //         .atom_hash_lut
    //         .get(&calc_atom_hash_str(str, atom_kind))
    //         .and_then(|ids| {
    //             ids.iter().find_map(|id| {
    //                 let id = id.get() as u32;
    //                 if is_const_atom(id) {
    //                     Some(self.atom_registry.const_atoms[id as usize].clone())
    //                 } else {
    //                     // user atom
    //                     let d = self
    //                         .atom_registry
    //                         .user_atoms
    //                         .get(id as usize - ATOM_CONST_COUNT as usize)
    //                         .cloned()
    //                         .flatten()
    //                         .unwrap();

    //                     if d.atom_info().kind() == atom_kind && d.str.content().eq_str(str) {
    //                         Some(d)
    //                     } else {
    //                         None
    //                     }
    //                 }
    //             })
    //         })
    // }
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
            let s = StringRef::from_str(rt, str)?;
            Self::from_string(rt, s, kind)
        }
    }

    pub(crate) fn from_i64(rt: RuntimePtr, n: i64) -> Result<Self, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&n) {
            Ok(Self::new_small_int(n as u32))
        } else {
            let s = StringRef::from_utf8(rt.into(), format!("{n}").into_bytes())?;
            Self::from_string(rt, s, AtomKind::String)
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

/// calc hash for str when using as an atom descriptor
#[inline(always)]
fn calc_atom_hash_str(str: &str, atom_kind: AtomKind) -> u32 {
    let mut h = atom_kind as u32;
    for c in str.encode_utf16() {
        h = hash_u32_from_base(h, c as u32);
    }
    h
}
