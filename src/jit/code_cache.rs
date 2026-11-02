// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Published machine-code memory, bounded by a byte quota.
//!
//! Ownership is split: the function's [`FunctionSlot`] holds the
//! strong `Rc<CodeSlot>`, and the cache only keeps a `Weak`. A mapping can
//! therefore only be freed (a) when the last `BytecodeImpl` that could call it
//! is collected, or (b) by an explicit eviction that clears the slot first.
//! Eviction only ever runs from `Jit::poll`, between native activations, so no
//! in-flight frame is holding the entry — the only point where "no stale entry
//! pointer can be called" is knowable. Retiring a live entry hands the function
//! back to the interpreter; the caller resets its hotness so it recompiles if it
//! becomes hot again (that is also the channel `tier-down` would use).
//!
//! [`FunctionSlot`]: crate::jit::FunctionSlot

use std::{
    cell::{Cell, UnsafeCell},
    collections::HashMap,
    rc::{Rc, Weak},
};

use super::platform::CodeMem;

/// The cache could not accept an artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheError {
    /// The artifact does not fit in the remaining byte budget.
    ArtifactTooLarge,
}

/// One published executable mapping. Owned by a [`CodeSlot`].
pub(crate) struct CodeBlock {
    mem: CodeMem,
    bytes: usize,
}

impl CodeBlock {
    #[inline(always)]
    pub(crate) const fn new(mem: CodeMem) -> Self {
        Self {
            bytes: mem.len(),
            mem,
        }
    }

    #[inline(always)]
    pub(crate) const fn len(&self) -> usize {
        self.bytes
    }

    /// Start of the published code, i.e. the `NativeEntry` address.
    #[inline(always)]
    pub(crate) const fn entry(&self) -> *const u8 {
        self.mem.entry()
    }
}

/// The strong owner of one function's published code, held by its
/// [`FunctionSlot`] and referred to weakly by [`CodeCache`]. `None` after an
/// eviction: the function falls back to the interpreter.
pub(crate) struct CodeSlot {
    code: UnsafeCell<Option<Rc<CodeBlock>>>,
    /// Shared monotonic clock; bumped on every use so the cache can pick the
    /// least recently used live mapping.
    clock: Rc<Cell<u64>>,
    last_use: Cell<u64>,
    /// Whether the mapping was ever entered. Retiring or replacing code that
    /// never ran is compile budget spent for nothing.
    entered: Cell<bool>,
}

impl CodeSlot {
    pub(crate) fn new(code: Rc<CodeBlock>, clock: Rc<Cell<u64>>) -> Self {
        let now = clock.get();
        Self {
            code: UnsafeCell::new(Some(code)),
            clock,
            last_use: Cell::new(now),
            entered: Cell::new(false),
        }
    }

    /// The entry pointer, refreshing the LRU stamp. `None` once evicted.
    ///
    /// # Safety
    /// Single-threaded, like the rest of the JIT: the write is a short,
    /// non-reentrant update with no aliasing reference held across it.
    pub(crate) fn entry(&self) -> Option<*const u8> {
        self.entered.set(true);
        let now = self.clock.get().wrapping_add(1);
        self.clock.set(now);
        self.last_use.set(now);
        // SAFETY: see above.
        unsafe { &*self.code.get() }
            .as_ref()
            .map(|code| code.entry())
    }

    /// Whether this mapping was ever entered.
    #[inline(always)]
    pub(crate) const fn entered(&self) -> bool {
        self.entered.get()
    }

    #[inline(always)]
    pub(crate) const fn has_code(&self) -> bool {
        // SAFETY: single-threaded read.
        unsafe { &*self.code.get() }.is_some()
    }

    #[inline]
    fn bytes(&self) -> usize {
        // SAFETY: single-threaded read.
        unsafe { &*self.code.get() }
            .as_ref()
            .map_or(0, |code| code.len())
    }

    #[inline(always)]
    const fn last_use(&self) -> u64 {
        self.last_use.get()
    }

