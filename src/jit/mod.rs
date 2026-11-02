// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Tiered bytecode JIT (feature `jit`).
//!
//! The interpreter stays the source of truth: it records hotness at function
//! entries and loop backedges, and any op it cannot handle natively falls back
//! to it. This module owns the host-facing handle, the per-runtime state hung
//! off `RuntimeImpl`, and the interpreter hook entry points.
//!
//! A hot function is queued by the `runtime::Coordinator`; verification and
//! compilation run on worker threads, and the runtime thread installs the
//! finished code into the function's `jit` slot. `poll` only dispatches an
//! attempt to a worker, drains finished ones and installs them — the runtime
//! thread stays the single writer of every shared slot and the code cache.

mod abi;
mod analysis;
mod bytecode;
mod code_cache;
mod compiler;
mod config;
mod direct_call;
#[cfg(feature = "jit_dump")]
mod dump;
mod error;
mod fast_hash;
mod feedback;
mod inline_call;
mod layout;
mod metrics;
mod platform;
mod policy;
mod rate;
mod runtime;
mod snapshot;
#[cfg(test)]
mod test_harness;

mod handle;
mod hooks;
mod native;

pub use handle::Jit;
pub(crate) use hooks::*;
pub(crate) use native::invoke_native;

pub use {
    config::{JitConfig, JitConfigBuilder, JitTierPolicy},
    error::JitError,
    metrics::JitMetrics,
    policy::{JitPolicy, opcode_names, opcode_policies},
};

/// Every opcode the baseline lowers as a pure stack shuffle (see
/// `compiler::shuffle_opcodes`); the opcode gate checks its oracle set against
/// this.
pub use compiler::shuffle_opcodes;

pub(crate) use abi::{FunctionSlot, GlobalVarCache, JitExitKind};
pub(crate) use direct_call::DirectCall;
pub(crate) use feedback::FeedbackSlot;

use {
    abi::{HELPERS, JitExecFrame, LIT_ATOM_MAX},
    bytecode::VerifyLimits,
    code_cache::CodeCache,
    runtime::{
        BackgroundCompiler, CompileCompletion, CompileOutcome, Coordinator, JobResult, Tier,
        Tiering,
    },
    snapshot::CompileSnapshot,
};

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

use cranelift_codegen::isa::TargetIsa;
use gc_lite::GcTraceCtx;

use crate::{
    JSValue,
    function::Bytecode,
    runtime::{Runtime, RuntimeImpl},
};

/// How the two gates map onto the two tiers.
///
/// The forced policies ask for a fixed tier, so there is only one gate. The
/// `Automatic` policy runs the two gates — unless the host asked for an eager
/// compile (thresholds 1/1): "give me the code at the first event" is not a
/// request for a tiering experiment, so the top tier is compiled right away.
const fn tiering_for(cfg: &JitConfig) -> Tiering {
    match cfg.tier_policy {
        JitTierPolicy::Tier1 => Tiering::Single(Tier::Tier1),
        JitTierPolicy::Tier2 => Tiering::Single(Tier::Tier2),
        _ => {
            if cfg.call_threshold <= 1 && cfg.loop_threshold <= 1 {
                Tiering::Single(Tier::Tier2)
            } else {
                Tiering::TwoTier
            }
        }
    }
}

/// State hung off `RuntimeImpl` while a JIT is attached. Single-threaded: the
/// runtime and its interpreter run on one thread, so `Cell`/`RefCell` suffice.
/// Worker threads hold only `Send` snapshots and channels; every field here
/// stays on the runtime thread (single writer).
pub(crate) struct JitState {
    cfg: JitConfig,
    isa: Arc<dyn TargetIsa>,
    metrics: Cell<JitMetrics>,
    /// Hotness bookkeeping, the compile queue and per-function lifecycle.
    coordinator: RefCell<Coordinator>,
    /// Published code, bounded by `cfg.max_code_bytes`.
    cache: RefCell<CodeCache>,
    /// Worker threads and the in-flight attempts they are running.
    /// Single-writer state like the rest of `JitState`.
    background: RefCell<BackgroundCompiler>,
    /// Set whenever work is queued for the compiler: the interpreter's
    /// trampoline then polls at its next safe iteration (deferred polling), so
    /// functions hot inside one `eval` can run natively.
    poll_requested: Cell<bool>,
    /// Number of native activations currently on the Rust stack. Automatic
    /// polling is only allowed at zero: retiring a live mapping (single-owner mapping) while
    /// generated code still runs would free the code under it.
    native_depth: Cell<u32>,
    limits: VerifyLimits,
}

