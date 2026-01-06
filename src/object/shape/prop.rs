use gc_lite::GcTracable;
use hash_link::{HashedElement, PrimitiveHashedElement};

use crate::{
    atom::Atom,
    object::{JS_PROP_LENGTH, JS_PROP_WRITABLE},
};

/// Property defination in shape
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeProperty {
    pub(crate) atom: Atom,
    pub(super) flags: u32,
}

unsafe impl GcTracable for ShapeProperty {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.atom.trace(tracer);
    }
}

impl HashedElement for ShapeProperty {
    /// use atom id as hash value
    #[inline(always)]
    fn get_hash(&self) -> u32 {
        self.atom.id()
    }
}

impl PrimitiveHashedElement for ShapeProperty {
    #[inline(always)]
    fn is_deleted(&self) -> bool {
        self.atom.is_null()
    }

    #[inline(always)]
    fn set_deleted(&mut self) {
        self.atom = Atom::NULL;
    }
}

impl ShapeProperty {
    #[inline(always)]
    pub const fn flags(&self) -> u32 {
        self.flags
    }

    #[inline(always)]
    pub const fn set_flags(&mut self, f: u32) {
        self.flags = f;
    }

    /// is special property `length`?
    #[inline(always)]
    pub const fn is_length(&self) -> bool {
        self.flags & JS_PROP_LENGTH != 0
    }

    #[inline(always)]
    pub const fn is_writable(&self) -> bool {
        self.flags & JS_PROP_WRITABLE != 0
    }
}
