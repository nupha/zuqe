// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Published machine-code memory, bounded by a byte quota (epic D4).
//!
//! The cache owns every executable mapping for the JIT's lifetime, so an
//! installed entry stays valid until the runtime is torn down. There is no
//! eviction yet: a mapping may only be freed once no `BytecodeImpl` still
//! holds its entry pointer, and the JIT has no safe way to prove that today.
//! When the quota is reached, new compiles are refused instead — the frontend
//! retries and then settles the function in the interpreter.
//!
//! `invalidate`/LRU eviction arrive with the generation scheme and the
//! profitability model (epic F5); see the plan's D-15.

use std::collections::HashMap;

use super::platform::CodeMem;

/// The cache could not accept an artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheError {
    /// The artifact does not fit in the remaining byte budget.
    ArtifactTooLarge,
}

struct CachedCode {
    /// Kept solely so the mapping outlives the cache; the entry pointer is
    /// stored in the function's `jit` slot.
    _mem: CodeMem,
    bytes: usize,
}

/// Owns all published code and enforces `max_bytes`.
pub(crate) struct CodeCache {
    max_bytes: usize,
    used: usize,
    entries: HashMap<u64, CachedCode>,
}

impl CodeCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            used: 0,
            entries: HashMap::new(),
        }
    }

    /// Take ownership of `mem` for function `id`. `mem` is dropped (unmapped)
    /// if the quota would be exceeded.
    pub(crate) fn insert(&mut self, id: u64, mem: CodeMem) -> Result<(), CacheError> {
        let bytes = mem.len();
        if bytes > self.max_bytes.saturating_sub(self.used) {
            return Err(CacheError::ArtifactTooLarge);
        }
        if let Some(old) = self.entries.insert(
            id,
            CachedCode {
                _mem: mem,
                bytes,
            },
        ) {
            self.used = self.used.saturating_sub(old.bytes);
        }
        self.used = self.used.saturating_add(bytes);
        Ok(())
    }

    pub(crate) const fn used_bytes(&self) -> u64 {
        self.used as u64
    }

    #[allow(dead_code)]
    pub(crate) fn contains(&self, id: u64) -> bool {
        self.entries.contains_key(&id)
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(bytes: usize) -> CodeMem {
        CodeMem::alloc(bytes).unwrap()
    }

    #[test]
    fn refuses_artifacts_over_the_quota() {
        let mut cache = CodeCache::new(8 * 1024);
        assert!(cache.insert(1, mem(8 * 1024)).is_ok());
        assert_eq!(cache.used_bytes(), 8 * 1024);
        assert_eq!(cache.insert(2, mem(1)), Err(CacheError::ArtifactTooLarge));
        assert_eq!(cache.len(), 1);
    }
}
