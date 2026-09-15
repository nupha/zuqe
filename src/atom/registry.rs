// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    collections::{HashMap, hash_map::Entry},
    num::NonZeroU32,
};

use crate::{
    Exception, RuntimeImpl, ScopedContext,
    atom::{is_const_atom, is_small_int_atom},
    number::parse_u32,
    string::{JS_STRING_LEN_MAX, StringData, StringRef},
    utils::{hash_u32_from_base, unlikely},
};

use super::{
    ATOM_CONST_COUNT, ATOM_CONST_DEFS, Atom, AtomInfo, AtomKind, JS_ATOM_HASH_PRIVATE,
    JS_ATOM_HASH_SYMBOL, JS_ATOM_MAX, JS_ATOM_MAX_INT, JS_ATOM_TAG_INT, USER_ATOM_ID_START,
};

pub(crate) struct AtomRegistry {
    /// user atom descriptor slots. None means the slot is unused.
    user_atoms: Vec<Option<StringRef>>,

    /// atom hash lookup table, is dict { atom_hash: [atom_id] },
    /// slot index = atom_id - 1
    hash_lut: HashMap<u32, Vec<u32>>,
}

impl AtomRegistry {
    /// create atom registry, setup with const atoms.
    pub(crate) fn new() -> Self {
        Self {
            user_atoms: Vec::new(),
            hash_lut: HashMap::new(),
        }
    }
}

/// Register user atom with `str` desciptor
/// (in case of creating an anonymous symbol, str desc should be ident-type-string).
/// Check for atom reuse in prior to perform real registering.
fn register_atom(
    rt: &mut RuntimeImpl,
    mut ctx: Option<&mut ScopedContext>,
    str: StringRef,
    mut atom_kind: AtomKind,
) -> Result<Atom, i32> {
    // match for existing atoms, don't register twice for same content.
    let (desc_str, hash): (StringRef, u32) = match atom_kind {
        AtomKind::String | AtomKind::GlobalSymbol => {
            debug_assert!(!str.content().is_ident());

            let ss = if let Some(a) = str.atom_info() {
                // already registered
                if a.kind() == atom_kind {
                    return Ok(Atom(a.id()));
                }

                // dup string content without atom info
                if let Some(cx) = ctx.as_mut() {
                    StringRef::from_content(cx, str.content().clone())?
                } else {
                    StringRef::create_root(rt, str.content().clone(), None)?
                }
            } else {
                str
            };

            let hash = Atom::calc_atom_hash(ss.content(), atom_kind);
            let a_reg = &mut rt.atom_registry;

            // check for reuse: match registered atoms for (hash, atom_type, string_content)
            if let Some(kv) = a_reg.hash_lut.get(&hash) {
                for &atom_id in kv {
                    let atom = Atom::from_id(atom_id);
                    if atom.is_const() {
                        if atom.const_kind() == atom_kind && ss.content().eq_str(atom.const_str()) {
                            return Ok(atom);
                        }
                        continue;
                    }

                    let ds = a_reg.user_atoms[atom_id as usize - USER_ATOM_ID_START as usize]
                        .as_ref()
                        .unwrap();
                    let at_info = ds.atom_info().unwrap();
                    if at_info.kind() == atom_kind && ss.eq(ds) {
                        return Ok(Atom(at_info.id()));
                    }
                }
            }

            (ss, hash)
        }

        AtomKind::Symbol | AtomKind::Private => {
            // these atom types are distinct, even they have same string content.
            let ss: StringRef = if str.content().is_ident() {
                debug_assert_eq!(
                    atom_kind,
                    AtomKind::Symbol,
                    "ident string type is only for anonymous symbol"
                );
                str
            } else if str.atom_info().is_some() {
                // dup string content without atom info
                if let Some(cx) = ctx.as_mut() {
                    StringRef::from_content(cx, str.content().clone())?
                } else {
                    StringRef::create_root(rt, str.content().clone(), None)?
                }
            } else {
                str
            };

            let hash = if atom_kind == AtomKind::Symbol {
                JS_ATOM_HASH_SYMBOL
            } else {
                atom_kind = AtomKind::Symbol;
                JS_ATOM_HASH_PRIVATE
            };

            (ss, hash)
        }
    };

    debug_assert!(desc_str.atom_info().is_none());

    if rt.atom_registry.hash_lut.len() == rt.atom_registry.hash_lut.capacity()
        && rt.atom_registry.hash_lut.try_reserve(16).is_err()
    {
        if let Some(c) = ctx {
            c.throw(Exception::OutOfMemory);
        }
        return Err(-1);
    }

    // find a free slot
    let (idx, atom_id) =
        if let Some(i) = rt.atom_registry.user_atoms.iter().position(|p| p.is_none()) {
            (Some(i), i as u32 + USER_ATOM_ID_START)
        } else {
            if rt.atom_registry.user_atoms.len() == rt.atom_registry.user_atoms.capacity()
                && rt.atom_registry.user_atoms.try_reserve(16).is_err()
            {
                if let Some(c) = ctx {
                    c.throw(Exception::OutOfMemory);
                }
                return Err(-1);
            }

            // [GUARDIAN] an atom id is a u32 whose highest bit is reserved for the small-int
            // tag, so a registered atom id must stay within `1..=i32::MAX` (== JS_ATOM_MAX).
            // Refuse to allocate past that range instead of overflowing into the tag bit.
            let new_id = rt.atom_registry.user_atoms.len() as u32 + USER_ATOM_ID_START;
            if new_id > JS_ATOM_MAX {
                if let Some(c) = ctx {
                    c.throw(Exception::TooManyAtoms);
                }
                return Err(-1);
            }

            (None, new_id)
        };

    match rt.atom_registry.hash_lut.entry(hash) {
        Entry::Occupied(kv) => {
            kv.into_mut().push(atom_id);
        }
        Entry::Vacant(kv) => {
            kv.insert(vec![atom_id]);
        }
    }

    desc_str.set_atom_info(AtomInfo::new(
        unsafe { NonZeroU32::new_unchecked(atom_id) },
        atom_kind,
        hash,
    ));

    if let Some(i) = idx {
        debug_assert_eq!(i, atom_id as usize - USER_ATOM_ID_START as usize);
        debug_assert!(
            rt.atom_registry.user_atoms[i].is_none(),
            "atom slot {i} is in use"
        );
        rt.atom_registry.user_atoms[i].replace(desc_str);
    } else {
        rt.atom_registry.user_atoms.push(Some(desc_str));

        debug_assert_eq!(
            rt.atom_registry.user_atoms.len(),
            atom_id as usize - USER_ATOM_ID_START as usize + 1
        );
    }

    log::trace!(
        "[register_atom] {atom_id}: {:?}, {:?}",
        atom_kind,
        desc_str.gc_head()
    );

    Ok(Atom(atom_id))
}

