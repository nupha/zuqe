// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Compilation queue and per-function lifecycle.
//!
//! Hotness decides *when* a function is worth compiling; this module owns the
//! request queue and the states a function moves through. Verification and
//! codegen run on compiler workers, so the coordinator itself never touches
//! code.
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

use crate::{
    function::Bytecode,
    jit::{fast_hash::FastHasherBuilder, metrics::JitMetrics, rate},
};

use super::hotness::{AdaptiveInputs, HotThresholds, HotnessState, Tier};

/// Where a function is in its compile lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    /// Seen but not hot yet.
    Cold,
    /// Waiting in the queue.
    Queued,
    /// Handed to a compiler worker.
    Compiling,
    /// Native code is installed in the function's slot.
    Installed,
    /// Never compiled again: rejected by the frontend, or out of attempts.
    Settled,
}

/// How the two gates map onto the two tiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tiering {
    /// One tier only: every compile produces it. The forced policies ask for
    /// a fixed tier, and an eager host (thresholds 1/1) asks for the code at
    /// the first event — there is no room for a tiering experiment.
    Single(Tier),
    /// Two gates: the first compiles the baseline tier, the second one
    /// promotes the function to the optimizing tier.
    TwoTier,
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
    /// The tier this attempt must produce.
    pub(crate) tier: Tier,
}

struct FunctionRecord {
    hotness: HotnessState,
    thresholds: HotThresholds,
    lifecycle: Lifecycle,
    /// The tier of the installed code (`Cold` while there is none).
    tier: Tier,
    /// The tier the queued attempt must produce.
    pending_tier: Tier,
    /// A compile is already queued: the "queue exactly once" invariant. It is
    /// cleared when the attempt lands (so the second gate can queue the
    /// promotion) and when the function is invalidated.
    queued: bool,
    attempts: u32,
    /// Whether `functions_verified`/`functions_rejected` already counted this
    /// function. Verification is deterministic, so it is counted once even
    /// when a later codegen failure retries it.
    verify_counted: bool,
    /// Held while `Queued`/`Compiling` so the GC cannot free the function.
    pinned: Option<Bytecode>,
    /// Speculative guard misses observed in this function's native code. Past
    /// [`TIER_DOWN_MISSES`] the function is recompiled without speculation.
    guard_misses: u64,
    /// Tier-down requested: the next compile must not speculate.
    tier_down: bool,
    /// Invocations still required before the function may be promoted again
    /// (V8's `minimum_invocations_after_ic_update`). It keeps the promotion
    /// and the tier-down from chasing each other.
    cooldown: u32,
    /// Work the function did since it was queued (entries + back edges). Zero
    /// means it has not run since: it is idle, and the queue may drop it for
    /// a hotter newcomer (the queue admission policy).
    work_since_queue: u32,
    /// The size of the function's bytecode, in bytes. The cost gate needs it
    /// to weigh a compile against the work the function does (the compile-rate gate).
    bytecode_bytes: u32,
}

impl FunctionRecord {
    /// Count the work done since the function was queued. Only a queued
    /// function accumulates it: a function that is not waiting for a compile
    /// has no place in the queue to be dropped from.
    const fn note_work(&mut self) {
        if self.queued {
            self.work_since_queue = self.work_since_queue.saturating_add(1);
        }
    }
}

/// Tier-down: how many speculative guard misses one function may take before it is
/// recompiled without speculation. Shortcut: a fixed threshold instead of a
/// per-site profitability model; raise/lower it if the guard-miss metric shows
/// the wrong trade-off.
pub(crate) const TIER_DOWN_MISSES: u64 = 64;

