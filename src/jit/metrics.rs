// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Observable counters for a running JIT.

/// The upper edges of the compile-time histogram, in nanoseconds. Bucket `i`
/// counts the attempts that took **less** than `COMPILE_BUCKET_EDGES_NS[i]`;
/// the last bucket counts the rest (the overflow bucket).
///
/// The edges straddle the measured compile cost (1.0–1.4 ms per function,
/// plan §13.6.4), so a run shows whether the cost is dominated by a few slow
/// functions or is spread out.
pub const COMPILE_BUCKET_EDGES_NS: [u64; 5] = [
    100_000,    // < 0.1 ms
    300_000,    // < 0.3 ms
    1_000_000,  // < 1 ms
    3_000_000,  // < 3 ms
    10_000_000, // < 10 ms
];

/// The number of histogram buckets: one per edge plus the overflow bucket.
pub const COMPILE_BUCKETS: usize = COMPILE_BUCKET_EDGES_NS.len() + 1;

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
    /// Functions that were compiled again over existing code (a promotion to
    /// the optimizing tier, or a tier-down). The compile count minus this is
    /// the number of functions that got native code for the first time.
    pub functions_recompiled: u64,
    /// Functions promoted to the optimizing tier (plan §13.6.5 item 1): the
    /// second gate fired and the function was recompiled with the tier-2
    /// specialization.
    pub tier2_promotions: u64,
    /// Functions demoted by the tier-down (P7 7.2): their speculative fast
    /// paths missed too often, so they were recompiled without speculation.
    pub tier_downs: u64,
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
    /// Property accesses the inline guard answered on its own (P1-1): an own
    /// data property, read or written straight out of the object's slot.
    ///
    /// Read together with [`Self::property_guard_misses`]: a hit is the guard
    /// paying off, a miss is an access the helper had to own (a prototype-chain
    /// property, an accessor, a non-object target, …). A site that never hits
    /// is a site the guard cannot serve — the reason to look at *what* it
    /// touches.
    pub property_guard_hits: u64,
    /// Property accesses the inline guard handed back to the helper (P1-1).
    ///
    /// A miss is expected and common (it is not a defect), so it never feeds
    /// the tier-down — that is [`Self::specialized_guard_misses`]' job.
    pub property_guard_misses: u64,
    /// Calls at a monomorphic site that took the direct path (D-6 route B):
    /// the callee's compiled code was entered without the interpreter's call
    /// protocol.
    pub inline_call_hits: u64,
    /// Calls at a monomorphic site that took the general call path (D-6 route
    /// B): the guard missed (the callee was not the recorded one), or the
    /// callee could not run directly (a generator, a cross-realm callee, one
    /// that allocates GC nodes).
    pub inline_call_misses: u64,
    /// Nanoseconds spent compiling (verification + codegen), summed over every
    /// attempt. The publish step (mmap / mprotect) is *not* included: it is
    /// not a compile.
    pub compile_nanos: u64,
    /// Compile attempts that were measured. An attempt that the frontend
    /// rejected counts too — it cost a verification.
    pub compile_attempts: u64,
    /// Histogram of the per-attempt compile time: bucket `i` counts the
    /// attempts shorter than `COMPILE_BUCKET_EDGES_NS[i]`, the last one the
    /// rest.
    pub compile_histogram: [u64; COMPILE_BUCKETS],
}

impl JitMetrics {
    /// Account for one compile attempt that took `nanos`.
    pub(crate) fn note_compile(&mut self, nanos: u64) {
        self.compile_nanos = self.compile_nanos.saturating_add(nanos);
        self.compile_attempts = self.compile_attempts.saturating_add(1);
        let bucket = COMPILE_BUCKET_EDGES_NS
            .iter()
            .position(|edge| nanos < *edge)
            .unwrap_or(COMPILE_BUCKETS - 1);
        self.compile_histogram[bucket] = self.compile_histogram[bucket].saturating_add(1);
    }

    /// The mean compile time per attempt, in nanoseconds. `0` when nothing
    /// was compiled.
    pub const fn avg_compile_nanos(&self) -> u64 {
        if self.compile_attempts == 0 {
            return 0;
        }
        self.compile_nanos / self.compile_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_histogram_buckets_by_duration() {
        let mut m = JitMetrics::default();
        m.note_compile(1_000); // < 0.1 ms
        m.note_compile(200_000); // < 0.3 ms
        m.note_compile(900_000); // < 1 ms
        m.note_compile(2_000_000); // < 3 ms
        m.note_compile(9_000_000); // < 10 ms
        m.note_compile(90_000_000); // overflow
        assert_eq!(m.compile_histogram, [1, 1, 1, 1, 1, 1]);
        assert_eq!(m.compile_attempts, 6);
        assert_eq!(
            m.compile_nanos,
            1_000 + 200_000 + 900_000 + 2_000_000 + 9_000_000 + 90_000_000
        );
    }

    #[test]
    fn the_average_is_zero_without_a_compile() {
        let mut m = JitMetrics::default();
        assert_eq!(m.avg_compile_nanos(), 0);
        m.note_compile(1_000_000);
        assert_eq!(m.avg_compile_nanos(), 1_000_000);
    }
}
