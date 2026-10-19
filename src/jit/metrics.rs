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
    /// Functions handed to a background worker and not yet drained
    /// (`jit_background` only). Always `0` in the inline model, where `poll`
    /// compiles the whole queue before returning.
    pub functions_compiling: u64,
    /// Calls to `Jit::poll`.
    pub polls: u64,
    /// Functions the frontend rejected (fail-closed): they stay in the
    /// interpreter forever.
    pub functions_rejected: u64,
    /// Functions that passed frontend verification.
    pub functions_verified: u64,
    /// Functions successfully compiled and installed.
    pub functions_compiled: u64,
    /// Times a compiled entry was invoked.
    pub native_entries: u64,
    /// Native entries that exited through `JitExitKind::Interrupt` (a safepoint
    /// delivered an interrupt), i.e. the interrupt unwound out of generated code.
    pub native_interrupts: u64,
    /// Compilation attempts that failed (codegen or code-cache quota).
    pub compile_failures: u64,
    /// Compilation attempts abandoned because they exceeded the time budget.
    pub compile_timeouts: u64,
    /// Functions requeued after a failed attempt.
    pub functions_retried: u64,
    /// Functions permanently parked in the interpreter: rejected, or out of
    /// compile attempts.
    pub functions_settled: u64,
    /// Deoptimizations back to the interpreter. Always `0`: D-25 rejected the
    /// whole-frame deopt / `DeoptMap` design, so a guard miss falls back to the
    /// interpreter helper *inside* the native frame instead of unwinding. The
    /// field stays for API stability but is never written.
    pub deopts: u64,
    /// Machine code bytes currently published.
    pub code_bytes: u64,
    /// Distinct tier-2 feedback sites recorded (epic F1). Zero under
    /// `Interpreter`/`Tier1` policies, which never record.
    pub feedback_sites: u64,
    /// Binary ops whose feedback was int-monomorphic and that therefore got a
    /// speculative inline int fast path instead of a helper call (epic F2).
    /// Zero under `Interpreter`/`Tier1` policies, and under `Automatic` until a
    /// site clears the profitability gate (epic F5,
    /// `JitConfig::specialization_min_samples`).
    pub specialized_ops: u64,
    /// Times an installed speculative int fast path (epic F2) bailed to its
    /// helper because the runtime value was not the assumed `Int` (P7 7.2
    /// instrumentation). Read together with `specialized_ops`: a high ratio
    /// means the tier-2 specialization is paying more guards than it saves,
    /// which is the evidence `tier-down` would need.
    pub specialized_guard_misses: u64,
}