impl RuntimeImpl {
    /// init atom registry: seed the hash lookup table with the static const
    /// atoms. Const atoms have no GC descriptor node — their content/kind come
    /// from `ATOM_CONST_DEFS` at `(atom_id - 1)`.
    pub(crate) fn init_const_atoms(&mut self) {
        debug_assert!(
            self.atom_registry.hash_lut.is_empty(),
            "must init atom registry only once"
        );

        self.atom_registry
            .hash_lut
            .reserve(ATOM_CONST_COUNT as usize + 64);

        for (idx, &(str, atom_kind)) in ATOM_CONST_DEFS.iter().enumerate() {
            let atom_id = idx as u32 + 1;

            let hash: u32 = match atom_kind {
                AtomKind::String | AtomKind::GlobalSymbol => {
                    Atom::calc_atom_str_hash(str, atom_kind)
                }
                AtomKind::Symbol => JS_ATOM_HASH_SYMBOL,
                AtomKind::Private => JS_ATOM_HASH_PRIVATE,
            };

            match self.atom_registry.hash_lut.entry(hash) {
                Entry::Vacant(kv) => {
                    kv.insert(vec![atom_id]);
                }
                Entry::Occupied(kv) => {
                    kv.into_mut().push(atom_id);
                }
            }
        }
    }

    /// remove atom from registry.
    /// this is called when the string descriptor is being dropped,
    /// so don't trigger string drop again.
    pub(crate) fn remove_atom(&mut self, atom_info: &AtomInfo) {
        let atom_id = atom_info.id();
        let atom_hash = atom_info.hash();
        debug_assert!(!is_const_atom(atom_id) && !is_small_int_atom(atom_id));

        let a_reg = &mut self.atom_registry;

        // Note: atom_id associated descriptor string is now an invalid gc pointer.
        if let Some(slot) = a_reg
            .user_atoms
            .get_mut(atom_id as usize - USER_ATOM_ID_START as usize)
            && slot.take().is_some()
        {
            let purge = if let Some(ids) = a_reg.hash_lut.get_mut(&atom_hash) {
                if ids.len() == 1 {
                    debug_assert_eq!(ids[0], atom_id);
                    true
                } else {
                    let i = {
                        #[cfg(debug_assertions)]
                        {
                            ids.iter().position(|&x| x == atom_id).unwrap()
                        }
                        #[cfg(not(debug_assertions))]
                        unsafe {
                            ids.iter().position(|&x| x == atom_id).unwrap_unchecked()
                        }
                    };

                    ids.swap_remove(i);
                    false
                }
            } else {
                false
            };

            if purge {
                a_reg.hash_lut.remove(&atom_hash);
            }
        }
    }