    /// Retire the code. The mapping is unmapped as soon as the last
    /// `Rc<CodeBlock>` goes away.
    #[inline(always)]
    fn evict(&self) {
        // SAFETY: single-threaded; the caller is `Jit::poll`, between native
        // activations.
        unsafe { (*self.code.get()).take() };
    }
}

struct CachedCode {
    slot: Weak<CodeSlot>,
    bytes: usize,
}

/// The live mappings one insert retired to make room, and how many of them
/// never ran.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Retired {
    pub(crate) ids: Vec<u64>,
    /// How many of those mappings had never been entered.
    pub(crate) never_entered: u64,
}

/// Tracks all published code and enforces `max_bytes`.
pub(crate) struct CodeCache {
    max_bytes: usize,
    used: usize,
    entries: HashMap<u64, CachedCode>,
    clock: Rc<Cell<u64>>,
}

impl CodeCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            used: 0,
            entries: HashMap::new(),
            clock: Rc::new(Cell::new(0)),
        }
    }

    /// The shared use clock a new [`CodeSlot`] must be built with.
    pub(crate) fn clock(&self) -> Rc<Cell<u64>> {
        Rc::clone(&self.clock)
    }

    /// Register `slot` for function `id`. When the budget is short the least
    /// recently used live mapping is retired first (its slot cleared, so the
    /// function falls back to the interpreter); the retired ids are returned so
    /// the caller can reset their hotness. `Err` only when the artifact alone
    /// does not fit the whole budget.
    pub(crate) fn insert(&mut self, id: u64, slot: &Rc<CodeSlot>) -> Result<Retired, CacheError> {
        self.sweep();
        let bytes = slot.bytes();
        let mut retired = Retired::default();
        while bytes > self.max_bytes.saturating_sub(self.used) {
            let Some(victim) = self.lru_victim() else {
                break;
            };
            if self.retire(victim) {
                retired.never_entered = retired.never_entered.saturating_add(1);
            }
            retired.ids.push(victim);
        }
        if bytes > self.max_bytes.saturating_sub(self.used) {
            return Err(CacheError::ArtifactTooLarge);
        }
        if let Some(old) = self.entries.insert(
            id,
            CachedCode {
                slot: Rc::downgrade(slot),
                bytes,
            },
        ) {
            self.used = self.used.saturating_sub(old.bytes);
        }
        self.used = self.used.saturating_add(bytes);
        Ok(retired)
    }

    /// Drop every entry whose owner is already gone and return the reclaimed
    /// bytes. A retired entry's old `NativeEntry` can no longer be called: the
    /// only way to reach it was through the owner that just died.
    pub(crate) fn reclaim(&mut self) -> usize {
        let before = self.used;
        self.sweep();
        before - self.used
    }

    fn sweep(&mut self) {
        // `strong_count == 0` is exactly "no `FunctionSlot` still holds it";
        // `upgrade` would needlessly clone a live `Rc`.
        let dead: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.slot.strong_count() == 0)
            .map(|(&id, _)| id)
            .collect();
        for id in dead {
            if let Some(entry) = self.entries.remove(&id) {
                self.used = self.used.saturating_sub(entry.bytes);
            }
        }
    }

    /// The live mapping used least recently, or `None` when every entry is
    /// either already empty or dead. Ties break on the id for determinism.
    fn lru_victim(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter_map(|(&id, entry)| {
                let slot = entry.slot.upgrade()?;
                slot.has_code().then_some((slot.last_use(), id))
            })
            .min_by_key(|&(last_use, id)| (last_use, id))
            .map(|(_, id)| id)
    }

    /// Clear `id`'s slot and drop its accounting, so the freed bytes are usable.
    /// Returns whether the retired mapping had never been entered.
    fn retire(&mut self, id: u64) -> bool {
        let Some(entry) = self.entries.remove(&id) else {
            return false;
        };
        self.used = self.used.saturating_sub(entry.bytes);
        let Some(slot) = entry.slot.upgrade() else {
            return false;
        };
        let unused = !slot.entered();
        slot.evict();
        unused
    }

    pub(crate) const fn used_bytes(&self) -> u64 {
        self.used as u64
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, id: u64) -> bool {
        self.entries.contains_key(&id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(cache: &CodeCache, bytes: usize) -> Rc<CodeSlot> {
        Rc::new(CodeSlot::new(
            Rc::new(CodeBlock::new(CodeMem::alloc(bytes).unwrap())),
            cache.clock(),
        ))
    }

    #[test]
    fn refuses_an_artifact_larger_than_the_whole_budget() {
        let mut cache = CodeCache::new(8 * 1024);
        let too_big = slot(&cache, 16 * 1024);
        assert_eq!(cache.insert(1, &too_big), Err(CacheError::ArtifactTooLarge));
        assert_eq!(cache.used_bytes(), 0);
        assert!(!cache.contains(1));
    }

    /// Single-owner retirement of a live function: a short budget clears the least
    /// recently used slot instead of refusing the compile. The cleared slot
    /// holds no entry, so the function can only fall back to the interpreter —
    /// its old `NativeEntry` can no longer be called.
    #[test]
    fn a_short_budget_retires_the_live_lru_entry() {
        let mut cache = CodeCache::new(8 * 1024);
        let first = slot(&cache, 8 * 1024);
        assert_eq!(cache.insert(1, &first), Ok(Retired::default()));
        assert_eq!(cache.used_bytes(), 8 * 1024);

        let second = slot(&cache, 1);
        let evicted = cache.insert(2, &second).expect("live retire");
        assert_eq!(evicted.ids, vec![1]);
        assert_eq!(
            evicted.never_entered, 1,
            "the retired mapping never ran a native entry"
        );
        assert!(!cache.contains(1));
        assert_eq!(cache.used_bytes(), second.bytes() as u64);

        assert!(first.entry().is_none(), "the retired entry is not callable");
        assert!(!first.has_code());
    }

    /// Single-owner retirement when the owner is collected: its bytes come back and the
    /// entry disappears without any eviction.
    #[test]
    fn a_dropped_owner_reclaims_its_bytes() {
        let mut cache = CodeCache::new(8 * 1024);
        let first = slot(&cache, 8 * 1024);
        assert_eq!(cache.insert(1, &first), Ok(Retired::default()));

        // Live owner: nothing to reclaim.
        assert_eq!(cache.reclaim(), 0);
        // Owner collected (the `BytecodeImpl` dropped): the mapping is freed.
        drop(first);
        assert_eq!(cache.reclaim(), 8 * 1024);
        assert_eq!(cache.used_bytes(), 0);
        assert!(!cache.contains(1));

        // The reclaimed budget is usable again.
        let second = slot(&cache, 8 * 1024);
        assert_eq!(cache.insert(2, &second), Ok(Retired::default()));
    }

    /// Re-inserting the same function replaces the old mapping's accounting
    /// instead of double-counting it.
    #[test]
    fn reinstalling_the_same_id_replaces_its_bytes() {
        let mut cache = CodeCache::new(8 * 1024);
        let first = slot(&cache, 64);
        assert_eq!(cache.insert(7, &first), Ok(Retired::default()));
        let second = slot(&cache, 64);
        assert_eq!(cache.insert(7, &second), Ok(Retired::default()));
        // One page either way (`CodeMem` rounds up), and only one entry.
        assert_eq!(cache.used_bytes(), second.bytes() as u64);
        assert_eq!(cache.len(), 1);
    }

    /// The LRU stamp decides the victim: a recently used entry survives a
    /// retire of the older one.
    #[test]
    fn the_least_recently_used_entry_is_retired() {
        let mut cache = CodeCache::new(16 * 1024);
        let first = slot(&cache, 8 * 1024);
        let second = slot(&cache, 8 * 1024);
        assert_eq!(cache.insert(1, &first), Ok(Retired::default()));
        assert_eq!(cache.insert(2, &second), Ok(Retired::default()));
        // Touch `1`, so `2` (inserted later) is the LRU one.
        first.entry();
        let third = slot(&cache, 1);
        assert_eq!(cache.insert(3, &third).unwrap().ids, vec![2]);
        assert!(first.has_code(), "the recently used entry stays");
        assert!(!second.has_code());
    }
}
