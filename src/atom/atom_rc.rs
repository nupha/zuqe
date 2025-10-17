use std::{ops::Deref, ptr::NonNull};

use crate::{atom::Atom, runtime::JSRuntime};

use super::JSAtom;

/// A reference-counted wrapper for JSAtom that provides automatic memory management
/// and safe access to the underlying JSAtom.
#[derive(Debug)]
pub struct JSAtomRc {
    atom: Atom,
    rt: NonNull<JSRuntime>,
}

impl JSAtomRc {
    /// the invalid atom
    pub const NULL: Self = Self {
        atom: Atom::from(0),
        rt: NonNull::dangling(),
    };

    /// wrap a atom to atom rc by cloning it
    #[inline]
    pub fn from_ref<A: Into<JSAtom>>(rt: &JSRuntime, atom: A) -> Self {
        let a: JSAtom = atom.into();
        a.dup(rt).into_rc(rt)
    }

    #[inline(always)]
    pub fn as_ref(&self) -> JSAtom {
        JSAtom(self.id())
    }

    /// consume the atom rc and return the atom id. DO NOT decrease refcount.
    pub(crate) unsafe fn leak(mut self) -> u32 {
        let a = std::mem::replace(&mut self.atom, Atom::NULL);
        unsafe { a.leak() }
    }
}

impl Deref for JSAtomRc {
    type Target = Atom;

    /// Automatically dereferences to access the inner JSAtom
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.atom
    }
}

impl Clone for JSAtomRc {
    /// Dup the inner JSAtom
    #[inline]
    fn clone(&self) -> Self {
        Self {
            atom: if !self.atom.is_null() {
                self.atom.dup(unsafe { self.rt.as_ref() })
            } else {
                Atom::NULL
            },
            rt: self.rt,
        }
    }
}

impl Drop for JSAtomRc {
    /// Automatically free inner atom when JSAtomRc is destroyed
    fn drop(&mut self) {
        let a = std::mem::replace(&mut self.atom, Atom::NULL);
        a.free(unsafe { self.rt.as_mut() });
    }
}

impl From<JSAtomRc> for Atom {
    /// consume JSAtomRc into Atom
    #[inline(always)]
    fn from(mut v: JSAtomRc) -> Self {
        std::mem::replace(&mut v.atom, Atom::NULL)
    }
}

impl Atom {
    pub fn into_rc(self, rt: &JSRuntime) -> JSAtomRc {
        JSAtomRc {
            atom: self,
            rt: NonNull::from(rt),
        }
    }
}
