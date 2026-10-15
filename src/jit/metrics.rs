// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Observable counters for a running JIT.

/// A point-in-time snapshot of the JIT's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitMetrics {
    /// Native-entry hook invocations (one per call into a JIT-tracked function).
    pub function_entries: u64,
    /// Distinct functions seen at entry.
    pub distinct_functions: u64,
    /// Backward branches taken in tracked functions.
    pub backedges: u64,
    /// Functions queued for compilation.
    pub functions_queued: u64,
    /// Queue insertions dropped because the queue was full.
    pub queue_drops: u64,
    /// Functions currently waiting to be compiled.
    pub queue_len: u64,
    /// Calls to `Jit::poll`.
    pub polls: u64,
    /// Functions the compiler rejected (fail-closed). Reserved for later epics.
    pub functions_rejected: u64,
    /// Functions successfully compiled. Reserved for later epics.
    pub functions_compiled: u64,
    /// Compilation attempts that failed. Reserved for later epics.
    pub compile_failures: u64,
    /// Deoptimizations back to the interpreter. Reserved for later epics.
    pub deopts: u64,
    /// Machine code bytes published. Reserved for later epics.
    pub code_bytes: u64,
}