impl JitState {
    fn new(cfg: JitConfig) -> Result<Self, JitError> {
        // The dump switch is read once, at attach: everything below only ever
        // runs under the `jit_dump` feature.
        #[cfg(feature = "jit_dump")]
        dump::read_env();
        let isa = build_host_isa()?;
        let limits = VerifyLimits {
            max_metadata_bytes: cfg.max_metadata_bytes,
            max_snapshot_bytes: cfg.max_code_bytes.max(cfg.max_metadata_bytes),
            ..VerifyLimits::default()
        };
        let coordinator = Coordinator::new(
            cfg.call_threshold,
            cfg.loop_threshold,
            cfg.tier2_call_threshold,
            cfg.tier2_loop_threshold,
            tiering_for(&cfg),
            cfg.tier2_min_invocations,
            cfg.max_queue_len,
            cfg.max_compile_attempts,
            cfg.compile_timeout_ms,
            cfg.native_saving_ns_per_bytecode,
        );
        let background = BackgroundCompiler::new(cfg.workers, Arc::clone(&isa))?;
        Ok(Self {
            cache: RefCell::new(CodeCache::new(cfg.max_code_bytes)),
            cfg,
            isa,
            metrics: Cell::new(JitMetrics::default()),
            coordinator: RefCell::new(coordinator),
            background: RefCell::new(background),
            poll_requested: Cell::new(false),
            native_depth: Cell::new(0),
            limits,
        })
    }

    fn disabled(&self) -> bool {
        self.cfg.tier_policy == JitTierPolicy::Interpreter
    }

    const fn metrics(&self) -> JitMetrics {
        self.metrics.get()
    }

    fn target_isa(&self) -> &dyn TargetIsa {
        self.isa.as_ref()
    }

    /// Whether this policy can still reach tier 2: only then is
    /// feedback worth its per-site recording cost.
    const fn records_feedback(&self) -> bool {
        matches!(
            self.cfg.tier_policy,
            JitTierPolicy::Tier2 | JitTierPolicy::Automatic
        )
    }

    /// Record the operand types seen at a binary site (type feedback).
    fn note_binary(&self, bc: &Bytecode, site: u32, lhs: JSValue, rhs: JSValue, result: JSValue) {
        if !self.records_feedback() {
            return;
        }
        if bc.feedback.record(
            site,
            feedback::type_class(lhs),
            feedback::type_class(rhs),
            feedback::type_class(result),
        ) {
            self.count_feedback_site();
        }
    }

    /// Record the type seen at a single-value site: a branch condition or a
    /// local update (type feedback). Branch conditions have no result, so the result
    /// class stays 0.
    fn note_value(&self, bc: &Bytecode, site: u32, value: JSValue) {
        if !self.records_feedback() {
            return;
        }
        let class = feedback::type_class(value);
        if bc.feedback.record(site, class, 0, 0) {
            self.count_feedback_site();
        }
    }

    /// Record a `inc_loc`/`dec_loc` site: the slot's value before the update and
    /// the value it holds after (the result class tells
    /// overflow from a clean int update).
    fn note_local_update(&self, bc: &Bytecode, site: u32, before: JSValue, after: JSValue) {
        if !self.records_feedback() {
            return;
        }
        if bc.feedback.record(
            site,
            feedback::type_class(before),
            0,
            feedback::type_class(after),
        ) {
            self.count_feedback_site();
        }
    }

    fn count_feedback_site(&self) {
        let mut m = self.metrics.get();
        m.feedback_sites = m.feedback_sites.saturating_add(1);
        self.metrics.set(m);
    }

