use std::{collections::hash_map::Entry, ffi::CStr, num::NonZeroU16, ptr::NonNull, rc::Rc};

use crate::{
    atom::__JS_AtomIsConst,
    runtime::JSRuntime,
    value::{JSString, StringContent},
};

use super::{
    consts::{
        JS_ATOM_Private_brand, JS_ATOM_Symbol_toPrimitive, JS_ATOM_INIT_COUNT,
        JS_DEFAULT_ATOMS_TEXT,
    },
    JSAtom, JSAtomInfo, JSAtomStruct, JSAtomTypeHint, JS_ATOM_MAX_INT, JS_ATOM_NULL,
};

impl JSRuntime {
    /// register an atom from JSString. the JSString is consumed.
    pub(crate) unsafe fn add_atom(
        &mut self,
        mut str: Option<Rc<JSString>>,
        type_hint: JSAtomTypeHint,
    ) -> Result<JSAtom, i32> {
        debug_assert!(type_hint != JSAtomTypeHint::None);

        // match str aginst existing atoms, don't register twice for same content.
        let (atom_desc, hash) = match type_hint {
            JSAtomTypeHint::String | JSAtomTypeHint::GlobalSymbol => {
                let ss = str.as_ref().unwrap();

                if let Some(at) = ss.atom_info() {
                    if at.hint() == type_hint {
                        let a = at.atom();
                        if !__JS_AtomIsConst(a) {
                            Rc::increment_strong_count(ss.as_ref());
                        }
                        return Ok(a);
                    }
                }

                let hash = unsafe { ss.content().calc_hash(type_hint) };

                // match registered atoms for hash, type and content
                match self.atom_hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for &aid in kv.get() {
                            if let Some(astr) =
                                self.atom_slots.get(u16::from(aid) as usize).unwrap()
                            {
                                let at = astr.atom_info().unwrap();
                                if at.hint() == type_hint && astr.content() == ss.content() {
                                    let a = at.atom();
                                    if !__JS_AtomIsConst(a) {
                                        Rc::increment_strong_count(astr.as_ref());
                                    }
                                    return Ok(a);
                                }
                            }
                        }
                    }
                }

                (str.take().unwrap(), hash)
            }
            JSAtomTypeHint::Symbol | JSAtomTypeHint::Private => {
                // these atom types are unique, even for same string content.
                let ss = if let Some(s) = str.take() {
                    if s.atom_info().is_none() {
                        s
                    } else {
                        JSString::new(s.content().clone())
                    }
                } else {
                    StringContent::Empty.into()
                };

                let hash = unsafe { ss.content().calc_hash(type_hint) };
                (ss, hash)
            }
            JSAtomTypeHint::None => {
                #[cfg(debug_assertions)]
                unreachable!()
            }
        };

        //
        // register new atom
        //

        if self.atom_hash_lut.len() == self.atom_hash_lut.capacity() {
            if self.atom_hash_lut.try_reserve(16).is_err() {
                return Err(-1);
            }
        }

        // find a free slot
        let (idx, aid) = if let Some(idx) = self
            .atom_slots
            .iter()
            .enumerate()
            .skip(1) // index 0 is not usable
            .find(|(_, p)| p.is_none())
            .map(|(i, _)| i)
        {
            (Some(idx), NonZeroU16::new(idx as u16).unwrap())
        } else {
            if self.atom_slots.len() == self.atom_slots.capacity() {
                if self.atom_slots.try_reserve(16).is_err() {
                    return Err(-1);
                }
            }
            (None, NonZeroU16::new(self.atom_slots.len() as u16).unwrap())
        };

        let at = match type_hint {
            JSAtomTypeHint::String => JSAtomInfo::String(aid),
            JSAtomTypeHint::GlobalSymbol => JSAtomInfo::GlobalSymbol(aid),
            JSAtomTypeHint::Symbol => JSAtomInfo::Symbol(aid),
            JSAtomTypeHint::Private => JSAtomInfo::Private(aid),
            JSAtomTypeHint::None => {
                #[cfg(debug_assertions)]
                unreachable!()
            }
        };

        atom_desc.set_hash(hash);
        atom_desc.set_atom_type(at);

        match self.atom_hash_lut.entry(hash) {
            Entry::Vacant(kv) => {
                kv.insert(vec![aid]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(aid);
            }
        }

        if let Some(i) = idx {
            debug_assert!(i != 0, "atom slot #0 is not usable");
            debug_assert!(!self.atom_slots[i].is_some(), "atom slot {i} is in use");
            self.atom_slots[i] = Some(atom_desc);
            debug_assert_eq!(i, u16::from(aid) as usize);
        } else {
            self.atom_slots.push(Some(atom_desc));
            debug_assert_eq!(self.atom_slots.len() - 1, u16::from(aid) as usize);
        }

        Ok(at.atom())
    }

    /// find atom associated with given JSString
    pub fn find_atom_of(&self, jsstr: &JSAtomStruct) -> Option<JSAtom> {
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

    /// convering Rc<JSString> into atom.
    /// Note: the `str` will be consumed.
    pub fn add_atom_jsstring(&mut self, str: Rc<JSString>) -> Result<JSAtom, i32> {
        if let Ok(i) = str.content().parse_u32() {
            if i <= JS_ATOM_MAX_INT {
                return Ok(JSAtom::from_u32(i));
            }
        }

        unsafe { self.add_atom(Some(str), JSAtomTypeHint::String) }
    }

    /// register an atom from str
    pub fn add_atom_str(&mut self, str: &str, type_hint: JSAtomTypeHint) -> Result<JSAtom, i32> {
        self.add_atom_jsstring(JSString::from_str(str)?)
    }

    pub fn add_atom_i64(&mut self, n: i64) -> JSAtom {
        if n as u64 <= JS_ATOM_MAX_INT as u64 {
            JSAtom::from_u32(n as u32)
        } else {
            if let Ok(tmp) = JSString::from_utf8(format!("{n}").into_bytes()) {
                self.add_atom_jsstring(tmp).unwrap_or(JS_ATOM_NULL)
            } else {
                JS_ATOM_NULL
            }
        }
    }

    #[inline(always)]
    pub fn add_atom_u32(&mut self, n: u32) -> JSAtom {
        self.add_atom_i64(n as i64)
    }

    /// get atom description for atom.
    /// returns null if atom is not found.
    #[inline(always)]
    pub fn get_atom_desc(&self, atom: JSAtom) -> Option<&Rc<JSString>> {
        debug_assert!(atom.0 != 0);
        if let Some(s) = self.atom_slots.get(atom.0 as usize) {
            s.as_ref()
        } else {
            None
        }
    }

    /// initialize default constant atoms
    pub unsafe fn init_default_atoms(&mut self) -> Result<(), i32> {
        let mut p = JS_DEFAULT_ATOMS_TEXT.as_ptr();
        for i in 1..JS_ATOM_INIT_COUNT {
            let atom_type = if i == JS_ATOM_Private_brand.0 {
                JSAtomTypeHint::Private
            } else if i >= JS_ATOM_Symbol_toPrimitive.0 {
                JSAtomTypeHint::Symbol
            } else {
                JSAtomTypeHint::String
            };

            let cstr = CStr::from_ptr(p.cast());
            self.add_atom_str(cstr.to_str().unwrap(), atom_type)?;

            p = p.add(cstr.count_bytes() + 1);
        }

        Ok(())
    }
}
