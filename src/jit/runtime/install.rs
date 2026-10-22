// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Publish one compiled function into executable memory and its `jit` slot.

use std::rc::Rc;

use crate::{
    function::Bytecode,
    jit::{
        code_cache::{CodeBlock, CodeCache, CodeSlot},
        platform::CodeMem,
    },
};

/// Map, publish and install `bytes` for `bytecode`. Returns `None` when nothing
/// was installed (the mapping failed or the artifact does not fit the whole
/// budget); otherwise the ids of the live mappings the cache retired to make
/// room, which the caller resets to `Cold` so they can recompile. The
/// function's slot owns the mapping (D-16); the cache only tracks its bytes.
pub(crate) fn publish(
    cache: &mut CodeCache,
    id: u64,
    bytecode: &Bytecode,
    bytes: &[u8],
) -> Option<Vec<u64>> {
    let Ok(mem) = CodeMem::alloc(bytes.len()) else {
        return None;
    };
    if mem.write(bytes).is_err() || mem.make_executable().is_err() {
        return None;
    }
    let block = Rc::new(CodeBlock::new(mem));
    let slot = Rc::new(CodeSlot::new(block, cache.clock()));
    let evicted = cache.insert(id, &slot).ok()?;
    bytecode.jit.install(slot);
    Some(evicted)
}