    /// Record the callee seen at a call site (the direct-call path's call feedback).
    ///
    /// Only while the function is still interpreted: once its code is
    /// installed, the recorded sites are baked into that code, and recording
    /// more would only cost. The recording is what makes a call site
    /// *monomorphic* — one callee, always — which is the premise of the
    /// direct call the compiler emits for such a site.
    fn note_call(&self, bc: &Bytecode, site: u32, callee: JSValue) {
        if self.disabled() || bc.jit.has_code() {
            return;
        }
        // The callee's identity is its bytecode node: two closures over one
        // bytecode share it (and share its compiled code), so it is the right
        // key. A callee that is not a bytecode function records `None`, which
        // makes the site non-monomorphic as soon as anything else shows up.
        let callee = crate::function::BCWrapper::try_from(callee)
            .ok()
            .map(|bfun| bfun.payload().bytecode());
        bc.feedback.record_call(site, callee);
    }

    fn note_enter(&self, bc: &Bytecode) {
        if self.disabled() {
            return;
        }
        let id = function_id(bc);
        let mut m = self.metrics.get();
        m.function_entries = m.function_entries.saturating_add(1);
        let measured = bc.feedback.total_samples();
        let mut coordinator = self.coordinator.borrow_mut();
        let tier = coordinator.record_call(&mut m, id, bc.opcodes(), measured);
        // A call-count hot function must also ask for a poll: the trampoline
        // polls at its next frame boundary, so the callee can run natively for
        // the rest of the eval. A loop-only marker would miss call-count
        // hotness.
        self.enqueue_and_request_poll(&mut coordinator, &mut m, id, bc, tier);
        drop(coordinator);
        self.metrics.set(m);
    }

    /// Queue `id` for `tier` when it crossed a gate, then ask the trampoline
    /// to poll at its next safe point: any queued work needs a poll, not just
    /// the request just made. Without both halves the work sits until the host
    /// polls by hand.
    ///
    /// Single-writer: the runtime thread only.
    fn enqueue_and_request_poll(
        &self,
        coordinator: &mut Coordinator,
        m: &mut JitMetrics,
        id: u64,
        bc: &Bytecode,
        tier: Option<Tier>,
    ) {
        if let Some(tier) = tier {
            coordinator.enqueue(m, id, Some(*bc), tier);
        }
        if coordinator.queue_len() > 0 {
            self.poll_requested.set(true);
        }
    }

    /// The native tier reports its own work back into the hotness counters.
    ///
    /// The interpreter only counts entries and back edges of *interpreted*
    /// frames, so a compiled function's counters would freeze at the moment
    /// its code was installed — and the second gate
    /// would never be crossed. The native tier therefore reports the
    /// activations it ran and the back edges its loops took.
    ///
    /// Only a function that may still be promoted reports at all: an
    /// optimizing-tier function has nothing left to cross, and the lookup
    /// would cost more than the call it rides on.
    fn note_native_work(&self, bc: &Bytecode, activation: bool, backedges: u32) {
        if self.disabled() {
            return;
        }
        let id = function_id(bc);
        let mut coordinator = self.coordinator.borrow_mut();
        let mut tier = None;
        if activation {
            tier = coordinator.record_native_call(id, 1);
        }
        if let Some(promoted) = coordinator.record_native_loops(id, backedges) {
            tier = tier.or(Some(promoted));
        }
        // The metrics are only touched when a promotion is queued: copying
        // the counter block on every native call would cost more than the
        // call itself.
        if let Some(tier) = tier {
            let mut m = self.metrics.get();
            self.enqueue_and_request_poll(&mut coordinator, &mut m, id, bc, Some(tier));
            drop(coordinator);
            self.metrics.set(m);
            return;
        }
        drop(coordinator);
    }

    /// Keep the function's "may still be promoted" flag in step with its tier:
    /// only a baseline-tier function can be promoted, so only it needs the
    /// native tier's per-call bookkeeping.
    fn sync_promotable(&self, bytecode: &Bytecode, id: u64) {
        let promotable = self.coordinator.borrow().tier(id) == Tier::Tier1;
        bytecode.jit.set_promotable(promotable);
    }

