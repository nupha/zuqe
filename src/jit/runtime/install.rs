// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Publish one compiled function into executable memory and its `jit` slot.

use crate::{
    function::Bytecode,
    jit::{abi::NativeEntry, code_cache::CodeCache, platform::CodeMem},
};

/// Map, publish and install `bytes` for `bytecode`. Returns `false` when the
/// code cache is at its byte quota, in which case nothing is installed and the
/// caller records a failed attempt.
pub(crate) fn publish(
    cache: &mut CodeCache,
    id: u64,
    bytecode: &Bytecode,
    bytes: &[u8],
) -> bool {
    let Ok(mem) = CodeMem::alloc(bytes.len()) else {
        return false;
    };
    if mem.write(bytes).is_err() || mem.make_executable().is_err() {
        return false;
    }
    // SAFETY: `mem.entry()` is the start of the just-published code, produced
    // by `compiler::compile` for the same `NativeEntry` ABI. The code cache
    // keeps the mapping alive for as long as the entry can be called.
    let entry: NativeEntry = unsafe { std::mem::transmute(mem.entry()) };
    if cache.insert(id, mem).is_err() {
        return false;
    }
    bytecode.jit_slot().install(entry);
    true
}
