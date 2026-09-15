// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod atom;
mod registry;

pub use atom::*;

pub(crate) use registry::*;

include!(concat!(env!("OUT_DIR"), "/atoms.rs"));

/// first user (per-runtime) atom id; ids below are const atoms from the
/// static `ATOM_CONST_DEFS` table.
pub const USER_ATOM_ID_START: u32 = ATOM_CONST_COUNT + 1;

use std::num::NonZeroU32;

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
    GlobalSymbol,
    Symbol,
    /// private symbol
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtomInfo {
    /// atom id
    id: NonZeroU32,
    /// atom kind
    kind: AtomKind,
    /// atom hash
    hash: u32,
}

impl AtomInfo {
    #[inline]
    pub(crate) const fn new(id: NonZeroU32, kind: AtomKind, hash: u32) -> Self {
        Self { id, kind, hash }
    }

    #[inline(always)]
    pub const fn kind(&self) -> AtomKind {
        self.kind
    }

    #[inline(always)]
    pub const fn id(&self) -> u32 {
        self.id.get()
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