    /// get atom descriptor string by atom id.
    /// returns None for const atoms (no GC node) or invalid user atom ids.
    pub(crate) fn atom_desc_string(&self, atom_id: u32) -> Option<StringRef> {
        debug_assert!(
            atom_id != 0 && !is_small_int_atom(atom_id),
            "not for small-int-atom or null-atom"
        );

        if is_const_atom(atom_id) {
            None
        } else {
            self.atom_registry
                .user_atoms
                .get(atom_id as usize - USER_ATOM_ID_START as usize)
                .and_then(|d| *d)
        }
    }
}

impl Atom {
    /// make atom from `str`. if `str` is a safe-integer representation, create int-atom instead.
    ///
    /// Because the returned atom is a root, the underlying descriptor string is guaranteed to
    /// survive GC. Non-small-int atoms always store a root descriptor — if the same string was
    /// previously registered as a non-root atom (e.g. via `from_str` in a GC scope), this function
    /// upgrades it to a root.
    pub(crate) fn root_str(rt: &mut RuntimeImpl, str: &str) -> Result<Self, i32> {
        if str.is_empty() {
            Ok(Atom::CONST_empty_string)
        } else if let Some(i) = parse_u32(str)
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            let s = StringRef::create_root(rt, StringData::from_str(str)?, None)?;
            let atom = register_atom(rt, None, s, AtomKind::String)?;
            // register_atom may return an existing atom that was originally created
            // via from_str/from_string (scope-local, non-root descriptor). Ensure
            // the descriptor is root regardless of its origin. Const atoms have no
            // GC node and are already immortal.
            if !atom.is_const() {
                let desc = rt.atom_desc_string(atom.id()).unwrap();
                if !desc.gc_head().is_root() {
                    rt.gc_heap_mut().set_root_node(desc.gc_head_ptr());
                }
            }
            Ok(atom)
        }
    }

    #[inline(always)]
    pub(crate) fn from_content_as_is(
        ctx: &mut ScopedContext,
        str: StringRef,
        kind: AtomKind,
    ) -> Result<Self, i32> {
        register_atom(&mut ctx.rt_ptr(), Some(ctx), str, kind)
    }

    pub(crate) fn intern_content(
        ctx: &mut ScopedContext,
        str: StringRef,
        kind: AtomKind,
    ) -> Result<Self, i32> {
        if str.is_empty() {
            Ok(Atom::CONST_empty_string)
        } else if let Some(i) = str.parse_u32()
            && i <= JS_ATOM_MAX_INT
        {
            Ok(Self::new_small_int(i))
        } else {
            register_atom(&mut ctx.rt_ptr(), Some(ctx), str, kind)
        }
    }

    /// Look up an already-registered atom of `kind` by content, **without
    /// allocating**. Returns `None` when `str` is not interned yet.
    ///
    /// The hash is computed with [`Atom::calc_atom_str_hash`], which matches
    /// [`Atom::calc_atom_hash`] for well-formed strings (both fold scalar
    /// values, and `StringData::chars()` combines surrogate pairs), so a hit
    /// lands in the same bucket the atom was registered under.
    ///
    /// Only `String`/`GlobalSymbol` atoms are content-addressable: named
    /// `Symbol`/`Private` atoms are intentionally *not* deduplicated by content
    /// (they use a fixed hash), so this returns `None` for them and the caller
    /// falls back to the registering slow path.
    fn lookup_str(rt: &RuntimeImpl, str: &str, kind: AtomKind) -> Option<Atom> {
        if str.is_empty() {
            return Some(Atom::CONST_empty_string);
        } else if let Some(i) = parse_u32(str)
            && i <= JS_ATOM_MAX_INT
        {
            return Some(Atom::new_small_int(i));
        }

        let hash = Self::calc_atom_str_hash(str, kind);
        // `hash_lut` maps content hash -> candidate atom ids (const + user).
        for &id in rt.atom_registry.hash_lut.get(&hash)? {
            let atom = Atom::from_id(id);
            if atom.is_const() {
                if atom.const_kind() == kind && atom.const_str() == str {
                    return Some(atom);
                }
                continue;
            }

            let Some(desc) = rt.atom_desc_string(id) else {
                continue;
            };
            // Guard against cross-kind hash collisions: only `String` atoms.
            if desc.atom_info().is_some_and(|a| a.kind() == kind) && desc.content().eq_str(str) {
                return Some(Atom(id));
            }
        }

        None
    }

    /// Intern an atom from `str`. if `str` is a safe-integer representation, create int-atom instead.
    ///
    /// Fast path: an already-interned atom is returned **without allocating**
    /// (see [`Atom::lookup_str`]); only a miss allocates a `StringRef` and
    /// registers a new atom.
    ///
    /// Enforces `JS_STRING_LEN_MAX` here so every `&str -> atom` path (parser,
    /// `prop_str_to_atom`, `create_interned_string`, zapi keys, …) rejects an
    /// over-long key with a proper `InternalError` instead of tripping the
    /// `StringBuf` debug-assert (debug) or silently building an over-long
    /// string (release).
    #[inline]
    pub(crate) fn intern_str(ctx: &mut ScopedContext, str: &str) -> Result<Self, i32> {
        if unlikely(str.len() > JS_STRING_LEN_MAX) {
            return Err(ctx.throw_internal_error("string too long"));
        }
        if let Some(a) = Self::lookup_str(ctx.rt(), str, AtomKind::String) {
            Ok(a)
        } else {
            let s = StringRef::alloc_from_str(ctx, str)?;
            Self::intern_content(ctx, s, AtomKind::String)
        }
    }

    /// Intern an atom from static &str
    ///
    /// Fast path: an already-interned atom is returned without allocating a
    /// descriptor node (see [`Atom::lookup_str`]).
    pub fn intern_static(ctx: &mut ScopedContext, str: &'static str) -> Result<Self, i32> {
        if let Some(a) = Self::lookup_str(ctx.rt(), str, AtomKind::String) {
            Ok(a)
        } else {
            let s = StringRef::alloc_static(ctx, str, None)?;
            Self::intern_content(ctx, s, AtomKind::String)
        }
    }

    /// Make an small integral atom.
    /// Small integral atom is primitive, and do not need to register.
    #[inline(always)]
    pub(crate) fn new_small_int(n: u32) -> Self {
        debug_assert!(n <= JS_ATOM_MAX_INT);
        Self(n | JS_ATOM_TAG_INT)
    }

    #[inline]
    pub(crate) fn new_i64(ctx: &mut ScopedContext, i: i64) -> Result<Self, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&i) {
            Ok(Self::new_small_int(i as u32))
        } else {
            let s = StringRef::from_utf8_bytes(ctx, format!("{i}").into())?;
            Self::intern_content(ctx, s, AtomKind::String)
        }
    }

    /// Create const atom
    #[inline(always)]
    pub(crate) const fn new_const(id: u32) -> Self {
        debug_assert!(is_const_atom(id));
        Self(id)
    }

    /// calc atom hash with str and kind
    pub(crate) fn calc_atom_hash(str: &StringData, kind: AtomKind) -> u32 {
        let mut h = kind as u32;
        if !str.is_empty() && !str.is_ident() {
            for (_, c) in str.chars() {
                h = hash_u32_from_base(h, c.to_u32());
            }
        }
        h
    }

    /// calc atom hash for str and type.
    #[inline(always)]
    fn calc_atom_str_hash(str: &str, kind: AtomKind) -> u32 {
        let mut h = kind as u32;
        if !str.is_empty() {
            for c in str.chars() {
                h = hash_u32_from_base(h, c as _);
            }
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Runtime, ScopedContext};
    use std::time::Instant;

    /// Engine-side benchmark for the `lookup_str` fast path.
    ///
    /// Verifies that resolving an already-interned key no longer allocates a
    /// descriptor, and shows the benefit for both the raw lookup and the
    /// `&str -> atom` / `&str -> (interned) value` entry points.
    #[test]
    fn lookup_str_fast_path_avoids_allocation() -> Result<(), i32> {
        const N: usize = 200_000;
        // A realistic mix, so the hash bucket / descriptor cache isn't trivially hot.
        const NAMES: &[&str] = &["role", "content", "tool_call_id", "name", "arguments"];

        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            Ok(())
        })?;

        ctx.with_new_scope(|cx| {
            // Intern the keys once (slow path), as a parser would.
            for n in NAMES {
                let _ = Atom::intern_str(cx, n)?;
            }

            // 1. Raw lookup: hash + table probe + content compare, no alloc.
            let t = Instant::now();
            for i in 0..N {
                std::hint::black_box(Atom::lookup_str(
                    cx.rt(),
                    NAMES[i % NAMES.len()],
                    AtomKind::String,
                ));
            }
            let lookup = t.elapsed();

            // 2. `from_str` on a hit: lookup fast path.
            let t = Instant::now();
            for i in 0..N {
                std::hint::black_box(Atom::intern_str(cx, NAMES[i % NAMES.len()])?);
            }
            let fast = t.elapsed();

            // 3. Pre-optimization `from_str`: allocate a StringRef, then register.
            let t = Instant::now();
            for i in 0..N {
                let s = StringRef::alloc_from_str(cx, NAMES[i % NAMES.len()])?;
                std::hint::black_box(Atom::intern_content(cx, s, AtomKind::String)?);
            }
            let slow = t.elapsed();

            // 4. `&str -> interned value` (create_atom_string) reuses the descriptor.
            let t = Instant::now();
            for i in 0..N {
                std::hint::black_box(cx.create_string_interned(NAMES[i % NAMES.len()])?);
            }
            let atom_str = t.elapsed();

            // 5. fresh value via utf8 bytes (create_string_from_utf8) always allocates.
            let t = Instant::now();
            for i in 0..N {
                std::hint::black_box(
                    cx.create_string_from_utf8(NAMES[i % NAMES.len()].as_bytes().to_vec())?,
                );
            }
            let new_str = t.elapsed();

            let n = N as f64;
            let ns = |d: std::time::Duration| d.as_nanos() as f64 / n;
            println!(
                "\n[atom key resolution, {N} ops, {} distinct keys]",
                NAMES.len()
            );
            println!(
                "  lookup_str (raw lookup)          : {:>8.1} ns/op",
                ns(lookup)
            );
            println!(
                "  Atom::from_str (fast, on hit)    : {:>8.1} ns/op",
                ns(fast)
            );
            println!(
                "  Atom::from_str (slow, old path)  : {:>8.1} ns/op",
                ns(slow)
            );
            println!(
                "  create_string_interned (reuse)   : {:>8.1} ns/op",
                ns(atom_str)
            );
            println!(
                "  create_string_from_utf8 (fresh)  : {:>8.1} ns/op",
                ns(new_str)
            );
            println!(
                "  => from_str: {:.1}x faster ({:.0}% saved);  interned value vs fresh: {:.1}x\n",
                ns(slow) / ns(fast),
                100.0 * (ns(slow) - ns(fast)) / ns(slow),
                ns(new_str) / ns(atom_str),
            );
            Ok::<(), i32>(())
        })?;
        Ok(())
    }

    /// Phase 1 acceptance: const atoms are resolved from the static table and
    /// user atoms are reused by `(content, kind)`.
    #[test]
    fn const_atom_hit_and_user_reuse() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            Ok(())
        })?;

        ctx.with_new_scope(|cx| {
            // const hit: no registration, no GC node.
            let a = Atom::intern_str(cx, "Object")?;
            assert_eq!(a, Atom::CONST_Object);
            assert!(a.is_const());
            assert_eq!(a.const_kind(), AtomKind::String);
            assert_eq!(a.const_str(), "Object");
            assert!(cx.rt().atom_desc_string(a.id()).is_none());

            // user atom: interned twice -> same id, and distinct from consts.
            let b = Atom::intern_str(cx, "zzz_phase1_user_key")?;
            assert!(!b.is_const());
            let c = Atom::intern_str(cx, "zzz_phase1_user_key")?;
            assert_eq!(b, c, "same (content, kind) must reuse one user atom");

            // const string still yields a usable value / string ref.
            let sv = a.to_value(cx)?;
            assert_eq!(
                sv.as_string(false)
                    .unwrap()
                    .content()
                    .encode_utf8(false)
                    .as_ref(),
                "Object"
            );
            Ok::<(), i32>(())
        })?;
        Ok(())
    }

    /// Phase 1 acceptance: const Symbol atoms have a stable per-realm value
    /// identity and round-trip back to the const atom (symbol-key lookups).
    #[test]
    fn const_symbol_value_is_stable() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            Ok(())
        })?;

        ctx.with_new_scope(|cx| {
            let v1 = Atom::CONST_Symbol_iterator.to_value(cx)?;
            let v2 = Atom::CONST_Symbol_iterator.to_value(cx)?;
            assert!(v1.as_symbol(false).is_some());
            assert_eq!(v1, v2, "well-known symbol value identity must be stable");
            assert_eq!(v1.to_atom(cx)?, Atom::CONST_Symbol_iterator);

            // Private-brand const atom mints a symbol without panicking.
            assert!(
                Atom::CONST_Private_brand
                    .to_value(cx)?
                    .as_symbol(false)
                    .is_some()
            );
            Ok::<(), i32>(())
        })?;
        Ok(())
    }
}
