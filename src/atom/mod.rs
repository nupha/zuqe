mod atom;
mod registry;

pub use atom::*;

pub(crate) use registry::*;

include!(concat!(env!("OUT_DIR"), "/atoms.rs"));

use std::num::NonZeroU16;

use crate::string::StringRef;

/// atom descriptor kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum AtomDescKind {
    JS_ATOM_KIND_STRING,
    JS_ATOM_KIND_SYMBOL,
    JS_ATOM_KIND_PRIVATE,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum AtomKind {
    String = 1,
    #[cfg(feature = "builtin_symbol")]
    GlobalSymbol,
    #[cfg(feature = "builtin_symbol")]
    Symbol,
    /// private symbol
    #[cfg(feature = "builtin_symbol")]
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtomInfo {
    /// atom id
    id: NonZeroU16,
    /// atom kind
    kind: AtomKind,
    /// atom hash
    hash: u32,
}

impl AtomInfo {
    #[inline]
    pub(super) const fn new(id: NonZeroU16, kind: AtomKind, hash: u32) -> Self {
        Self { id, kind, hash }
    }

    #[inline(always)]
    pub const fn kind(&self) -> AtomKind {
        self.kind
    }

    #[inline(always)]
    pub const fn id(&self) -> u32 {
        self.id.get() as u32
    }

    #[inline(always)]
    pub const fn hash(&self) -> u32 {
        self.hash
    }

    #[inline(always)]
    pub const fn is_const(&self) -> bool {
        is_const_atom(self.id())
    }

    #[inline(always)]
    pub const fn is_small_int(&self) -> bool {
        is_small_int_atom(self.id())
    }
}
