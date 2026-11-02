// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Observable counters for a running JIT.

use super::{
    abi::{JitExecFrame, JitExitKind},
    runtime::Published,
};

/// The upper edges of the compile-time histogram, in nanoseconds. Bucket `i`
/// counts the attempts that took **less** than `COMPILE_BUCKET_EDGES_NS[i]`;
/// the last bucket counts the rest (the overflow bucket).
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
    /// Interpreter function-entry hook invocations (one per call into a
    /// JIT-tracked function).
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
    /// Functions handed to a compiler worker and not yet drained.
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
    /// Functions promoted to the optimizing tier: the
    /// second gate fired and the function was recompiled with the tier-2
    /// specialization.
    pub tier2_promotions: u64,
    /// Functions demoted by a tier-down: their speculative fast
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
    /// Machine code bytes currently published.
    pub code_bytes: u64,
    /// Mappings the code cache retired to stay within its byte budget. Each one
    /// handed its function back to the interpreter.
    pub code_retired: u64,
    /// Installed mappings replaced by a newer compile of the same function (a
    /// tier-2 promotion, or a tier-down).
    pub code_replaced: u64,
    /// Installations that never ran: retired or replaced before a single
    /// native entry. A high count means the compile budget went to code
    /// nobody called.
    pub code_never_entered: u64,
    /// Distinct tier-2 feedback sites recorded. Zero under
    /// `Interpreter`/`Tier1` policies, which never record.
    pub feedback_sites: u64,
    /// Binary ops whose feedback was int-monomorphic and that therefore got a
    /// speculative inline int fast path instead of a helper call. Zero under
    /// `Interpreter`/`Tier1`, and under `Automatic` until a site clears the
    /// profitability gate (`JitConfig::specialization_min_samples`).
    pub specialized_ops: u64,
    /// Times an installed speculative int fast path bailed to its helper
    /// because the runtime value was not the assumed `Int`. Read together with
    /// [`Self::specialized_ops`]: a high ratio means the specialization pays
    /// more guards than it saves.
    pub specialized_guard_misses: u64,
    /// Property accesses the inline guard answered on its own: an own data
    /// property, read or written straight out of the object's slot. Read
    /// together with [`Self::property_guard_misses`] — a miss is an access the
    /// helper had to own (a prototype-chain property, an accessor, a
    /// non-object target, …), so a site that never hits is a site the guard
    /// cannot serve.
    pub property_guard_hits: u64,
    /// Property accesses the inline guard handed back to the helper. A miss is
    /// expected and common, so it never feeds the tier-down — that is
    /// [`Self::specialized_guard_misses`]' job.
    pub property_guard_misses: u64,
    /// Global-variable accesses the inline guard answered on its own: an own
    /// data property of `global_obj` (`var`/`function`) or `global_var_obj`
    /// (`let`/`const`), read or written straight out of its slot.
    pub global_guard_hits: u64,
    /// Global-variable accesses handed back to the helper: an unbound name, a
    /// TDZ slot, a read-only binding, an accessor, or a shape change.
    pub global_guard_misses: u64,
    /// Calls at a monomorphic site that took the fast path (the direct-call path):
    /// either the callee's compiled code was entered directly or the
    /// inline guard passed and the copied body ran in the caller.
    pub mono_call_hits: u64,
    /// Calls whose site passed the inline guard and ran the callee's body
    /// inside the caller (the inlined call path). The body may still bail to the
    /// general path on an operand the inline subset does not implement; this
    /// counter only says the guard was passed.
    pub inline_body_hits: u64,
    /// Nested call sites inside a copied body whose constant-identity guard
    /// passed (the inlined call path).
    pub nested_inline_hits: u64,
    /// Calls at a monomorphic site that took the general call path: the guard
    /// missed (the callee was not the recorded one), or the
    /// callee could not run directly (a generator, a cross-realm callee, one
    /// that allocates GC nodes).
    pub mono_call_misses: u64,
    /// Observability for the scope's LOCAL list: the largest one seen at a JIT
    /// safepoint, sampled right before a deferred collection clears it. It is a
    /// high-water mark (a scope that is never cleared grows without bound and
    /// lifts it with the iteration count), so a caller-scope leak shows up in
    /// the bench output.
    pub scope_local_peak: u64,
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

    /// Account for what one publish did: the replacement over existing code,
    /// the mappings the cache retired to fit, and the code nobody ever
    /// entered (reported separately from the compile itself).
    pub(crate) fn note_published(&mut self, published: &Published) {
        if published.replaced {
            self.code_replaced = self.code_replaced.saturating_add(1);
        }
        if published.replaced_never_entered {
            self.code_never_entered = self.code_never_entered.saturating_add(1);
        }
        self.code_retired = self
            .code_retired
            .saturating_add(published.evicted.ids.len() as u64);
        self.code_never_entered = self
            .code_never_entered
            .saturating_add(published.evicted.never_entered);
    }

    /// Fold one native activation's counters in: the entry (and whether it
    /// exited through an interrupt) plus the instrumentation the generated code
    /// kept in the frame.
    pub(crate) fn note_native_exit(&mut self, kind: JitExitKind, frame: &JitExecFrame) {
        self.native_entries = self.native_entries.saturating_add(1);
        self.native_interrupts = self
            .native_interrupts
            .saturating_add(u64::from(kind == JitExitKind::Interrupt));
        // Tier-down: this activation's speculative guard misses.
        self.specialized_guard_misses = self
            .specialized_guard_misses
            .saturating_add(u64::from(frame.guard_misses));
        self.property_guard_hits = self
            .property_guard_hits
            .saturating_add(u64::from(frame.property_guard_hits));
        self.property_guard_misses = self
            .property_guard_misses
            .saturating_add(u64::from(frame.property_guard_misses));
        self.global_guard_hits = self
            .global_guard_hits
            .saturating_add(u64::from(frame.global_guard_hits));
        self.global_guard_misses = self
            .global_guard_misses
            .saturating_add(u64::from(frame.global_guard_misses));
        self.mono_call_hits = self
            .mono_call_hits
            .saturating_add(u64::from(frame.mono_call_hits));
        self.inline_body_hits = self
            .inline_body_hits
            .saturating_add(u64::from(frame.inline_body_hits));
        self.nested_inline_hits = self
            .nested_inline_hits
            .saturating_add(u64::from(frame.nested_inline_hits));
        self.mono_call_misses = self
            .mono_call_misses
            .saturating_add(u64::from(frame.mono_call_misses));
        // A high-water mark, so the max — not a sum.
        self.scope_local_peak = self.scope_local_peak.max(u64::from(frame.scope_local_peak));
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
