// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Compilation queue and per-function lifecycle (epic D2).
//!
//! Hotness decides *when* a function is worth compiling; this module owns the
//! request queue and the states a function moves through. Verification and
//! codegen run inline in `JitState::poll` (epic D3 is a documented deviation —
//! no background thread yet), so the coordinator itself never touches code.
//!
//! While a function is queued or compiling the coordinator pins its `Bytecode`
//! handle: the GC traces pinned handles (`JitState::trace`), so the bytecode
//! and its `jit` slot stay alive until the compile result has been written into
//! them. Once installed (or settled) the pin is dropped — from then on the slot
//! lives and dies with the function itself.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{function::Bytecode, jit::metrics::JitMetrics};

use super::hotness::{AdaptiveInputs, HotThresholds, HotnessState};

/// Where a function is in its compile lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    /// Seen but not hot yet.
    Cold,
    /// Waiting in the queue.
    Queued,
    /// Handed to the (inline) pipeline.
    Compiling,
    /// Native code is installed in the function's slot.
    Installed,
    /// Never compiled again: rejected by the frontend, or out of attempts.
    Settled,
}

/// How one compile attempt ended. `Installed`/`Failed`/`TimedOut` all imply the
/// frontend accepted the function (verify passed); `Rejected` means it refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompileOutcome {
    Installed,
    Rejected,
    Failed,
    TimedOut,
}

/// One function ready to compile. `bytecode` keeps the function pinned for the
/// duration of the attempt.
pub(crate) struct Request {
    pub(crate) function_id: u64,
    pub(crate) bytecode: Bytecode,
}

struct FunctionRecord {
    hotness: HotnessState,
    thresholds: HotThresholds,
    lifecycle: Lifecycle,
    attempts: u32,
    /// Whether `functions_verified`/`functions_rejected` already counted this
    /// function. Verification is deterministic, so it is counted once even
    /// when a later codegen failure retries it.
    verify_counted: bool,
    /// Held while `Queued`/`Compiling` so the GC cannot free the function.
    pinned: Option<Bytecode>,
    /// Speculative guard misses observed in this function's native code (P7
    /// 7.2). Past [`TIER_DOWN_MISSES`] the function is recompiled without
    /// speculation.
    guard_misses: u64,
    /// Tier-down requested: the next compile must not speculate.
    tier_down: bool,
    /// Whether the *currently installed* code may speculate. Cleared once a
    /// tier-down recompile installs, so the function is not re-queued forever.
    speculate: bool,
}

/// P7 7.2: how many speculative guard misses one function may take before it is
/// recompiled without speculation. Shortcut: a fixed threshold instead of a
/// per-site profitability model; raise/lower it if the guard-miss metric shows
/// the wrong trade-off.
pub(crate) const TIER_DOWN_MISSES: u64 = 64;

/// The request queue plus the lifecycle of every function the JIT has seen.
pub(crate) struct Coordinator {
    records: HashMap<u64, FunctionRecord>,
    queue: VecDeque<u64>,
    base_calls: u32,
    base_loops: u32,
    max_queue_len: usize,
    max_attempts: u32,
    timeout: Duration,
}

impl Coordinator {
    pub(crate) fn new(
        base_calls: u32,
        base_loops: u32,
        max_queue_len: usize,
        max_attempts: u32,
        compile_timeout_ms: u64,
    ) -> Self {
        Self {
            records: HashMap::new(),
            queue: VecDeque::new(),
            base_calls,
            base_loops,
            max_queue_len,
            max_attempts,
            timeout: Duration::from_millis(compile_timeout_ms),
        }
    }

    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Count a function entry. Returns true when it just became hot and the
    /// caller should [`Self::enqueue`] it.
    pub(crate) fn record_call(
        &mut self,
        m: &mut JitMetrics,
        id: u64,
        opcodes: &[u8],
        measured_work: u32,
    ) -> bool {
        let first = !self.records.contains_key(&id);
        if first {
            m.distinct_functions = m.distinct_functions.saturating_add(1);
        }
        let record = self.record_mut(id, opcodes, measured_work);
        record.hotness.record_call(&record.thresholds)
    }