/// The request queue plus the lifecycle of every function the JIT has seen.
pub(crate) struct Coordinator {
    records: HashMap<u64, FunctionRecord, FastHasherBuilder>,
    queue: VecDeque<u64>,
    base_calls: u32,
    base_loops: u32,
    base_tier2_calls: u32,
    base_tier2_loops: u32,
    tiering: Tiering,
    tier2_min_invocations: u32,
    max_queue_len: usize,
    max_attempts: u32,
    timeout: Duration,
    /// The host's estimate of what one bytecode of native execution saves;
    /// The benefit side of the cost gate. `0` turns the gate off.
    native_saving_ns_per_bytecode: u64,
}

impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        base_calls: u32,
        base_loops: u32,
        base_tier2_calls: u32,
        base_tier2_loops: u32,
        tiering: Tiering,
        tier2_min_invocations: u32,
        max_queue_len: usize,
        max_attempts: u32,
        compile_timeout_ms: u64,
        native_saving_ns_per_bytecode: u64,
    ) -> Self {
        Self {
            records: HashMap::default(),
            queue: VecDeque::new(),
            base_calls,
            base_loops,
            base_tier2_calls,
            base_tier2_loops,
            tiering,
            tier2_min_invocations,
            max_queue_len,
            max_attempts,
            timeout: Duration::from_millis(compile_timeout_ms),
            native_saving_ns_per_bytecode,
        }
    }

    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The cost gate: how many activations a function must be observed
    /// at before its compile can pay for itself. It combines the **measured**
    /// cost of a compile (`jit::rate`) with the host's estimate of what
    /// native execution saves. `0` means "no gate".
    fn payback_activations(&self) -> u32 {
        if self.native_saving_ns_per_bytecode == 0 {
            return 0;
        }
        rate::ns_per_bytecode()
            .map(|rate| rate::payback_activations(rate, self.native_saving_ns_per_bytecode))
            .unwrap_or(0)
    }

    /// The tier the first gate produces.
    const fn first_tier(&self) -> Tier {
        match self.tiering {
            Tiering::Single(tier) => tier,
            Tiering::TwoTier => Tier::Tier1,
        }
    }

    /// Count a function entry. Returns the tier to compile when this event
    /// makes the function hot.
    pub(crate) fn record_call(
        &mut self,
        m: &mut JitMetrics,
        id: u64,
        opcodes: &[u8],
        measured_work: u32,
    ) -> Option<Tier> {
        let tier = self.first_tier();
        // One hash for the whole lookup: the lookup that finds the record
        // already answers "first time we see this function", so a separate
        // `contains_key` would hash the key a second time on every entry.
        let (first, record) = self.record_mut(id, opcodes, measured_work);
        if first {
            m.distinct_functions = m.distinct_functions.saturating_add(1);
        }
        record.hotness.record_call();
        record.cooldown = record.cooldown.saturating_sub(1);
        // The work that crosses the gate happened *before* the function was
        // queued, so it is not counted as "work since queued".
        record.note_work();
        Self::crossed_first_gate(record, tier)
    }

    /// Count a taken backward branch; same contract as `record_call`.
    pub(crate) fn record_loop(
        &mut self,
        _m: &mut JitMetrics,
        id: u64,
        opcodes: &[u8],
        measured_work: u32,
    ) -> Option<Tier> {
        let tier = self.first_tier();
        let (_first, record) = self.record_mut(id, opcodes, measured_work);
        record.hotness.record_loop();
        record.note_work();
        Self::crossed_first_gate(record, tier)
    }

    /// Count the activations the native tier ran for a function. The
    /// interpreter stops counting once the code is installed, so without this
    /// the second gate could never be crossed.
    pub(crate) fn record_native_call(&mut self, id: u64, activations: u32) -> Option<Tier> {
        if activations == 0 {
            return None;
        }
        let two_tier = matches!(self.tiering, Tiering::TwoTier);
        let record = self.records.get_mut(&id)?;
        record.hotness.record_native_calls(activations);
        record.cooldown = record.cooldown.saturating_sub(activations);
        record.note_work();
        Self::crossed_second_gate(record, two_tier)
    }

    /// Count the back edges the native tier ran for a function.
    pub(crate) fn record_native_loops(&mut self, id: u64, backedges: u32) -> Option<Tier> {
        if backedges == 0 {
            return None;
        }
        let two_tier = matches!(self.tiering, Tiering::TwoTier);
        let record = self.records.get_mut(&id)?;
        record.hotness.record_native_loops(backedges);
        record.note_work();
        Self::crossed_second_gate(record, two_tier)
    }

    /// The first gate: a function that has no code yet is compiled as soon as
    /// it is hot. Returns the tier to compile.
    fn crossed_first_gate(record: &mut FunctionRecord, tier: Tier) -> Option<Tier> {
        if record.queued
            || !matches!(record.lifecycle, Lifecycle::Cold)
            || !record.hotness.crossed_first_gate(&record.thresholds)
        {
            return None;
        }
        record.queued = true;
        Some(tier)
    }

    /// The second gate: a function that already runs natively is promoted to
    /// the optimizing tier once it is hot enough. The tier-down's cooldown
    /// blocks it, so a demoted function is not promoted right back.
    fn crossed_second_gate(record: &mut FunctionRecord, two_tier: bool) -> Option<Tier> {
        if !two_tier
            || record.queued
            || record.cooldown > 0
            || record.tier != Tier::Tier1
            || !matches!(record.lifecycle, Lifecycle::Installed)
            || !record.hotness.crossed_second_gate(&record.thresholds)
        {
            return None;
        }
        record.queued = true;
        Some(Tier::Tier2)
    }

    /// The record for `id`, creating it when it is new. The flag says
    /// whether this call created it: the caller counts distinct functions,
    /// and the lookup that finds the record already knows the answer — a
    /// separate `contains_key` would hash the key a second time on every
    /// function entry.
    ///
    /// The creation stays inside the closure on purpose: it walks the
    /// bytecode (the adaptive thresholds), and inlined into the lookup it
    /// would run on every single entry.
    fn record_mut(
        &mut self,
        id: u64,
        opcodes: &[u8],
        measured_work: u32,
    ) -> (bool, &mut FunctionRecord) {
        let (base_calls, base_loops) = (self.base_calls, self.base_loops);
        let (base_tier2_calls, base_tier2_loops) = (self.base_tier2_calls, self.base_tier2_loops);
        let payback = self.payback_activations();
        let mut created = false;
        let record = self.records.entry(id).or_insert_with(|| {
            created = true;
            let inputs = AdaptiveInputs::from_opcodes(opcodes).with_measured_work(measured_work);
            let thresholds = inputs.thresholds(
                base_calls,
                base_loops,
                base_tier2_calls,
                base_tier2_loops,
                payback,
            );
            FunctionRecord {
                hotness: HotnessState::default(),
                thresholds,
                lifecycle: Lifecycle::Cold,
                tier: Tier::Cold,
                pending_tier: Tier::Tier1,
                queued: false,
                attempts: 0,
                verify_counted: false,
                pinned: None,
                guard_misses: 0,
                tier_down: false,
                cooldown: 0,
                work_since_queue: 0,
                bytecode_bytes: inputs.bytecode_bytes,
            }
        });
        (created, record)
    }

    /// The size of a function's bytecode, in bytes (0 when it is unknown).
    pub(crate) fn bytecode_bytes(&self, id: u64) -> u32 {
        self.records
            .get(&id)
            .map_or(0, |record| record.bytecode_bytes)
    }

    /// Move a now-hot function into the queue. When the queue is full a
    /// request is dropped either way, but **not** always the newcomer:
    ///
    /// - a queued function that has **stopped running** (no calls since it
    ///   was queued) is dropped instead, so the compile budget goes to the
    ///   function that is still hot — dropping a function that is no longer
    ///   called buys nothing, dropping a live one does (the queue admission policy);
    /// - when every queued function is still running, the newcomer is the
    ///   coldest one and is dropped itself.
    ///
    /// A dropped request is counted in `queue_drops`; the queue keeps its
    /// length either way. `bytecode` is `None` only in unit tests.
    pub(crate) fn enqueue(
        &mut self,
        m: &mut JitMetrics,
        id: u64,
        bytecode: Option<Bytecode>,
        tier: Tier,
    ) {
        if self.queue.len() >= self.max_queue_len {
            m.queue_drops = m.queue_drops.saturating_add(1);
            if !self.evict_idle() {
                return;
            }
        }
        let Some(record) = self.records.get_mut(&id) else {
            return;
        };
        record.pinned = bytecode;
        record.lifecycle = Lifecycle::Queued;
        record.pending_tier = tier;
        record.work_since_queue = 0;
        self.queue.push_back(id);
        m.functions_queued = m.functions_queued.saturating_add(1);
        m.queue_len = self.queue.len() as u64;
    }

    /// Drop a queued function that has stopped running, so a hotter newcomer
    /// can take its place. Returns `false` when every queued function is
    /// still being called — then the newcomer is the coldest one and is
    /// dropped instead.
    ///
    /// The evicted function keeps its hotness counters, so its next call
    /// queues it again. A function that is already installed keeps its code:
    /// only the pending promotion is dropped, and the second gate fires again
    /// on the next activation.
    fn evict_idle(&mut self) -> bool {
        let idle = self.queue.iter().copied().find(|id| {
            self.records
                .get(id)
                .is_some_and(|record| record.work_since_queue == 0)
        });
        let Some(id) = idle else {
            return false;
        };
        self.queue.retain(|queued| *queued != id);
        if let Some(record) = self.records.get_mut(&id) {
            record.queued = false;
            record.pinned = None;
            record.work_since_queue = 0;
            if record.tier == Tier::Cold {
                // No code yet: the function goes back to cold and its next
                // call queues it again.
                record.lifecycle = Lifecycle::Cold;
            } else {
                // The code is installed: only the pending attempt is dropped
                // (a first compile or a tier-2 promotion), and the gate that
                // asked for it fires again.
                record.lifecycle = Lifecycle::Installed;
            }
        }
        true
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
            // A pending tier-down demotes the attempt to the baseline tier,
            // whatever was requested. The effective tier stays with the record
            // until the attempt lands, so the worker model can report it
            // without carrying it through the worker.
            let tier = if record.tier_down {
                Tier::Tier1
            } else {
                record.pending_tier
            };
            record.pending_tier = tier;
            return Some(Request {
                function_id: id,
                bytecode,
                tier,
            });
        }
        None
    }

    /// Record the result of one attempt, retrying or settling as configured.
    /// The tier comes from the record: the attempt's own tier, unless a
    /// tier-down demoted it.
    pub(crate) fn complete(&mut self, m: &mut JitMetrics, id: u64, outcome: CompileOutcome) {
        let mut requeue = false;
        {
            let Some(record) = self.records.get_mut(&id) else {
                return;
            };
            match outcome {
                CompileOutcome::Installed => {
                    count_verified(m, record);
                    let previous = record.tier;
                    let tier = record.pending_tier;
                    record.lifecycle = Lifecycle::Installed;
                    record.pinned = None;
                    record.queued = false;
                    if record.tier_down {
                        // A tier-down landed: the installed code no longer
                        // speculates, and the promotion waits for a while
                        // (V8's `minimum_invocations_after_ic_update`).
                        record.tier = Tier::Tier1;
                        record.tier_down = false;
                        record.cooldown = self.tier2_min_invocations;
                        m.tier_downs = m.tier_downs.saturating_add(1);
                    } else {
                        record.tier = tier;
                        if tier == Tier::Tier2 && previous == Tier::Tier1 {
                            m.tier2_promotions = m.tier2_promotions.saturating_add(1);
                        }
                    }
                    if previous != Tier::Cold {
                        m.functions_recompiled = m.functions_recompiled.saturating_add(1);
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
    /// live mapping (single-owner mapping). The hotness counters go with it: the code is
    /// gone, so the function has to become hot again from scratch.
    pub(crate) fn invalidate(&mut self, id: u64) {
        if let Some(record) = self.records.get_mut(&id) {
            record.lifecycle = Lifecycle::Cold;
            record.pinned = None;
            record.attempts = 0;
            record.verify_counted = false;
            record.queued = false;
            record.tier = Tier::Cold;
            record.hotness = HotnessState::default();
            record.work_since_queue = 0;
        }
    }

    /// Tier-down: count this activation's speculative guard misses. Once the total
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

    /// Whether a tier-down is pending for this function: the next compile
    /// must not speculate, whatever tier it asks for.
    pub(crate) fn tier_down_pending(&self, id: u64) -> bool {
        self.records.get(&id).is_some_and(|record| record.tier_down)
    }

    /// Whether an installed function should be re-queued for a non-speculative
    /// recompile (tier-down).
    pub(crate) fn needs_tier_down_requeue(&self, id: u64) -> bool {
        self.records.get(&id).is_some_and(|record| {
            record.tier_down
                && record.tier == Tier::Tier2
                && matches!(record.lifecycle, Lifecycle::Installed)
        })
    }

    /// The tier of a function's installed code (`Cold` when it has none).
    pub(crate) fn tier(&self, id: u64) -> Tier {
        self.records
            .get(&id)
            .map_or(Tier::Cold, |record| record.tier)
    }

    pub(crate) fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// The bytecode a queued/compiling function is pinned by, if any.
    pub(crate) fn pinned(&self, id: u64) -> Option<Bytecode> {
        self.records.get(&id).and_then(|record| record.pinned)
    }

    /// Undo a `begin_next` whose job could not be handed to a worker (channel
    /// full or workers gone): put the function back at the head of the queue.
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

const fn count_verified(m: &mut JitMetrics, record: &mut FunctionRecord) {
    if !record.verify_counted {
        record.verify_counted = true;
        m.functions_verified = m.functions_verified.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use std::hash::Hasher;

    use super::*;

    use crate::jit::fast_hash::FastHasher;

    /// A coordinator with the two gates: the first at `calls`/`loops`, the
    /// second at `tier2_calls`/`tier2_loops`.
    fn two_tier(
        calls: u32,
        loops: u32,
        tier2_calls: u32,
        tier2_loops: u32,
        max_attempts: u32,
    ) -> Coordinator {
        Coordinator::new(
            calls,
            loops,
            tier2_calls,
            tier2_loops,
            Tiering::TwoTier,
            500,
            8,
            max_attempts,
            30_000,
            0,
        )
    }

    fn coordinator(max_attempts: u32) -> Coordinator {
        Coordinator::new(
            1,
            1,
            1024,
            2048,
            Tiering::Single(Tier::Tier2),
            500,
            8,
            max_attempts,
            30_000,
            0,
        )
    }

    /// Drive a function to hot and queue it (no bytecode pin in tests).
    fn make_hot(c: &mut Coordinator, m: &mut JitMetrics, id: u64) {
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];
        let tier = c
            .record_call(m, id, &opcodes, 0)
            .expect("should become hot");
        c.enqueue(m, id, None, tier);
    }

    /// Simulate `poll` taking the next request off the queue.
    fn dispatch(c: &mut Coordinator) {
        let _ = c.begin_next();
    }

    /// Profitability-gate wiring: the interpreter's measurement reaches the thresholds. A
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
        let mut c = two_tier(32, 56, 1024, 2048, 4);
        for _ in 0..63 {
            assert!(
                c.record_call(&mut m, 1, &helper_dense, 8).is_none(),
                "64 calls are needed"
            );
        }
        assert!(
            c.record_call(&mut m, 1, &helper_dense, 8).is_some(),
            "the 64th call is hot"
        );

        // Proven work (4096 observations) halves the base instead: the 16th.
        let mut c = two_tier(32, 56, 1024, 2048, 4);
        for _ in 0..15 {
            assert!(c.record_call(&mut m, 4, &helper_dense, 4096).is_none());
        }
        assert!(
            c.record_call(&mut m, 4, &helper_dense, 4096).is_some(),
            "16th call"
        );

        // Same function, no measurement (a baseline tier): the *static* helper
        // density still doubles the base, so 64 calls are needed.
        let mut c = two_tier(32, 56, 1024, 2048, 4);
        for _ in 0..63 {
            assert!(c.record_call(&mut m, 2, &helper_dense, 0).is_none());
        }
        assert!(
            c.record_call(&mut m, 2, &helper_dense, 0).is_some(),
            "the static shape applies without a measurement"
        );

        // Eager base: adaptation is skipped, so the first call queues it.
        let mut c = coordinator(4);
        assert!(c.record_call(&mut m, 3, &helper_dense, 4096).is_some());
    }

    #[test]
    fn entry_crosses_threshold_once() {
        let mut c = two_tier(2, 2, 1024, 2048, 4);
        let mut m = JitMetrics::default();
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];
        assert!(c.record_call(&mut m, 7, &opcodes, 0).is_none());
        assert!(c.record_call(&mut m, 7, &opcodes, 0).is_some());
        assert!(
            c.record_call(&mut m, 7, &opcodes, 0).is_none(),
            "already queued"
        );
        assert_eq!(m.distinct_functions, 1);
    }

    /// T8 end to end: the two interpreter hooks feed **one** weighted budget.
    /// 16 entries plus 28 back edges cross the 32/56 line although neither
    /// counter has reached its own corner (16 < 32, 28 < 56) — the wiring the
    /// `hotness` unit tests cannot see.
    #[test]
    fn mixed_calls_and_loops_share_one_budget() {
        let mut c = two_tier(32, 56, 1024, 2048, 4);
        let mut m = JitMetrics::default();
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];
        for _ in 0..15 {
            assert!(c.record_call(&mut m, 7, &opcodes, 0).is_none());
        }
        for _ in 0..28 {
            assert!(
                c.record_loop(&mut m, 7, &opcodes, 0).is_none(),
                "15×56 + 28×32 is still under the budget"
            );
        }
        assert!(
            c.record_call(&mut m, 7, &opcodes, 0).is_some(),
            "16×56 + 28×32 crosses the budget with neither corner reached"
        );
    }

    /// The map's hash has to spread the node addresses over the buckets.
    /// They are aligned pointers, so a hash that keeps their low bits would
    /// file them all under one bucket and turn every lookup into a walk over
    /// the same chain. The mixer itself is pinned in `jit::fast_hash`.
    #[test]
    fn the_pointer_hash_spreads_aligned_keys() {
        let hash_of = |ptr: u64| {
            let mut h = FastHasher::default();
            h.write_u64(ptr);
            h.finish()
        };
        // The addresses the GC hands out: aligned, and a page apart.
        let keys = [
            0x1000u64, 0x2000, 0x3000, 0x4000, 0x5000, 0x6000, 0x7000, 0x8000,
        ];
        // The low bits pick the bucket (128 of them is a small table).
        let buckets: std::collections::HashSet<u64> =
            keys.iter().map(|k| hash_of(*k) & 0x7f).collect();
        assert!(
            buckets.len() > keys.len() / 2,
            "the hash clusters the keys: {buckets:?}"
        );
    }

    /// A queue of one: the newcomer takes the place of the queued function,
    /// which has not run since it was queued. The queue keeps its length and
    /// the dropped request is counted.
    #[test]
    fn a_full_queue_drops_the_idle_function() {
        let mut c = Coordinator::new(
            1,
            1,
            1024,
            2048,
            Tiering::Single(Tier::Tier2),
            500,
            1,
            4,
            30_000,
            0,
        );
        let mut m = JitMetrics::default();
        make_hot(&mut c, &mut m, 1);
        // Second hot function: the queue is full, so the idle one is dropped.
        make_hot(&mut c, &mut m, 2);
        assert_eq!(m.queue_drops, 1);
        assert_eq!(m.functions_queued, 2);
        assert_eq!(c.queue_len(), 1);
    }

    /// The queue admission policy: a full queue does not drop the newcomer
    /// blindly. A queued function that has **stopped running** is dropped
    /// instead, so the compile budget goes to the function that is still hot.
    #[test]
    fn a_full_queue_drops_an_idle_function_for_the_newcomer() {
        let mut c = Coordinator::new(
            1,
            1,
            1024,
            2048,
            Tiering::Single(Tier::Tier2),
            500,
            2,
            4,
            30_000,
            0,
        );
        let mut m = JitMetrics::default();
        make_hot(&mut c, &mut m, 1);
        make_hot(&mut c, &mut m, 2);
        assert_eq!(c.queue_len(), 2, "the queue is full");

        // The third function is hot and takes the place of the first queued
        // one, which has not run since it was queued.
        make_hot(&mut c, &mut m, 3);
        assert_eq!(m.queue_drops, 1, "the idle function was dropped");
        assert_eq!(c.queue_len(), 2, "the queue keeps its length");
        assert_eq!(m.functions_queued, 3);

        // The evicted function is queued again by its next call: it keeps its
        // hotness counters.
        let tier = c
            .record_call(&mut m, 1, &[crate::bytecode::opcodes::OP_return_undef], 0)
            .expect("still hot");
        c.enqueue(&mut m, 1, None, tier);
        assert_eq!(m.functions_queued, 4);
    }

    /// When every queued function is still running, the newcomer is the
    /// coldest one and is dropped — the live functions are kept.
    #[test]
    fn a_full_queue_keeps_the_functions_that_are_still_running() {
        let mut c = Coordinator::new(
            1,
            1,
            1024,
            2048,
            Tiering::Single(Tier::Tier2),
            500,
            2,
            4,
            30_000,
            0,
        );
        let mut m = JitMetrics::default();
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];
        make_hot(&mut c, &mut m, 1);
        make_hot(&mut c, &mut m, 2);
        // Both queued functions run again: they are not idle any more.
        c.record_call(&mut m, 1, &opcodes, 0);
        c.record_call(&mut m, 2, &opcodes, 0);

        make_hot(&mut c, &mut m, 3);
        assert_eq!(m.queue_drops, 1, "the newcomer was dropped");
        assert_eq!(c.queue_len(), 2);
        assert_eq!(m.functions_queued, 2, "the newcomer did not get in");
    }

    /// A pending tier-2 promotion that is evicted keeps the installed code:
    /// only the promotion request is dropped, and the second gate fires again
    /// on the next activation.
    #[test]
    fn evicting_a_pending_promotion_keeps_the_installed_code() {
        let mut c = Coordinator::new(1, 1, 2, 2, Tiering::TwoTier, 500, 1, 4, 30_000, 0);
        let mut m = JitMetrics::default();
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];

        // The baseline tier is installed.
        let tier = c.record_call(&mut m, 1, &opcodes, 0).expect("hot");
        c.enqueue(&mut m, 1, None, tier);
        dispatch(&mut c);
        c.complete(&mut m, 1, CompileOutcome::Installed);
        assert_eq!(c.tier(1), Tier::Tier1);

        // The second gate queues the promotion.
        let tier = c.record_native_call(1, 2).expect("promoted");
        c.enqueue(&mut m, 1, None, tier);
        assert_eq!(c.queue_len(), 1, "the queue is full");

        // A newcomer takes the place of the idle promotion.
        make_hot(&mut c, &mut m, 2);
        assert_eq!(m.queue_drops, 1);
        assert_eq!(c.queue_len(), 1);
        assert_eq!(c.tier(1), Tier::Tier1, "the installed code is still there");

        // The second gate fires again on the next activation.
        assert!(
            c.record_native_call(1, 2).is_some(),
            "the promotion is queued again"
        );
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

    /// The first gate compiles the baseline tier, the second one
    /// promotes the function to the optimizing tier. The promotion is driven
    /// by the counts the *native* tier reports back, because the interpreter
    /// stops counting once the code is installed.
    #[test]
    fn the_second_gate_promotes_a_compiled_function() {
        let mut c = two_tier(2, 2, 4, 8, 4);
        let mut m = JitMetrics::default();
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];

        // Two calls cross the first gate: the baseline tier.
        assert!(c.record_call(&mut m, 1, &opcodes, 0).is_none());
        let tier = c.record_call(&mut m, 1, &opcodes, 0).expect("hot");
        assert_eq!(tier, Tier::Tier1);
        c.enqueue(&mut m, 1, None, tier);
        dispatch(&mut c);
        c.complete(&mut m, 1, CompileOutcome::Installed);
        assert_eq!(
            c.tier(1),
            Tier::Tier1,
            "the baseline tier does not speculate"
        );

        // The native tier reports one more activation: still below the
        // second gate (4 calls).
        assert!(c.record_native_call(1, 1).is_none());
        // ... and the back edges of a hot loop cross it.
        let tier = c.record_native_loops(1, 8).expect("promoted");
        assert_eq!(tier, Tier::Tier2);
        c.enqueue(&mut m, 1, None, tier);
        dispatch(&mut c);
        c.complete(&mut m, 1, CompileOutcome::Installed);
        assert_eq!(c.tier(1), Tier::Tier2, "the optimizing tier speculates");
        assert_eq!(m.tier2_promotions, 1);
        assert_eq!(m.functions_recompiled, 1);
        assert_eq!(m.functions_compiled, 2);
    }

    /// A demoted function (tier-down) must not be promoted right
    /// back — the cooldown keeps the two recompiles from chasing each other.
    #[test]
    fn a_demoted_function_waits_before_it_is_promoted_again() {
        let mut c = two_tier(1, 1, 2, 2, 4);
        let mut m = JitMetrics::default();
        let opcodes = [crate::bytecode::opcodes::OP_return_undef];

        // First gate: the baseline tier.
        let tier = c.record_call(&mut m, 1, &opcodes, 0).expect("hot");
        assert_eq!(tier, Tier::Tier1);
        c.enqueue(&mut m, 1, None, tier);
        dispatch(&mut c);
        c.complete(&mut m, 1, CompileOutcome::Installed);
        assert_eq!(c.tier(1), Tier::Tier1);

        // Second gate: the optimizing tier.
        let tier = c.record_native_call(1, 2).expect("promoted");
        assert_eq!(tier, Tier::Tier2);
        c.enqueue(&mut m, 1, None, tier);
        dispatch(&mut c);
        c.complete(&mut m, 1, CompileOutcome::Installed);
        assert_eq!(c.tier(1), Tier::Tier2);

        // The guards miss too often: the function is demoted.
        c.note_guard_misses(1, TIER_DOWN_MISSES as u32);
        assert!(c.needs_tier_down_requeue(1));
        c.enqueue(&mut m, 1, None, Tier::Tier1);
        dispatch(&mut c);
        c.complete(&mut m, 1, CompileOutcome::Installed);
        assert_eq!(c.tier(1), Tier::Tier1);
        assert_eq!(m.tier_downs, 1);

        // The second gate is crossed, but the cooldown blocks the promotion.
        assert!(c.record_native_call(1, 100).is_none());
        // ... until enough new invocations have passed.
        assert!(c.record_native_call(1, 500).is_some(), "cooldown over");
    }
}
