// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod atom;
mod registry;

pub use atom::*;

pub(crate) use registry::*;

include!(concat!(env!("OUT_DIR"), "/atoms.rs"));

use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU32,
    sync::OnceLock,
};

/// Host const atoms: `(name, kind, text_hash)`, kept in id order. Host atom
/// ids are `ATOM_CONST_COUNT + 1 .. ATOM_CONST_COUNT + len`, just past the
/// build-time generated const atoms. Populated once by
/// `init_host_const_atoms` (from the linkme-collected host class names).
static ATOM_HOST_CLASS_NAMES: OnceLock<Vec<(&'static str, AtomKind, u32)>> = OnceLock::new();

/// First user (per-runtime) atom id: below it lie the build-time const atoms
/// plus the runtime-initialized host const atoms.
pub(crate) fn user_atom_id_start() -> u32 {
    ATOM_CONST_COUNT + ATOM_HOST_CLASS_NAMES.get().map_or(0, |v| v.len() as u32) + 1
}

/// Initialize host const atoms from the deduplicated host class names. Must
/// run before any runtime starts. Text hashes mirror `calc_atom_str_hash` so
/// minted descriptor hashes agree with the user-atom `hash_lut`. Returns the
/// `name -> const atom id` map for filling `ClassDef::name_atom`.
pub(crate) fn init_host_const_atoms(
    names: impl Iterator<Item = &'static str>,
) -> HashMap<&'static str, u32> {
    if let Some(table) = ATOM_HOST_CLASS_NAMES.get() {
        return table
            .iter()
            .enumerate()
            .map(|(i, (name, _, _))| (*name, ATOM_CONST_COUNT + i as u32 + 1))
            .collect();
    }
    let mut table = Vec::new();
    let mut id_map = HashMap::new();
    let mut seen = HashSet::new();
    for name in names {
        if seen.insert(name) {
            let id = ATOM_CONST_COUNT + table.len() as u32 + 1;
            table.push((
                name,
                AtomKind::String,
                Atom::calc_atom_str_hash(name, AtomKind::String),
            ));
            id_map.insert(name, id);
        }
    }
    if let Err(prev) = ATOM_HOST_CLASS_NAMES.set(table) {
        // Lost the first-call race (another runtime started concurrently).
        return prev
            .iter()
            .enumerate()
            .map(|(i, (name, _, _))| (*name, ATOM_CONST_COUNT + i as u32 + 1))
            .collect();
    }
    id_map
}

/// Resolve a host const atom by `(kind, content-vec)` from the host table
/// (linear scan; the host set is small). Returns `None` before init or on miss.
pub(crate) fn lookup_host_const(kind: AtomKind, cmp: impl Fn(&str) -> bool) -> Option<Atom> {
    let host = ATOM_HOST_CLASS_NAMES.get()?;
    for (i, (name, k, _)) in host.iter().enumerate() {
        if *k == kind && cmp(name) {
            return Some(Atom::from_id(ATOM_CONST_COUNT + i as u32 + 1));
        }
    }
    None
}

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
    pub fn is_const(&self) -> bool {
        is_const_atom(self.id())
    }

    #[inline(always)]
    pub const fn is_small_int(&self) -> bool {
        is_small_int_atom(self.id())
    }
}
