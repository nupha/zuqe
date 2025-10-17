use std::{ops::Deref, ptr::NonNull};

use crate::{atom::Atom, runtime::JSRuntime};

use super::AtomRef;

/// A reference-counted wrapper for AtomRef that provides automatic memory management
/// and safe access to the underlying AtomRef.
#[derive(Debug)]
pub struct AtomRc {
    atom: Atom,
    rt: NonNull<JSRuntime>,
}

impl AtomRc {
    /// the invalid atom
    pub const NULL: Self = Self {
        atom: Atom::from(0),
        rt: NonNull::dangling(),
    };

    /// wrap a atom to atom rc by cloning it
    #[inline]
    pub fn from_ref<A: Into<AtomRef>>(rt: &JSRuntime, atom: A) -> Self {
        let a: AtomRef = atom.into();
        a.dup(rt).into_rc(rt)
    }

    #[inline(always)]
    pub fn as_ref(&self) -> AtomRef {
        AtomRef(self.id())
    }

    /// consume the atom rc and return the atom id. DO NOT decrease refcount.
    pub(crate) unsafe fn leak(mut self) -> u32 {
        let a = std::mem::replace(&mut self.atom, Atom::NULL);
        unsafe { a.leak() }
    }
}

impl Deref for AtomRc {
    type Target = Atom;

    /// Automatically dereferences to access the inner AtomRef
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.atom
    }
}

impl Clone for AtomRc {
    /// Dup the inner AtomRef
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

impl Drop for AtomRc {
    /// Automatically free inner atom when AtomRc is destroyed
    fn drop(&mut self) {
        let a = std::mem::replace(&mut self.atom, Atom::NULL);
        a.free(unsafe { self.rt.as_mut() });
    }
}

impl From<AtomRc> for Atom {
    /// consume AtomRc into Atom
    #[inline(always)]
    fn from(mut v: AtomRc) -> Self {
        std::mem::replace(&mut v.atom, Atom::NULL)
    }
}

impl Atom {
    pub fn into_rc(self, rt: &JSRuntime) -> AtomRc {
        AtomRc {
            atom: self,
            rt: NonNull::from(rt),
        }
    }
}
