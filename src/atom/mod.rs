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

/// Build the initial host const table + `name -> id` map. A host class name
/// duplicating a build-time const atom resolves to that const atom and is *not*
/// stored in the host table (or two const atoms would share one content
/// string). Duplicate host names are deduplicated.
#[allow(clippy::type_complexity)]
fn build_host_const_table(
    names: &[&'static str],
) -> (
    Vec<(&'static str, AtomKind, u32)>,
    HashMap<&'static str, u32>,
) {
    let mut table = Vec::new();
    let mut id_map = HashMap::new();
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(*name) {
            continue;
        }

        if let Some(a) = Atom::lookup_const(*name, AtomKind::String) {
            id_map.insert(*name, a.id());
            continue;
        }

        let id = ATOM_CONST_COUNT + table.len() as u32 + 1;
        table.push((
            *name,
            AtomKind::String,
            Atom::calc_atom_str_hash(*name, AtomKind::String),
        ));
        id_map.insert(*name, id);
    }

    (table, id_map)
}

/// Map class names to const atom ids against an existing host table: names in
/// the table map to their host slot, names shadowed by a build-time const atom
/// map to that const atom.
fn host_class_name_map(
    table: &[(&'static str, AtomKind, u32)],
    names: &[&'static str],
) -> HashMap<&'static str, u32> {
    names
        .iter()
        .map(|&name| {
            let id = table
                .iter()
                .enumerate()
                .find(|(_, (n, _, _))| *n == name)
                .map_or_else(
                    || {
                        Atom::lookup_const(name, AtomKind::String)
                            .expect("host class name missing from host const table")
                            .id()
                    },
                    |(i, _)| ATOM_CONST_COUNT + i as u32 + 1,
                );
            (name, id)
        })
        .collect()
}

/// Initialize host const atoms from the deduplicated host class names. Must
/// run before any runtime starts. Text hashes mirror `calc_atom_str_hash` so
/// minted descriptor hashes agree with the user-atom `hash_lut`. Returns the
/// `name -> const atom id` map for filling `ClassDef::name_atom`.
pub(crate) fn init_host_const_atoms(
    names: impl Iterator<Item = &'static str>,
) -> HashMap<&'static str, u32> {
    let names: Vec<&'static str> = names.collect();
    let (table, id_map) = build_host_const_table(&names);
    match ATOM_HOST_CLASS_NAMES.set(table) {
        Ok(()) => id_map,
        Err(prev) => {
            // Lost the first-call race (another runtime started concurrently):
            // the winner's table holds the same host entries, and shadowed
            // names (absent from both) still resolve via the const table.
            host_class_name_map(&prev, &names)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_const_atoms_shadow_buildtime_consts() {
        // "Object"/"Symbol" duplicate build-time const atoms: they must reuse
        // the const id, not mint shadowed host copies. "ZzFakeHost" is not a
        // const nor a linkme-registered host class in test builds, so it must
        // be minted as a fresh host const atom.
        let (table, map) = build_host_const_table(&["Object", "Symbol", "ZzFakeHost", "ZzFakeHost"]);
        assert_eq!(map["Object"], Atom::CONST_Object.id());
        assert_eq!(map["Symbol"], Atom::CONST_Symbol.id());
        let host_id = map["ZzFakeHost"];
        assert!(host_id > ATOM_CONST_COUNT);
        assert_eq!(map.len(), 3, "duplicate host names deduplicated");
        assert_eq!(table.len(), 1, "shadowed names live outside the host table");

        // Re-deriving the map from the stored table yields the same ids.
        let re = host_class_name_map(&table, &["Object", "ZzFakeHost"]);
        assert_eq!(re["Object"], Atom::CONST_Object.id());
        assert_eq!(re["ZzFakeHost"], host_id);
    }
}