    /// Tier-down: fold one activation's speculative guard misses into the
    /// function's record and, once the threshold is crossed, queue the
    /// non-speculative recompile. A function with installed code never reaches
    /// `note_enter`, so this (not the interpreter hook) is what triggers
    /// tier-down.
    fn note_guard_misses(&self, bc: &Bytecode, misses: u32) {
        if misses == 0 {
            return;
        }
        let id = function_id(bc);
        let mut m = self.metrics.get();
        let mut coordinator = self.coordinator.borrow_mut();
        coordinator.note_guard_misses(id, misses);
        // The demotion compiles the baseline tier, whatever the function's
        // tier was.
        let tier = coordinator
            .needs_tier_down_requeue(id)
            .then_some(Tier::Tier1);
        self.enqueue_and_request_poll(&mut coordinator, &mut m, id, bc, tier);
        drop(coordinator);
        self.metrics.set(m);
    }

    /// Record a taken loop back edge and report whether the frame should
    /// re-enter its native code at that loop header (back-edge OSR).
    ///
    /// A top-level hot loop never crosses a frame boundary, so the trampoline
    /// never polls inside the `eval` and the frame would stay interpreted to the
    /// end. The back edge is itself a safe point (no native activation is in
    /// flight), so this hook polls there too; once an entry is installed, the
    /// interpreter re-enters it at the loop header.
    ///
    /// No pc-to-entry cache is needed: a taken back edge always jumps to a loop
    /// header, and every reachable loop header is an OSR entry point of the
    /// compiled dispatch (`collect_osr_pcs`), so "an entry is installed" is
    /// exactly the condition.
    fn note_backedge(&self, bc: &Bytecode) -> bool {
        if self.disabled() {
            return false;
        }
        let id = function_id(bc);
        let mut m = self.metrics.get();
        m.backedges = m.backedges.saturating_add(1);
        let measured = bc.feedback.total_samples();
        let mut coordinator = self.coordinator.borrow_mut();
        let tier = coordinator.record_loop(&mut m, id, bc.opcodes(), measured);
        // Deferred polling: there is work for the compiler; ask the trampoline
        // to poll at its next safe iteration (a no-op unless the host opted in).
        self.enqueue_and_request_poll(&mut coordinator, &mut m, id, bc, tier);
        drop(coordinator);
        self.metrics.set(m);
        // Back-edge OSR: poll here, not only at the next frame boundary, so a
        // finished compile can be installed and this loop re-entered natively.
        // Gated like the other automatic polls.
        self.poll_deferred();
        bc.jit.has_code()
    }

    /// The in-eval half of the automatic poll: the interpreter's trampoline runs
    /// at frame boundaries (between `execute_with_arena` calls, no JS running), so
    /// a hot callee that was just queued is dispatched to a worker there; once
    /// its code is installed, the callee runs natively for the rest of the
    /// *same* `eval`. This is what makes the automatic poll useful for a
    /// single-eval host: polling only when the eval returns would compile after
    /// all the work is done.
    ///
    /// `poll_requested` is raised by every enqueue (entry, back edge, tier-down);
    /// gated on `auto_poll` and on `native_depth == 0` (the trampoline can be
    /// entered from a native callee through the call helpers, and retiring a
    /// mapping then would free running code).
    fn poll_deferred(&self) {
        if !self.cfg.auto_poll || self.native_depth.get() != 0 {
            return;
        }
        // Also poll while an attempt is in flight, so a completion is drained
        // (and its code entered at the next back edge) without another enqueue.
        let pending = self.poll_requested.get() || self.background.borrow().in_flight_len() > 0;
        if pending {
            self.poll_requested.set(false);
            self.poll();
        }
    }

