// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Publish one compiled function into executable memory and its `jit` slot.

use std::rc::Rc;

use crate::{
    function::Bytecode,
    jit::{
        code_cache::{CodeBlock, CodeCache, CodeSlot, Retired},
        platform::CodeMem,
    },
};

/// What one publish did, for the counters.
pub(crate) struct Published {
    /// Live mappings the cache retired to make room; the caller resets them to
    /// `Cold` so they can recompile.
    pub(crate) evicted: Retired,
    /// The install replaced existing code.
    pub(crate) replaced: bool,
    /// The replaced code had never been entered.
    pub(crate) replaced_never_entered: bool,
}

/// Map, publish and install `bytes` for `bytecode`. Returns `None` when nothing
/// was installed (the mapping failed or the artifact does not fit the whole
/// budget). The function's slot owns the mapping; the cache only tracks its
/// bytes.
pub(crate) fn publish(
    cache: &mut CodeCache,
    id: u64,
    bytecode: &Bytecode,
    bytes: &[u8],
) -> Option<Published> {
    let Ok(mem) = CodeMem::alloc(bytes.len()) else {
        return None;
    };
    if mem.write(bytes).is_err() || mem.make_executable().is_err() {
        return None;
    }
    let block = Rc::new(CodeBlock::new(mem));
    let slot = Rc::new(CodeSlot::new(block, cache.clock()));
    let evicted = cache.insert(id, &slot).ok()?;
    let replaced = bytecode.jit.install(slot);
    Some(Published {
        evicted,
        replaced: replaced.is_some(),
        replaced_never_entered: replaced.is_some_and(|old| !old.entered()),
    })
}