    /// Count a taken backward branch; same contract as `record_call`.
    pub(crate) fn record_loop(
        &mut self,
        _m: &mut JitMetrics,
        id: u64,
        opcodes: &[u8],
        measured_work: u32,
    ) -> bool {
        let record = self.record_mut(id, opcodes, measured_work);
        record.hotness.record_loop(&record.thresholds)
    }

    fn record_mut(&mut self, id: u64, opcodes: &[u8], measured_work: u32) -> &mut FunctionRecord {
        let (base_calls, base_loops) = (self.base_calls, self.base_loops);
        self.records.entry(id).or_insert_with(|| {
            let thresholds = AdaptiveInputs::from_opcodes(opcodes)
                .with_measured_work(measured_work)
                .thresholds(base_calls, base_loops);
            FunctionRecord {
                hotness: HotnessState::default(),
                thresholds,
                lifecycle: Lifecycle::Cold,
                attempts: 0,
                verify_counted: false,
                pinned: None,
                guard_misses: 0,
                tier_down: false,
                speculate: true,
            }
        })
    }

    /// Move a now-hot function into the queue. A full queue drops the request
    /// but leaves the function marked queued, so it is not re-snapshotted on
    /// every subsequent entry. `bytecode` is `None` only in unit tests.
    pub(crate) fn enqueue(&mut self, m: &mut JitMetrics, id: u64, bytecode: Option<Bytecode>) {
        if self.queue.len() >= self.max_queue_len {
            m.queue_drops = m.queue_drops.saturating_add(1);
            return;
        }
        let Some(record) = self.records.get_mut(&id) else {
            return;
        };
        record.pinned = bytecode;
        record.lifecycle = Lifecycle::Queued;
        self.queue.push_back(id);
        m.functions_queued = m.functions_queued.saturating_add(1);
        m.queue_len = self.queue.len() as u64;
    }

    /// Take the next queued function. `None` when the queue is empty.
    pub(crate) fn begin_next(&mut self) -> Option<Request> {
        while let Some(id) = self.queue.pop_front() {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            if !matches!(record.lifecycle, Lifecycle::Queued) {
                continue;
            }
            let Some(bytecode) = record.pinned else {
                continue;
            };
            record.lifecycle = Lifecycle::Compiling;
            return Some(Request {
                function_id: id,
                bytecode,
            });
        }
        None
    }

    /// Record the result of one attempt, retrying or settling as configured.
    pub(crate) fn complete(&mut self, m: &mut JitMetrics, id: u64, outcome: CompileOutcome) {
        let mut requeue = false;
        {
            let Some(record) = self.records.get_mut(&id) else {
                return;
            };
            match outcome {
                CompileOutcome::Installed => {
                    count_verified(m, record);
                    record.lifecycle = Lifecycle::Installed;
                    record.pinned = None;
                    // A tier-down recompile has landed: the installed code no
                    // longer speculates, so stop re-queueing it.
                    if record.tier_down {
                        record.speculate = false;
                    }
                    m.functions_compiled = m.functions_compiled.saturating_add(1);
                }
                CompileOutcome::Rejected => {
                    if !record.verify_counted {
                        record.verify_counted = true;
                        m.functions_rejected = m.functions_rejected.saturating_add(1);
                    }
                    record.lifecycle = Lifecycle::Settled;
                    record.pinned = None;
                }
                CompileOutcome::Failed | CompileOutcome::TimedOut => {
                    count_verified(m, record);
                    if outcome == CompileOutcome::TimedOut {
                        m.compile_timeouts = m.compile_timeouts.saturating_add(1);
                    } else {
                        m.compile_failures = m.compile_failures.saturating_add(1);
                    }
                    record.attempts += 1;
                    if record.attempts >= self.max_attempts {
                        record.lifecycle = Lifecycle::Settled;
                        record.pinned = None;
                        m.functions_settled = m.functions_settled.saturating_add(1);
                    } else {
                        record.lifecycle = Lifecycle::Queued;
                        requeue = true;
                        m.functions_retried = m.functions_retried.saturating_add(1);
                    }
                }
            }
        }
        if requeue {
            self.queue.push_back(id);
        }
    }