    /// Automatic poll at an engine safe point (eval return, event-loop tick).
    /// A cheap no-op unless the host left `auto_poll` on, no native activation
    /// is in flight, and something is actually queued or compiling.
    fn poll_if_queued(&self) {
        if !self.cfg.auto_poll || self.native_depth.get() != 0 {
            return;
        }
        let pending = self.coordinator.borrow().queue_len() > 0
            || self.background.borrow().in_flight_len() > 0;
        if pending {
            self.poll();
        }
    }

    /// Non-blocking poll: abandon timed-out attempts, drain finished ones and
    /// install them, then dispatch as many new attempts as the workers can
    /// take. On return an attempt may still be in flight, so
    /// `functions_compiling > 0` means "not settled yet".
    fn poll(&self) {
        let mut m = self.metrics.get();

        // Abandon whatever is past the budget before draining: a result that only
        // arrived because the host was slow is discarded, and the worker's late
        // completion is dropped by attempt id. Expiry runs before the drain, so a
        // completion that arrived within the budget but was not drained in time
        // is discarded too — acceptable while the default budget (30 s) is far
        // above a compile.
        let timeout = self.coordinator.borrow().timeout();
        let expired = self.background.borrow_mut().expire(timeout);
        for id in expired {
            self.coordinator
                .borrow_mut()
                .complete(&mut m, id, CompileOutcome::TimedOut);
        }

        // Install the attempts that finished within the budget.
        loop {
            // Bind the borrow before `install_completion` takes it again.
            let received = self.background.borrow_mut().try_recv();
            let Some(completion) = received else {
                break;
            };
            self.install_completion(completion, &mut m);
        }

        self.dispatch();
        // Bound the record table: one load-and-compare per poll, and a sweep
        // only when it has doubled since the last one.
        self.coordinator.borrow_mut().prune_if_grown();
        m.functions_compiling = self.background.borrow().in_flight_len() as u64;
        self.finish_poll(&mut m);
    }

    /// Hand queued functions to workers while there is capacity. A request that
    /// cannot be submitted (channel full, workers gone) is rolled back to the
    /// queue.
    fn dispatch(&self) {
        loop {
            if !self.background.borrow().has_capacity() {
                break;
            }
            let Some(request) = self.coordinator.borrow_mut().begin_next() else {
                break;
            };
            let snapshot = self.snapshot_for(&request);
            let submitted = self.background.borrow_mut().submit(
                request.function_id,
                request.tier,
                snapshot,
                self.limits,
            );
            if !submitted {
                self.coordinator.borrow_mut().rollback(request.function_id);
                break;
            }
        }
    }

    /// Install one finished attempt. A completion whose attempt was abandoned
    /// (timed out) or whose function is no longer pinned is dropped.
    fn install_completion(&self, completion: CompileCompletion, m: &mut JitMetrics) {
        let Some(job) = self.background.borrow_mut().resolve(completion.attempt_id) else {
            return; // stale: the attempt was abandoned by `expire`, or already resolved
        };
        let id = job.function_id;
        // The compile cost is accounted here: the worker measured it, the
        // runtime thread owns the counters.
        self.note_compile_cost(m, id, completion.nanos);
        // Bind the borrow before `install` takes it again (it writes the
        // coordinator on eviction).
        let pinned = self.coordinator.borrow().pinned(id);
        let outcome = match completion.result {
            JobResult::Compiled { bytes, specialized } => {
                m.specialized_ops = m.specialized_ops.saturating_add(specialized);
                // The pin is dropped by `invalidate` (single-owner mapping); without it there is
                // no slot to install into, so the result is discarded as a
                // failure (and the record is resolved rather than stranded).
                match pinned {
                    Some(bytecode) => self.install(id, &bytecode, &bytes, m),
                    None => CompileOutcome::Failed,
                }
            }
            JobResult::Rejected => CompileOutcome::Rejected,
            JobResult::Failed => CompileOutcome::Failed,
        };
        self.coordinator.borrow_mut().complete(m, id, outcome);
        if let Some(bytecode) = pinned {
            self.sync_promotable(&bytecode, id);
        }
    }

