use std::{ops::Deref, ptr::NonNull};

use crate::runtime::JSRuntime;

use super::{JS_ATOM_NULL, JS_FreeAtomRT, JSAtom};

/// A reference-counted wrapper for JSAtom that provides automatic memory management
/// and safe access to the underlying JSAtom.
pub struct JSAtomRc {
    rt: NonNull<JSRuntime>,
    atom: JSAtom,
}

impl JSAtomRc {
    /// return NULL atom rc
    pub fn null(rt: &JSRuntime) -> Self {
        Self {
            rt: NonNull::from(rt),
            atom: JS_ATOM_NULL,
        }
    }

    /// wrap a atom to atom rc by cloning it
    #[inline]
    pub fn clone_from(rt: &JSRuntime, atom: JSAtom) -> Self {
        atom.dup(rt).into_rc(rt)
    }
}

impl Deref for JSAtomRc {
    type Target = JSAtom;

    /// Automatically dereferences to access the inner JSAtom
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.atom
    }
}

impl Clone for JSAtomRc {
    /// Dup the inner JSAtom
    #[inline(always)]
    fn clone(&self) -> Self {
        let v = self.atom.dup(unsafe { self.rt.as_ref() });
        Self {
            rt: self.rt,
            atom: v,
        }
    }
}

impl From<JSAtomRc> for JSAtom {
    /// Converts JSAtomRc to JSAtom while resetting the underlying JSAtom to undefined
    #[inline(always)]
    fn from(mut v: JSAtomRc) -> Self {
        std::mem::replace(&mut v.atom, JS_ATOM_NULL)
    }
}

impl Drop for JSAtomRc {
    /// Automatically free inner atom when JSAtomRc is destroyed
    fn drop(&mut self) {
        unsafe {
            JS_FreeAtomRT(self.rt.as_mut(), self.atom);
        }
    }
}

impl JSAtom {
    /// Wraps a JSAtom into a reference-counted JSAtomRc
    #[inline(always)]
    pub fn into_rc(self, rt: &JSRuntime) -> JSAtomRc {
        JSAtomRc {
            rt: NonNull::from(rt),
            atom: self,
        }
    }
}
