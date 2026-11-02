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
    /// Ran since the previous record-table sweep. Survivors keep their record;
    /// the rest are dropped (see [`Coordinator::prune_if_grown`]).
    touched: bool,
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

/// The record table's floor: below this many records a sweep is not worth its
/// pass over them (small runtimes never grow past it).
pub(crate) const MIN_RECORDS: usize = 1024;

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
    /// Record count that triggers the next sweep. It doubles after each one, so
    /// the sweep costs O(1) amortized per new function and the table stays
    /// within 2x of the live set.
    prune_at: usize,
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
            prune_at: MIN_RECORDS,
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
    const fn crossed_first_gate(record: &mut FunctionRecord, tier: Tier) -> Option<Tier> {
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
                touched: false,
            }
        });
        record.touched = true;
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

    /// Drop the records of functions that are gone, so the table tracks the
    /// live set instead of every function the runtime ever made hot.
    ///
    /// A record survives when it ran since the previous sweep, holds installed
    /// code (`tier != Cold`, itself bounded by the code cache), or is pinned by
    /// a queued/in-flight compile. The rest belong to functions the GC
    /// collected, or to ones that have been idle for a whole sweep — dropping
    /// those just means the next call starts from cold, which is what "idle"
    /// means. Called from `poll`, which is already the once-per-frame hook.
    pub(crate) fn prune_if_grown(&mut self) {
        if self.records.len() < self.prune_at {
            return;
        }
        self.records.retain(|_, record| {
            let keep = record.touched || record.pinned.is_some() || record.tier != Tier::Cold;
            record.touched = false;
            keep
        });
        self.prune_at = (self.records.len() * 2).max(MIN_RECORDS);
    }

    /// For tests: the current sweep threshold.
    #[cfg(test)]
    pub(crate) const fn prune_at(&self) -> usize {
        self.prune_at
    }

    /// For tests: the number of live records.
    #[cfg(test)]
    pub(crate) fn record_len(&self) -> usize {
        self.records.len()
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
mod tests;