    /// Build the pointer-free snapshot of a request: the tier-2 profitability
    /// gate plus the tier-down decision. The snapshot is all a worker needs.
    fn snapshot_for(&self, request: &runtime::Request) -> CompileSnapshot {
        // Forced `Tier2` feeds the compiler every recorded site (gate = 1
        // sample); `Automatic` applies the profitability gate.
        let min_samples = match self.cfg.tier_policy {
            JitTierPolicy::Tier2 => 1,
            _ => self.cfg.specialization_min_samples,
        };
        let mut snapshot =
            CompileSnapshot::from_bytecode(request.function_id, &request.bytecode, min_samples);
        // Only the optimizing tier specializes; a pending tier-down forbids it
        // even if the request asked for the optimizing tier.
        snapshot.allow_speculation = request.tier == Tier::Tier2
            && !self
                .coordinator
                .borrow()
                .tier_down_pending(request.function_id);
        snapshot
    }

    /// Publish `bytes` into the function's slot and reset any mappings the code
    /// cache retired to fit (single-owner mapping). The single writer of the slot and the cache.
    fn install(
        &self,
        id: u64,
        bytecode: &Bytecode,
        bytes: &[u8],
        m: &mut JitMetrics,
    ) -> CompileOutcome {
        let mut cache = self.cache.borrow_mut();
        let published = runtime::publish(&mut cache, id, bytecode, bytes);
        drop(cache);
        match published {
            Some(published) => {
                m.note_published(&published);
                // The cache retired these live mappings to fit. Clear
                // their hotness so becoming hot again recompiles them.
                if !published.evicted.ids.is_empty() {
                    let mut coordinator = self.coordinator.borrow_mut();
                    for id in published.evicted.ids {
                        coordinator.invalidate(id);
                    }
                }
                CompileOutcome::Installed
            }
            None => CompileOutcome::Failed,
        }
    }

    /// Count the poll, refresh the cache-derived counters and notify the host.
    fn finish_poll(&self, m: &mut JitMetrics) {
        m.polls = m.polls.saturating_add(1);
        m.queue_len = self.coordinator.borrow().queue_len() as u64;
        // Reclaim the budget of mappings whose function is already gone,
        // so a long-running process can keep compiling after churn.
        self.cache.borrow_mut().reclaim();
        m.code_bytes = self.cache.borrow().used_bytes();
        self.metrics.set(*m);
        if let Some(observe) = &self.cfg.metrics_observer {
            observe(&self.metrics.get());
        }
    }

    /// Account for one compile attempt: the per-runtime metrics and the
    /// process-wide compile rate (the cost gate's input).
    fn note_compile_cost(&self, m: &mut JitMetrics, id: u64, nanos: u64) {
        m.note_compile(nanos);
        let bytes = self.coordinator.borrow().bytecode_bytes(id);
        rate::record(nanos, u64::from(bytes));
    }

    /// Trace the functions held for an in-flight compile so the GC cannot free
    /// their bytecode (and its `jit` slot) before the result is installed.
    pub(crate) fn trace(&self, gcx: &mut GcTraceCtx) {
        self.coordinator.borrow().trace_pinned(gcx);
    }
}

/// Stable identity of a bytecode function in the coordinator's table.
///
/// It is a [`FunctionSlot::serial`], **not** the GC node address: gc-lite
/// reuses freed addresses, so an address-keyed table would hand a brand-new
/// function the record of a collected one (its heat, tier, and — worst —
/// `Installed`/`Settled` lifecycle, which silently stops it from ever being
/// compiled). The table also could not tell when a record's function died, so
/// it would grow for the runtime's lifetime.
#[inline]
fn function_id(bc: &Bytecode) -> u64 {
    bc.jit.serial()
}

/// Build the Cranelift target for the host machine (cached once per runtime).
fn build_host_isa() -> Result<Arc<dyn TargetIsa>, JitError> {
    let builder = cranelift_native::builder().map_err(JitError::UnsupportedTarget)?;
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    builder
        .finish(flags)
        .map_err(|e| JitError::Cranelift(e.to_string()))
}

#[cfg(test)]
mod tests;