    /// Return a function to `Cold` after its code was discarded, so it can be
    /// compiled again if it becomes hot. Called when the code cache retires a
    /// live mapping (D-16).
    pub(crate) fn invalidate(&mut self, id: u64) {
        if let Some(record) = self.records.get_mut(&id) {
            record.lifecycle = Lifecycle::Cold;
            record.pinned = None;
            record.attempts = 0;
            record.verify_counted = false;
            record.hotness.clear_queued();
        }
    }

    /// P7 7.2: count this activation's speculative guard misses. Once the total
    /// crosses [`TIER_DOWN_MISSES`], ask for a non-speculative recompile.
    pub(crate) fn note_guard_misses(&mut self, id: u64, misses: u32) {
        if misses == 0 {
            return;
        }
        let Some(record) = self.records.get_mut(&id) else {
            return;
        };
        record.guard_misses = record.guard_misses.saturating_add(u64::from(misses));
        if record.guard_misses >= TIER_DOWN_MISSES {
            record.tier_down = true;
        }
    }

    /// Whether the compiler may specialize this function (P7 7.2: false once
    /// tier-down asked for a non-speculative compile).
    pub(crate) fn speculate(&self, id: u64) -> bool {
        self.records
            .get(&id)
            .is_none_or(|record| record.speculate && !record.tier_down)
    }

    /// Whether an installed function should be re-queued for a non-speculative
    /// recompile (P7 7.2).
    pub(crate) fn needs_tier_down_requeue(&self, id: u64) -> bool {
        self.records.get(&id).is_some_and(|record| {
            record.tier_down && record.speculate && matches!(record.lifecycle, Lifecycle::Installed)
        })
    }

    pub(crate) fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// The bytecode a queued/compiling function is pinned by, if any.
    #[cfg(feature = "jit_background")]
    pub(crate) fn pinned(&self, id: u64) -> Option<Bytecode> {
        self.records.get(&id).and_then(|record| record.pinned)
    }

    /// Undo a `begin_next` whose job could not be handed to a worker (channel
    /// full or workers gone): put the function back at the head of the queue.
    #[cfg(feature = "jit_background")]
    pub(crate) fn rollback(&mut self, id: u64) {
        let compiling = self
            .records
            .get(&id)
            .is_some_and(|record| matches!(record.lifecycle, Lifecycle::Compiling));
        if !compiling {
            return;
        }
        if let Some(record) = self.records.get_mut(&id) {
            record.lifecycle = Lifecycle::Queued;
        }
        self.queue.push_front(id);
    }

    /// Trace the functions held for an in-flight compile. Called from the GC
    /// root scan; see the module docs.
    pub(crate) fn trace_pinned(&self, gcx: &mut GcTraceCtx) {
        for record in self.records.values() {
            if let Some(bytecode) = &record.pinned {
                bytecode.trace(gcx);
            }
        }
    }
}

