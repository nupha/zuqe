use std::{collections::hash_map::Entry, ffi::CStr, num::NonZeroU16, ptr::NonNull};

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
    JSAtom, JSAtomStruct, JSAtomType, JSAtomTypeHint, JS_ATOM_MAX_INT, JS_ATOM_NULL,
};

impl JSRuntime {
    /// register an atom.
    /// NOTE: str will be consumed once.
    /// if str is still alive after calling, it will be returned as Some(_)
    pub(crate) unsafe fn add_atom(
        &mut self,
        mut str: Option<Box<JSString>>,
        type_hint: JSAtomTypeHint,
    ) -> Result<(JSAtom, Option<Box<JSString>>), (i32, Option<Box<JSString>>)> {
        debug_assert!(type_hint != JSAtomTypeHint::None);

        let mut hash: u32 = 0;

        match type_hint {
            JSAtomTypeHint::String | JSAtomTypeHint::GlobalSymbol => {
                let ss = str.as_deref_mut().unwrap();

                if let Some(at) = ss.atom_type() {
                    if at.hint() == type_hint {
                        let a = at.atom();
                        return Ok((
                            a,
                            if __JS_AtomIsConst(a) {
                                str.and_then(|s| s.free(self))
                            } else {
                                str
                            },
                        ));
                    }
                }

                // hash for str
                hash = unsafe { ss.content().calc_hash(type_hint) };

                // match registered atoms for hash, type and content
                match self.atom_hash_lut.entry(hash) {
                    Entry::Vacant(kv) => {
                        kv.insert(Vec::with_capacity(1)); // prepare new entry for hash
                    }
                    Entry::Occupied(kv) => {
                        for &aid in kv.get() {
                            if let Some(mut astr) = self.atom_slots[u16::from(aid) as usize] {
                                let s = astr.as_mut();
                                let at = s.atom_type().unwrap();
                                if at.hint() == type_hint && s.content() == ss.content() {
                                    let a = at.atom();
                                    if !__JS_AtomIsConst(a) {
                                        s.inc_ref();
                                    }
                                    return Ok((
                                        a,
                                        if unsafe { __JS_AtomIsConst(a) } {
                                            str.and_then(|s| s.free(self))
                                        } else {
                                            str
                                        },
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            JSAtomTypeHint::None => {
                #[cfg(debug_assertions)]
                unreachable!()
            }
            _ => {
                // symbol and private hash will be calculated later
            }
        };

        //
        // register new atom
        //

        let mut ret_str: Option<Box<JSString>> = None;

        let (mut atom_desc, alive) = if let Some(ss) = str.take() {
            if ss.atom_type().is_none() {
                let rc = ss.ref_count();
                (ss, rc > 1)
            } else {
                if let Ok(s2) = JSString::copy_from(self, &ss) {
                    ret_str = ss.free(self);
                    (s2, false)
                } else {
                    return Err((-1, ss.free(self)));
                }
            }
        } else {
            // empty string
            match JSString::from_content(self, StringContent::Empty) {
                Ok(s) => (s, false),
                Err(_) => {
                    return Err((-1, None));
                }
            }
        };

        match type_hint {
            JSAtomTypeHint::Symbol => {
                hash = unsafe { atom_desc.content().calc_hash(type_hint) };
            }
            JSAtomTypeHint::Private => {
                hash = unsafe { atom_desc.content().calc_hash(type_hint) };
            }
            _ => {}
        }

        if atom_desc.is_empty() {
            println!("[O.o] atom type {type_hint:?}, empty_str, hash={hash}");
        }

        if self.atom_hash_lut.len() == self.atom_hash_lut.capacity() {
            if self.atom_hash_lut.try_reserve(16).is_err() {
                return Err((-1, str));
            }
        }

        #[cfg(feature = "dump_leaks")]
        unsafe {
            crate::types::list_add_tail(&mut atom_desc.link, &mut self.debug_string_list);
        }

        // find a free slot
        let (idx, aid) = if let Some(idx) = self
            .atom_slots
            .iter()
            .enumerate()
            .skip(1) // index 0 is not usable
            .find(|(_, &v)| v.is_none())
            .map(|(i, _)| i)
        {
            (Some(idx), NonZeroU16::new(idx as u16).unwrap())
        } else {
            if self.atom_slots.len() == self.atom_slots.capacity() {
                if self.atom_slots.try_reserve(16).is_err() {
                    return Err((-1, str));
                }
            }
            (None, NonZeroU16::new(self.atom_slots.len() as u16).unwrap())
        };

        let at = match type_hint {
            JSAtomTypeHint::String => JSAtomType::String(aid),
            JSAtomTypeHint::GlobalSymbol => JSAtomType::GlobalSymbol(aid),
            JSAtomTypeHint::Symbol => JSAtomType::Symbol(aid),
            JSAtomTypeHint::Private => JSAtomType::Private(aid),
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

        let p = NonNull::new(Box::into_raw(atom_desc)).unwrap(); // <-- take ownership of Box<JSString>
        if let Some(i) = idx {
            debug_assert!(i != 0, "atom slot #0 is not usable");
            debug_assert!(self.atom_slots[i].is_none(), "atom slot {i} is in use");
            self.atom_slots[i].replace(p);
            debug_assert_eq!(i, u16::from(aid) as usize);
        } else {
            self.atom_slots.push(Some(p));
            debug_assert_eq!(self.atom_slots.len() - 1, u16::from(aid) as usize);
        }

        if alive {
            ret_str.replace(unsafe { Box::from_raw(p.as_ptr()) });
        }
        Ok((at.atom(), ret_str))
    }

    /// find atom associated with given JSString
    pub fn find_atom_of(&self, jsstr: &JSAtomStruct) -> Option<JSAtom> {
        if let Some(at) = jsstr.atom_type() {
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
            None
        }
    }

    /// register a atom from JSString converting into atom.
    /// the JSString will be consumed once.
    /// returns JSAtom, and optionally the JSString if it is alive after converting.
    pub fn add_atom_jsstring(&mut self, ss: Box<JSAtomStruct>) -> (JSAtom, Option<Box<JSString>>) {
        if let Ok(i) = ss.content().parse_u32() {
            if i <= JS_ATOM_MAX_INT {
                let ss = ss.free(self);
                return (JSAtom::from_u32(i), ss);
            }
        }

        match unsafe { self.add_atom(Some(ss), JSAtomTypeHint::String) } {
            Ok((a, s)) => (a, s),
            Err((_, s)) => {
                // TODO: should generate an exception
                (JS_ATOM_NULL, s)
            }
        }
    }

    /// register an atom from str
    pub fn add_atom_str(&mut self, str: &str, type_hint: JSAtomTypeHint) -> Result<JSAtom, i32> {
        let ss = JSString::from_str(self, str)?;
        match unsafe { self.add_atom(Some(ss), type_hint) } {
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

    pub fn add_atom_i64(&mut self, n: i64) -> JSAtom {
        if n as u64 <= JS_ATOM_MAX_INT as u64 {
            JSAtom::from_u32(n as u32)
        } else {
            if let Ok(str) = JSString::from_str(self, &format!("{n}")) {
                match unsafe { self.add_atom(Some(str), JSAtomTypeHint::String) } {
                    Ok((a, ss)) => {
                        if let Some(s) = ss {
                            Box::leak(s);
                        }
                        a
                    }
                    Err((_, ss)) => {
                        if let Some(s) = ss {
                            Box::leak(s);
                        }
                        JS_ATOM_NULL
                    }
                }
            } else {
                return JS_ATOM_NULL;
            }
        }
    }

    #[inline(always)]
    pub fn add_atom_u32(&mut self, n: u32) -> JSAtom {
        self.add_atom_i64(n as i64)
    }

    /// get atom description for atom
    #[inline(always)]
    pub fn get_atom_desc(&self, atom: JSAtom) -> Option<NonNull<JSString>> {
        debug_assert!(atom.0 != 0);
        self.atom_slots.get(atom.0 as usize).cloned().flatten()
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
