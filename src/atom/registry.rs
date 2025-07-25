use std::{collections::hash_map::Entry, ffi::CStr, num::NonZeroU16, rc::Rc};

use crate::{
    runtime::JSRuntime,
    string::{JSString, StringContent},
};

use super::{
    AtomID, AtomType, JS_ATOM_MAX_INT, JS_ATOM_NULL, JSAtom,
    consts::{
        JS_ATOM_INIT_COUNT, JS_ATOM_Private_brand, JS_ATOM_Symbol_toPrimitive,
        JS_DEFAULT_ATOMS_TEXT,
    },
};

impl JSRuntime {
    /// find atom by jsstring
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

    /// register an atom using JSString.
    /// returned atom has ownership, caller should free it after use.
    pub(crate) fn register_atom(
        &mut self,
        mut desc: Option<JSString>,
        type_hint: AtomType,
    ) -> Result<JSAtom, i32> {
        debug_assert_ne!(type_hint, AtomType::None);

        // match str aginst existing atoms, don't register twice for same content.
        let atom_desc = match type_hint {
            AtomType::String | AtomType::GlobalSymbol => {
                let ss = desc.take().unwrap();

                if let Some(at) = ss.atom_info() {
                    if at.hint() == type_hint {
                        let a = at.atom();
                        if !a.is_const() {
                            let _ = Rc::into_raw(ss.0); // leak string strong count
                        }
                        return Ok(a);
                    }
                }

                let hash = ss.content().calc_hash(type_hint);

                // match registered atoms for hash, type and content
                match self.atom_hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for &aid in kv.get() {
                            let aptr = self.atom_slots[u16::from(aid) as usize];
                            if !aptr.is_null() {
                                let at = unsafe { (*aptr).atom_info().unwrap() };
                                if at.hint() == type_hint
                                    && unsafe { (*aptr).content() } == ss.content()
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
            AtomType::Symbol | AtomType::Private => {
                // these atom types are unique, even for same string content.
                let ss = if let Some(s) = desc.take() {
                    if s.atom_info().is_none() {
                        s
                    } else {
                        // clone to a new jsstring
                        JSString::new(self, s.content().clone())
                    }
                } else {
                    JSString::new(self, Rc::new(StringContent::Empty))
                };

                let hash = ss.content().calc_hash(type_hint);
                ss.set_hash(hash);
                ss
            }
            AtomType::None => {
                unreachable!()
            }
        };

        //
        // register new atom
        //

        if self.atom_hash_lut.len() == self.atom_hash_lut.capacity() {
            self.atom_hash_lut.try_reserve(16).map_err(|_| -1)?;
        }

        // find free atom slot
        let (idx, aid) = if let Some(idx) = self
            .atom_slots
            .iter()
            .enumerate()
            .skip(1) // index 0 is not usable
            .find(|(_, p)| p.is_null())
            .map(|(i, _)| i)
        {
            (Some(idx), NonZeroU16::new(idx as u16).unwrap())
        } else {
            if self.atom_slots.len() == self.atom_slots.capacity() {
                self.atom_slots.try_reserve(16).map_err(|_| -1)?;
            }
            (None, NonZeroU16::new(self.atom_slots.len() as u16).unwrap())
        };

        atom_desc.set_atom_type(match type_hint {
            AtomType::String => AtomID::String(aid),
            AtomType::GlobalSymbol => AtomID::GlobalSymbol(aid),
            AtomType::Symbol => AtomID::Symbol(aid),
            AtomType::Private => AtomID::Private(aid),
            AtomType::None => {
                unreachable!()
            }
        });

        match self.atom_hash_lut.entry(atom_desc.hash()) {
            Entry::Vacant(kv) => {
                kv.insert(vec![aid]);
            }
            Entry::Occupied(kv) => {
                kv.into_mut().push(aid);
            }
        }

        if let Some(i) = idx {
            debug_assert!(i != 0, "atom slot #0 is not usable");
            debug_assert!(self.atom_slots[i].is_null(), "atom slot {i} is in use");
            debug_assert_eq!(i, u16::from(aid) as usize);
            self.atom_slots[i] = Rc::into_raw(atom_desc.0);
        } else {
            self.atom_slots.push(Rc::into_raw(atom_desc.0));
            debug_assert_eq!(self.atom_slots.len() - 1, u16::from(aid) as usize);
        }

        Ok(JSAtom(aid.get() as u32))
    }

    /// convering a JSString into atom.
    /// Note: caller should free the result atom.
    pub fn register_atom_jsstring(
        &mut self,
        str: JSString,
        type_hint: AtomType,
    ) -> Result<JSAtom, i32> {
        if let Ok(i) = str.content().parse_u32() {
            if i <= JS_ATOM_MAX_INT {
                return Ok(JSAtom::new_u32(i));
            }
        }

        self.register_atom(Some(str), type_hint)
    }

    /// register an atom from str
    pub fn register_atom_str(&mut self, str: &str, type_hint: AtomType) -> Result<JSAtom, i32> {
        let s = JSString::from_str(self, str)?;
        self.register_atom_jsstring(s, type_hint)
    }

    pub fn register_atom_i64(&mut self, n: i64) -> JSAtom {
        if n as u64 <= JS_ATOM_MAX_INT as u64 {
            JSAtom::new_u32(n as u32)
        } else {
            if let Ok(tmp) = JSString::from_utf8(self, format!("{n}").into_bytes()) {
                self.register_atom_jsstring(tmp, AtomType::String)
                    .unwrap_or(JS_ATOM_NULL)
            } else {
                JS_ATOM_NULL
            }
        }
    }

    #[inline(always)]
    pub fn register_atom_u32(&mut self, n: u32) -> JSAtom {
        self.register_atom_i64(n as i64)
    }

    /// get atom description for atom.
    /// returns None if atom does not exist.
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
                AtomType::Private
            } else if i >= JS_ATOM_Symbol_toPrimitive.0 {
                AtomType::Symbol
            } else {
                AtomType::String
            };

            let cstr = unsafe { CStr::from_ptr(p.cast()) };
            self.register_atom_str(cstr.to_str().unwrap(), atom_type)?;

            p = unsafe { p.add(cstr.count_bytes() + 1) };
        }

        Ok(())
    }
}