fn count_verified(m: &mut JitMetrics, record: &mut FunctionRecord) {
    if !record.verify_counted {
        record.verify_counted = true;
        m.functions_verified = m.functions_verified.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinator(max_attempts: u32) -> Coordinator {
        Coordinator::new(1, 1, 8, max_attempts, 30_000)
    }

    /// Drive a function to hot and queue it (no bytecode pin in tests).
    fn make_hot(c: &mut Coordinator, m: &mut JitMetrics, id: u64) {
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];
        assert!(c.record_call(m, id, &opcodes, 0), "should become hot");
        c.enqueue(m, id, None);
    }

    /// Simulate `poll` taking the next request off the queue.
    fn dispatch(c: &mut Coordinator) {
        let _ = c.begin_next();
    }

    /// Epic F5 wiring: the interpreter's measurement reaches the thresholds. A
    /// helper-dominated function (every opcode a `Helper`) is expensive to
    /// compile, so its static shape doubles the base; proven work (4096
    /// observations) halves it instead. An eager base (`1`, what the diagnostic
    /// harnesses ask for) is never adapted.
    #[test]
    fn measured_work_adapts_the_production_base_only() {
        let helper_dense = [crate::bytecode::opcodes::OP_add; 4];
        let mut m = JitMetrics::default();

        // Production base (32/56) + a little measured work: helper density
        // doubles it, so 64 calls are needed instead of 32.
        let mut c = Coordinator::new(32, 56, 8, 4, 30_000);
        for _ in 0..63 {
            assert!(
                !c.record_call(&mut m, 1, &helper_dense, 8),
                "64 calls are needed"
            );
        }
        assert!(
            c.record_call(&mut m, 1, &helper_dense, 8),
            "the 64th call is hot"
        );

        // Proven work (4096 observations) halves the base instead: the 16th.
        let mut c = Coordinator::new(32, 56, 8, 4, 30_000);
        for _ in 0..15 {
            assert!(!c.record_call(&mut m, 4, &helper_dense, 4096));
        }
        assert!(c.record_call(&mut m, 4, &helper_dense, 4096), "16th call");

        // Same function, no measurement (a baseline tier): the *static* helper
        // density still doubles the base, so 64 calls are needed.
        let mut c = Coordinator::new(32, 56, 8, 4, 30_000);
        for _ in 0..63 {
            assert!(!c.record_call(&mut m, 2, &helper_dense, 0));
        }
        assert!(
            c.record_call(&mut m, 2, &helper_dense, 0),
            "the static shape applies without a measurement"
        );

        // Eager base: adaptation is skipped, so the first call queues it.
        let mut c = Coordinator::new(1, 1, 8, 4, 30_000);
        assert!(c.record_call(&mut m, 3, &helper_dense, 4096));
    }

    #[test]
    fn entry_crosses_threshold_once() {
        let mut c = Coordinator::new(2, 2, 8, 4, 30_000);
        let mut m = JitMetrics::default();
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];
        assert!(!c.record_call(&mut m, 7, &opcodes, 0));
        assert!(c.record_call(&mut m, 7, &opcodes, 0));
        assert!(!c.record_call(&mut m, 7, &opcodes, 0), "already queued");
        assert_eq!(m.distinct_functions, 1);
    }

    #[test]
    fn full_queue_drops_without_queueing() {
        let mut c = Coordinator::new(1, 1, 1, 4, 30_000);
        let mut m = JitMetrics::default();
        make_hot(&mut c, &mut m, 1);
        // Second hot function: the queue is full, so it is dropped.
        make_hot(&mut c, &mut m, 2);
        assert_eq!(m.queue_drops, 1);
        assert_eq!(m.functions_queued, 1);
        assert_eq!(c.queue_len(), 1);
    }

    #[test]
    fn failure_retries_then_settles() {
        let mut c = coordinator(2);
        let mut m = JitMetrics::default();
        make_hot(&mut c, &mut m, 9);
        dispatch(&mut c);

        c.complete(&mut m, 9, CompileOutcome::Failed);
        assert_eq!(m.functions_retried, 1);
        assert_eq!(m.functions_settled, 0);
        assert_eq!(c.queue_len(), 1, "requeued");

        dispatch(&mut c);
        c.complete(&mut m, 9, CompileOutcome::Failed);
        assert_eq!(m.functions_settled, 1);
        assert_eq!(c.queue_len(), 0, "out of attempts");
        assert_eq!(m.functions_verified, 1, "verified counted once");
    }

    #[test]
    fn rejected_settles_without_retry() {
        let mut c = coordinator(4);
        let mut m = JitMetrics::default();
        make_hot(&mut c, &mut m, 3);
        dispatch(&mut c);
        c.complete(&mut m, 3, CompileOutcome::Rejected);
        assert_eq!(m.functions_rejected, 1);
        assert_eq!(m.functions_retried, 0);
        assert_eq!(c.queue_len(), 0);
    }

    #[test]
    fn installed_counts_verified_and_compiled() {
        let mut c = coordinator(4);
        let mut m = JitMetrics::default();
        make_hot(&mut c, &mut m, 4);
        dispatch(&mut c);
        c.complete(&mut m, 4, CompileOutcome::Installed);
        assert_eq!(m.functions_verified, 1);
        assert_eq!(m.functions_compiled, 1);
        // The B/C invariant: every queued function is verified or rejected.
        assert_eq!(
            m.functions_verified + m.functions_rejected,
            m.functions_queued
        );
    }
}
