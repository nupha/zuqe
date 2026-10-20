// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Tiered bytecode JIT (feature `jit`).
//!
//! The interpreter stays the source of truth: it records hotness at function
//! entries and loop backedges, and any op it cannot handle natively falls back
//! to it. This module owns the host-facing handle, the per-runtime state hung
//! off `RuntimeImpl`, and the interpreter hook entry points.
//!
//! A hot function is queued by the `runtime::Coordinator`, verified, compiled
//! by the baseline lowering in `compiler`, and published into the function's
//! `jit` slot. Two compile models, selected by the `jit_background` feature
//! (in `default`):
//!
//! - **background** (default): `poll` only dispatches an attempt to a worker,
//!   drains finished ones and installs them — the runtime thread stays the
//!   single writer of every shared slot and the code cache;
//! - **inline** (`--no-default-features`, `jit` on): verification and codegen
//!   run in [`Jit::poll`] on the calling thread, which drains the whole queue
//!   before returning.

mod abi;
mod bytecode;
mod code_cache;
mod compiler;
mod config;
mod error;
mod feedback;
mod ir;
mod metrics;
mod platform;
mod policy;
mod runtime;
mod snapshot;

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

pub(crate) use abi::{FunctionSlot, JitExitKind};
pub(crate) use feedback::FeedbackSlot;

use {
    abi::{HELPERS, JitExecFrame},
    bytecode::{VerifyLimits, verify},
    code_cache::CodeCache,
    runtime::{CompileOutcome, Coordinator, Tier, Tiering},
    snapshot::CompileSnapshot,
};

#[cfg(feature = "jit_background")]
use runtime::{BackgroundCompiler, CompileCompletion, JobResult};

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

/// The compile-time budget classification is inline-only; the background poll
/// measures `dispatched_at` inside the worker bookkeeping instead.
#[cfg(not(feature = "jit_background"))]
use std::time::Instant;

use cranelift_codegen::isa::TargetIsa;
use gc_lite::GcTraceCtx;

use crate::{
    JSValue,
    function::Bytecode,
    runtime::{Runtime, RuntimeImpl},
};

/// How the two gates map onto the two tiers (plan §13.6.5 item 1).
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
/// Under `jit_background` the worker threads hold only `Send` snapshots and
/// channels; every field here stays on the runtime thread (single writer).
pub(crate) struct JitState {
    cfg: JitConfig,
    isa: Arc<dyn TargetIsa>,
    metrics: Cell<JitMetrics>,
    /// Hotness bookkeeping, the compile queue and per-function lifecycle.
    coordinator: RefCell<Coordinator>,
    /// Published code, bounded by `cfg.max_code_bytes`.
    cache: RefCell<CodeCache>,
    /// Worker threads and the in-flight attempts they are running
    /// (`jit_background` only). Single-writer state like the rest of `JitState`.
    #[cfg(feature = "jit_background")]
    background: RefCell<BackgroundCompiler>,
    /// Set by a loop back edge when there is queued work; the interpreter's
    /// trampoline polls at its next safe iteration (P6 5.1) so functions hot
    /// inside one `eval` can run natively.
    poll_requested: Cell<bool>,
    /// Number of native activations currently on the Rust stack. Automatic
    /// polling is only allowed at zero: retiring a live mapping (D-16) while
    /// generated code still runs would free the code under it.
    native_depth: Cell<u32>,
    limits: VerifyLimits,
}

impl JitState {
    fn new(cfg: JitConfig) -> Result<Self, JitError> {
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
        );
        #[cfg(feature = "jit_background")]
        let background = BackgroundCompiler::new(cfg.workers, Arc::clone(&isa))?;
        Ok(Self {
            cache: RefCell::new(CodeCache::new(cfg.max_code_bytes)),
            cfg,
            isa,
            metrics: Cell::new(JitMetrics::default()),
            coordinator: RefCell::new(coordinator),
            #[cfg(feature = "jit_background")]
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

    /// Whether this policy can still reach tier 2 (plan D-5): only then is
    /// feedback worth its per-site recording cost.
    const fn records_feedback(&self) -> bool {
        matches!(
            self.cfg.tier_policy,
            JitTierPolicy::Tier2 | JitTierPolicy::Automatic
        )
    }

    /// Record the operand types seen at a binary site (epic F1).
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
    /// local update (epic F1). Branch conditions have no result, so the result
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
    /// the value it holds after (epic P7 / task 7.1 — the result class tells
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

    fn note_enter(&self, bc: &Bytecode) {
        if self.disabled() {
            return;
        }
        let id = function_id(bc);
        let mut m = self.metrics.get();
        m.function_entries = m.function_entries.saturating_add(1);
        let measured = bc.feedback_samples();
        let mut coordinator = self.coordinator.borrow_mut();
        if let Some(tier) = coordinator.record_call(&mut m, id, bc.opcodes(), measured) {
            coordinator.enqueue(&mut m, id, Some(*bc), tier);
        }
        // Enqueueing here (a call-count hot function) must also ask for a poll:
        // the trampoline polls at its next frame boundary, so the callee can run
        // natively for the rest of the eval. A loop-only marker would miss
        // call-count hotness.
        if coordinator.queue_len() > 0 {
            self.poll_requested.set(true);
        }
        drop(coordinator);
        self.metrics.set(m);
    }

    /// The native tier reports its own work back into the hotness counters.
    ///
    /// The interpreter only counts entries and back edges of *interpreted*
    /// frames, so a compiled function's counters would freeze at the moment
    /// its code was installed — and the second gate (plan §13.6.5 item 1)
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
            coordinator.enqueue(&mut m, id, Some(*bc), tier);
            drop(coordinator);
            self.poll_requested.set(true);
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
        bytecode.jit_slot().set_promotable(promotable);
    }

    /// P7 7.2: fold one activation's speculative guard misses into the
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
        if coordinator.needs_tier_down_requeue(id) {
            // The demotion compiles the baseline tier, whatever the function's
            // tier was.
            coordinator.enqueue(&mut m, id, Some(*bc), Tier::Tier1);
            self.poll_requested.set(true);
        }
        drop(coordinator);
        self.metrics.set(m);
    }

    /// Record a taken loop back edge and report whether the frame should
    /// re-enter its native code at that loop header (epic N3, inline back-edge
    /// OSR).
    ///
    /// A top-level hot loop never crosses a frame boundary, so the trampoline
    /// never polls inside the `eval` and the frame would stay interpreted to the
    /// end. The back edge is itself a safe point (no native activation is in
    /// flight), so this hook polls there too; once an entry is installed, the
    /// interpreter re-enters it at the loop header.
    ///
    /// No pc-to-entry cache is needed: a *taken* back edge always jumps to a
    /// loop header, and every reachable loop header is an OSR entry point of the
    /// compiled dispatch (`collect_osr_pcs`), so "an entry is installed" is
    /// exactly the condition. `pc` stays in the signature for the background
    /// compiler (§12.9), which needs to know the point before an entry exists.
    fn note_backedge(&self, bc: &Bytecode, pc: usize) -> bool {
        let _ = pc;
        if self.disabled() {
            return false;
        }
        let id = function_id(bc);
        let mut m = self.metrics.get();
        m.backedges = m.backedges.saturating_add(1);
        let measured = bc.feedback_samples();
        let mut coordinator = self.coordinator.borrow_mut();
        if let Some(tier) = coordinator.record_loop(&mut m, id, bc.opcodes(), measured) {
            coordinator.enqueue(&mut m, id, Some(*bc), tier);
        }
        // P6 5.1: there is work for the compiler; ask the trampoline to poll at
        // its next safe iteration (a no-op unless the host opted in).
        if coordinator.queue_len() > 0 {
            self.poll_requested.set(true);
        }
        drop(coordinator);
        self.metrics.set(m);
        // N3: compile here, not at the next frame boundary, or the rest of this
        // loop stays interpreted. Gated like the other automatic polls.
        self.poll_deferred();
        bc.jit_slot().entry().is_some()
    }

    /// The in-eval half of the automatic poll: the interpreter's trampoline runs
    /// at frame boundaries (between `execute_with_arena` calls, no JS running), so
    /// a hot callee that was just queued can be compiled and then entered
    /// natively for the rest of the *same* `eval`. This is what makes the
    /// automatic poll useful for a single-eval host: polling only when the eval
    /// returns would compile after all the work is done.
    ///
    /// `poll_requested` is raised by every enqueue (entry, back edge, tier-down);
    /// gated on `auto_poll` and on `native_depth == 0` (the trampoline can be
    /// entered from a native callee through the call helpers, and retiring a
    /// mapping then would free running code).
    fn poll_deferred(&self) {
        if !self.cfg.auto_poll || self.native_depth.get() != 0 {
            return;
        }
        // Inline: `poll_requested` is raised by every enqueue. Background: also
        // poll while an attempt is in flight, so a completion is drained (and
        // its code entered at the next back edge) without another enqueue.
        #[cfg(not(feature = "jit_background"))]
        let pending = self.poll_requested.get();
        #[cfg(feature = "jit_background")]
        let pending = self.poll_requested.get() || self.background.borrow().in_flight_len() > 0;
        if pending {
            self.poll_requested.set(false);
            self.poll();
        }
    }

    /// Automatic poll at an engine safe point (eval return, event-loop tick).
    /// A cheap no-op unless the host left `auto_poll` on, no native activation
    /// is in flight, and something is actually queued (inline) or compiling
    /// (background).
    fn poll_if_queued(&self) {
        if !self.cfg.auto_poll || self.native_depth.get() != 0 {
            return;
        }
        #[cfg(not(feature = "jit_background"))]
        let pending = self.coordinator.borrow().queue_len() > 0;
        #[cfg(feature = "jit_background")]
        let pending = self.coordinator.borrow().queue_len() > 0
            || self.background.borrow().in_flight_len() > 0;
        if pending {
            self.poll();
        }
    }

    /// Verify, compile and install every queued function. Runs on the calling
    /// thread (inline model). The automatic poll only reaches this with no
    /// native activation in flight, and a host calling [`Jit::poll`] directly
    /// must not do so while generated code is running. A failed attempt is
    /// retried in the same poll until it installs, is rejected, or runs out of
    /// attempts, so the queue is always drained when this returns.
    #[cfg(not(feature = "jit_background"))]
    fn poll(&self) {
        let mut m = self.metrics.get();
        let timeout = self.coordinator.borrow().timeout();

        loop {
            let Some(request) = self.coordinator.borrow_mut().begin_next() else {
                break;
            };
            let started = Instant::now();
            let outcome = match self.run_pipeline(&request, &mut m) {
                CompileOutcome::Failed if started.elapsed() > timeout => CompileOutcome::TimedOut,
                other => other,
            };
            self.coordinator
                .borrow_mut()
                .complete(&mut m, request.function_id, outcome);
            self.sync_promotable(&request.bytecode, request.function_id);
        }

        self.finish_poll(&mut m);
    }

    /// Non-blocking poll (background model): abandon timed-out attempts, drain
    /// finished ones and install them, then dispatch as many new attempts as
    /// the workers can take. On return an attempt may still be in flight, so
    /// `functions_compiling > 0` means "not settled yet".
    #[cfg(feature = "jit_background")]
    fn poll(&self) {
        let mut m = self.metrics.get();

        // Abandon whatever is past the budget before draining, so a result that
        // only arrived because the host was slow is discarded (plan §12.5.4).
        // The worker keeps running; its late completion is dropped by attempt
        // id.
        //
        // shortcut: expiry runs before the drain, so a result that arrived
        // within the budget but was not drained before the deadline is also
        // discarded. Timestamping a completion on arrival would avoid that; not
        // needed while the default budget (30 s) is far above a compile.
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
        m.functions_compiling = self.background.borrow().in_flight_len() as u64;
        self.finish_poll(&mut m);
    }

    /// Hand queued functions to workers while there is capacity. A request that
    /// cannot be submitted (channel full, workers gone) is rolled back to the
    /// queue.
    #[cfg(feature = "jit_background")]
    fn dispatch(&self) {
        loop {
            if !self.background.borrow().has_capacity() {
                break;
            }
            let Some(request) = self.coordinator.borrow_mut().begin_next() else {
                break;
            };
            let snapshot = self.snapshot_for(&request);
            let submitted =
                self.background
                    .borrow_mut()
                    .submit(request.function_id, snapshot, self.limits);
            if !submitted {
                self.coordinator.borrow_mut().rollback(request.function_id);
                break;
            }
        }
    }

    /// Install one finished background attempt. A completion whose attempt was
    /// abandoned (timed out) or whose function is no longer pinned is dropped.
    #[cfg(feature = "jit_background")]
    fn install_completion(&self, completion: CompileCompletion, m: &mut JitMetrics) {
        let Some(job) = self.background.borrow_mut().resolve(completion.attempt_id) else {
            return; // stale: the attempt was abandoned by `expire`, or already resolved
        };
        let id = job.function_id;
        // Bind the borrow before `install` takes it again (it writes the
        // coordinator on eviction).
        let pinned = self.coordinator.borrow().pinned(id);
        let outcome = match completion.result {
            JobResult::Compiled { bytes, specialized } => {
                m.specialized_ops = m.specialized_ops.saturating_add(specialized);
                // The pin is dropped by `invalidate` (D-16); without it there is
                // no slot to install into, so the result is discarded as a
                // failure (and the record is resolved rather than stranded).
                match pinned {
                    Some(bytecode) => self.install(id, &bytecode, &bytes),
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
    /// gate (epic F5) plus the P7 7.2 tier-down decision. Shared by both models;
    /// the snapshot is all a worker needs.
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
        snapshot.speculate = request.tier == Tier::Tier2
            && !self
                .coordinator
                .borrow()
                .tier_down_pending(request.function_id);
        snapshot
    }

    /// Publish `bytes` into the function's slot and reset any mappings the code
    /// cache retired to fit (D-16). The single writer of the slot and the cache.
    fn install(&self, id: u64, bytecode: &Bytecode, bytes: &[u8]) -> CompileOutcome {
        let mut cache = self.cache.borrow_mut();
        let published = runtime::publish(&mut cache, id, bytecode, bytes);
        drop(cache);
        match published {
            Some(evicted) => {
                // D-16: the cache retired these live mappings to fit. Clear
                // their hotness so becoming hot again recompiles them.
                if !evicted.is_empty() {
                    let mut coordinator = self.coordinator.borrow_mut();
                    for id in evicted {
                        coordinator.invalidate(id);
                    }
                }
                CompileOutcome::Installed
            }
            None => CompileOutcome::Failed,
        }
    }

    /// Count the poll, refresh the cache-derived counters and notify the host.
    /// The tail both models share.
    fn finish_poll(&self, m: &mut JitMetrics) {
        m.polls = m.polls.saturating_add(1);
        m.queue_len = self.coordinator.borrow().queue_len() as u64;
        // D-16: reclaim the budget of mappings whose function is already gone,
        // so a long-running process can keep compiling after churn.
        self.cache.borrow_mut().reclaim();
        m.code_bytes = self.cache.borrow().used_bytes();
        self.metrics.set(*m);
        if let Some(observe) = &self.cfg.metrics_observer {
            observe(&self.metrics.get());
        }
    }

    /// One attempt: verify → compile → publish, on the calling thread. Never
    /// panics; any failure just leaves the function interpreted. `m`
    /// accumulates tier-2 counters.
    #[cfg(not(feature = "jit_background"))]
    fn run_pipeline(&self, request: &runtime::Request, m: &mut JitMetrics) -> CompileOutcome {
        match lower(self.snapshot_for(request), self.limits, self.isa.as_ref()) {
            Lowered::Compiled { bytes, specialized } => {
                m.specialized_ops = m.specialized_ops.saturating_add(specialized);
                self.install(request.function_id, &request.bytecode, &bytes)
            }
            Lowered::Rejected => CompileOutcome::Rejected,
            Lowered::Failed => CompileOutcome::Failed,
        }
    }

    /// Trace the functions held for an in-flight compile so the GC cannot free
    /// their bytecode (and its `jit` slot) before the result is installed.
    pub(crate) fn trace(&self, gcx: &mut GcTraceCtx) {
        self.coordinator.borrow().trace_pinned(gcx);
    }
}

/// How one lowering ended. `Rejected` means the frontend refused the snapshot
/// (fail-closed); `Compiled` carries owned code bytes for the runtime thread to
/// publish. Shared by the inline pipeline and the background worker, so the
/// worker handles only `Send` data.
pub(crate) enum Lowered {
    Compiled { bytes: Box<[u8]>, specialized: u64 },
    Rejected,
    Failed,
}

/// Verify and lower one snapshot. `isa` may be borrowed from a worker thread
/// (`TargetIsa: Send + Sync`); the snapshot and the result are `Send`, so this
/// is the whole cross-thread contract.
fn lower(snapshot: CompileSnapshot, limits: VerifyLimits, isa: &dyn TargetIsa) -> Lowered {
    let Ok(verified) = verify(snapshot, limits) else {
        return Lowered::Rejected;
    };
    // A function whose entry opcode the baseline cannot lower fails the same
    // way every attempt; the caller settles it fail-closed instead of retrying.
    if !compiler::entry_is_translatable(&verified) {
        return Lowered::Rejected;
    }
    let specialized = Cell::new(0);
    match compiler::compile(&verified, isa, &specialized) {
        Ok(bytes) => Lowered::Compiled {
            bytes,
            specialized: specialized.get(),
        },
        Err(_) => Lowered::Failed,
    }
}

/// Stable identity of a bytecode function: the GC node address. The GC never
/// moves nodes, so it is valid for the runtime's lifetime.
#[inline]
fn function_id(bc: &Bytecode) -> u64 {
    bc.gc_node_ptr().as_ptr() as u64
}

/// Build the Cranelift target for the host machine (cached once per runtime).
fn build_host_isa() -> Result<Arc<dyn TargetIsa>, JitError> {
    let builder = cranelift_native::builder().map_err(JitError::UnsupportedTarget)?;
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    builder
        .finish(flags)
        .map_err(|e| JitError::Cranelift(e.to_string()))
}

/// Host handle to a runtime's JIT. Created with [`Jit::attach_to`]; holds a strong
/// reference to the runtime so it stays valid as long as the handle does.
#[repr(transparent)]
pub struct Jit {
    rt: Runtime,
}

impl Jit {
    /// Attach a JIT to `rt` with `cfg`. Fails if a JIT is already attached, or
    /// if the host has no Cranelift backend.
    pub fn attach_to(rt: &Runtime, cfg: JitConfig) -> Result<Jit, JitError> {
        let state = JitState::new(cfg)?;
        rt.jit_install(Box::new(state))?;
        Ok(Jit { rt: rt.clone() })
    }

    /// Drive the coordinator.
    ///
    /// Background (`jit_background`, in `default`): dispatches attempts to
    /// workers and installs the finished ones; on return an attempt may still be
    /// in flight (`JitMetrics::functions_compiling > 0`), so call it again until
    /// the queue and the in-flight count are both zero. Inline
    /// (`--no-default-features`, `jit` on): verifies and compiles every queued
    /// function before returning.
    ///
    /// The runtime polls itself at engine safe points (eval return, event-loop
    /// tick), so a host normally does not need to call this. It stays public for
    /// hosts and tests that want to compile at an exact moment; pair it with
    /// [`JitConfigBuilder::auto_poll`]`(false)` when the timing must be fully
    /// explicit. Must not be called while generated code is running (a nested
    /// eval from a native callback); the automatic poll checks for that.
    pub fn poll(&self) {
        if let Some(state) = self.rt.jit() {
            state.poll();
        }
    }

    /// A snapshot of the JIT's counters.
    pub fn metrics(&self) -> JitMetrics {
        self.rt
            .jit()
            .map_or_else(JitMetrics::default, JitState::metrics)
    }

    /// Name of the Cranelift target ISA selected for this host.
    pub fn target_isa(&self) -> Option<String> {
        self.rt.jit().map(|s| s.target_isa().name().to_owned())
    }
}

// ── Interpreter hooks (no-ops unless a JIT is attached) ──

/// Called on entry to a bytecode function.
#[inline]
pub(crate) fn note_enter(rt: &RuntimeImpl, bc: &Bytecode) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_enter(bc);
    }
}

/// Called when a backward branch is taken (loop back edge). Returns whether the
/// interpreter should try the frame's native code at that loop header (N3).
#[inline]
pub(crate) fn note_backedge(rt: &RuntimeImpl, bc: &Bytecode, pc: usize) -> bool {
    match rt.jit.as_deref() {
        Some(state) => state.note_backedge(bc, pc),
        None => false,
    }
}

/// Poll from the interpreter's trampoline if a back edge asked for it (P6 5.1).
#[cfg(feature = "jit")]
#[inline]
pub(crate) fn poll_deferred(rt: &RuntimeImpl) {
    if let Some(state) = rt.jit.as_deref() {
        state.poll_deferred();
    }
}

/// Automatic poll at an engine safe point (eval return, event-loop tick), so a
/// host never has to drive compilation itself. No-op unless a JIT is attached,
/// `JitConfig::auto_poll` is on, no native activation is in flight and the queue
/// is non-empty.
#[cfg(feature = "jit")]
#[inline]
pub(crate) fn poll_if_queued(rt: &RuntimeImpl) {
    if let Some(state) = rt.jit.as_deref() {
        state.poll_if_queued();
    }
}

/// Tier-2 feedback hook: record the operand types and the result type of a
/// binary site (epic F1 / P7-7.1). A no-op unless a JIT is attached under a
/// tier-2-capable policy.
#[inline]
pub(crate) fn note_binary(
    rt: &RuntimeImpl,
    bc: &Bytecode,
    site: u32,
    lhs: JSValue,
    rhs: JSValue,
    result: JSValue,
) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_binary(bc, site, lhs, rhs, result);
    }
}

/// Tier-2 feedback hook: record the type of a single-value site — a branch
/// condition or a local update (epic F1).
#[inline]
pub(crate) fn note_value(rt: &RuntimeImpl, bc: &Bytecode, site: u32, value: JSValue) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_value(bc, site, value);
    }
}

/// Tier-2 feedback hook: record an `inc_loc`/`dec_loc` site — the value before
/// the update and the value after it (P7-7.1).
#[inline]
pub(crate) fn note_local_update(
    rt: &RuntimeImpl,
    bc: &Bytecode,
    site: u32,
    before: JSValue,
    after: JSValue,
) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_local_update(bc, site, before, after);
    }
}

/// Run the compiled entry for `bc`, if one is installed. Returns the exit kind
/// and the frame the code wrote its results into; `None` means "no native code,
/// interpret as usual".
///
/// `pc` is 0 at the function entry, or the OSR resume point otherwise: the pc
/// after a call the trampoline performed for us, or a loop header the
/// interpreter's back edge reported (N3, inline OSR). The compiled dispatch
/// selects the block for it and exits `Retry` for a pc it has no entry for.
///
/// # Safety
/// The caller (the interpreter) must be at `pc`, with no pending exception, the
/// frame's `cur_sp_offset` holding the operand-stack height that `pc` expects
/// (the OSR prologue reads it), and the frame pointers must belong to `bc`'s
/// current activation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn invoke_native(
    rt: &RuntimeImpl,
    bc: &Bytecode,
    scope: *mut u8,
    arg_base: *mut JSValue,
    var_base: *mut JSValue,
    sp_base: *mut JSValue,
    cur_sp_offset: *mut i32,
    this: JSValue,
    pc: u32,
    arg_real_count: u32,
    arena: *mut crate::vm_stack::VmStack,
    cur_frame_idx: u32,
    new_target: JSValue,
    home_obj: Option<crate::object::Object>,
) -> Option<(JitExitKind, JitExecFrame)> {
    let state = rt.jit.as_deref()?;
    let entry = bc.jit_slot().entry()?;
    let mut frame = JitExecFrame {
        scope,
        arg_base,
        var_base,
        sp_base,
        sp: sp_base,
        cpools: bc.consts().as_ptr(),
        cur_sp_offset,
        helpers: HELPERS.0.as_ptr().cast::<u8>(),
        this,
        gc_scope_kind: bc.gc_scope_kind() as u32,
        imm: 0,
        imm2: 0,
        pc,
        bytecode: &**bc,
        out_value: JSValue::UNDEFINED,
        out_pc: 0,
        out_sp: 0,
        arg_real_count,
        arena,
        cur_frame_idx,
        new_target,
        home_obj,
        guard_misses: 0,
        backedges: 0,
    };
    // SAFETY: `entry` was produced by `compile` for this exact bytecode layout
    // and ABI, and the slot is only populated while the code cache keeps the
    // mapping alive; the frame outlives the call.
    //
    // `native_depth` brackets the call: while it is non-zero no automatic poll
    // may retire a mapping (the code could still be executing). A nested
    // activation (a native callee through the call helpers) keeps it > 0 for the
    // whole outer run. A panic would leave it high, which only disables the
    // automatic poll — the safe direction.
    state
        .native_depth
        .set(state.native_depth.get().saturating_add(1));
    let raw = unsafe { entry(&mut frame) };
    state
        .native_depth
        .set(state.native_depth.get().saturating_sub(1));
    let kind = JitExitKind::from_raw(raw);
    let m = state.metrics.get();
    state.metrics.set(JitMetrics {
        native_entries: m.native_entries.saturating_add(1),
        native_interrupts: m
            .native_interrupts
            .saturating_add(u64::from(kind == JitExitKind::Interrupt)),
        // P7 7.2: this activation's speculative guard misses.
        specialized_guard_misses: m
            .specialized_guard_misses
            .saturating_add(u64::from(frame.guard_misses)),
        ..m
    });
    // P7 7.2: feed the same count to the function's record so a function whose
    // guards miss too often is recompiled without speculation.
    state.note_guard_misses(bc, frame.guard_misses);
    // The native tier's own work: the interpreter stopped counting this
    // function's entries and back edges when the code was installed, so the
    // second gate (plan §13.6.5 item 1) is fed from here. Only a function
    // that may still be promoted reports at all, so the check is a load next
    // to the entry pointer.
    if bc.jit_slot().promotable() {
        state.note_native_work(bc, pc == 0, frame.backedges);
    }

    Some((kind, frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bring the JIT to rest before asserting on compile results. Inline, one
    /// `poll` drains everything; background, `poll` is non-blocking, so this
    /// spins until no attempt is in flight and the queue is empty.
    fn settle(jit: &Jit) {
        #[cfg(not(feature = "jit_background"))]
        jit.poll();
        #[cfg(feature = "jit_background")]
        {
            use std::time::{Duration, Instant};
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                jit.poll();
                let m = jit.metrics();
                if m.queue_len == 0 && m.functions_compiling == 0 {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "the background compiler never settled: {m:?}"
                );
                // The worker compiles in the background; give it the CPU.
                std::thread::sleep(Duration::from_micros(50));
            }
        }
    }

    #[test]
    fn attach_reports_metrics_and_target() {
        let rt = crate::Runtime::create().unwrap();
        let jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
        assert_eq!(jit.metrics(), JitMetrics::default());
        assert!(jit.target_isa().is_some());
    }

    #[test]
    fn second_attach_is_rejected() {
        let rt = crate::Runtime::create().unwrap();
        let _jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
        assert!(matches!(
            Jit::attach_to(&rt, JitConfig::default()),
            Err(JitError::AlreadyAttached)
        ));
    }

    #[test]
    fn poll_counts() {
        let rt = crate::Runtime::create().unwrap();
        let jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
        jit.poll();
        jit.poll();
        assert_eq!(jit.metrics().polls, 2);
    }

    #[test]
    fn attached_jit_observes_runs_without_changing_results() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let src = b"var s = 0; for (var i = 0; i < 200; i++) s += i; s;";
        let name = EvalName::Name("jit-probe.js".into());

        let before = ctx
            .eval(src, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();

        let jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
        let after = ctx
            .eval(src, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();

        assert_eq!(before.as_int(), after.as_int());
        let m = jit.metrics();
        assert!(m.function_entries > 0, "entry hook fired");
        assert!(m.backedges > 0, "backedge hook fired");
    }

    #[test]
    fn poll_verifies_every_queued_function_exactly_once() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .build(),
        )
        .unwrap();

        // A script with several functions and a loop, so both entry and
        // backedge thresholds fire and at least one function is rejected
        // (`for...of`).
        let src = br#"
            function add(a, b) { return a + b; }
            function loop() { var s = 0; for (var i = 0; i < 100; i++) s = add(s, i); return s; }
            function iter() { var s = 0; for (var v of [1, 2, 3]) s += v; return s; }
            loop() + iter();
        "#;
        ctx.eval(
            src,
            &EvalName::Name("jit-verify-probe.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();

        settle(&jit);
        let m = jit.metrics();
        assert!(m.functions_queued > 0, "nothing queued");
        assert_eq!(
            m.functions_verified + m.functions_rejected,
            m.functions_queued,
            "every queued function must be accounted for"
        );
        assert!(m.functions_verified > 0, "nothing verified");
        assert_eq!(m.queue_len, 0, "poll drained the queue");
    }

    /// A function waiting to compile must be handed to the GC root scan, or it
    /// could be freed (and its `jit` slot with it) before `poll` installs the
    /// result. This asserts the trace itself, deterministically.
    #[test]
    fn queued_functions_are_traced_as_gc_roots() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .auto_poll(false) // the test drives the enqueue → GC → poll window
                .build(),
        )
        .unwrap();

        ctx.eval(
            b"function f(a){ return a + 1; } f(1);",
            &EvalName::Name("jit-pin.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
        assert!(jit.metrics().functions_queued > 0, "nothing queued");

        let mut gcx = ctx.gc_heap().create_trace_ctx(64);
        rt.jit().unwrap().trace(&mut gcx);
        assert!(
            !gcx.take_nodes().is_empty(),
            "a queued function was not traced as a GC root"
        );
    }

    /// End-to-end version of the same guarantee: collect *between* enqueue and
    /// poll. Without the pin the function's bytecode (and slot) could be gone
    /// by the time `poll` compiles; with it, poll still installs a valid entry.
    #[test]
    fn gc_between_enqueue_and_poll_still_installs() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .build(),
        )
        .unwrap();

        // The inner function becomes hot, then the IIFE returns and drops the
        // only reference to it.
        ctx.eval(
            b"(function(){ function f(a){ return a + 1; } return f(1); })();",
            &EvalName::Name("jit-pin-gc.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
        let queued = jit.metrics().functions_queued;
        assert!(queued > 0, "nothing queued");

        ctx.with_root_scope(|mut cx| cx.rt_mut().gc_collect());

        settle(&jit);
        let m = jit.metrics();
        assert_eq!(m.queue_len, 0, "poll drained the queue");
        assert!(
            m.functions_compiled > 0,
            "nothing compiled from the pinned snapshot (queued={queued})"
        );
    }

    /// M2b-8: when the platform refuses executable memory the attempt must
    /// fail, be retried once, settle the function in the interpreter — and the
    /// observable result must not change. This is the fault-injection half of
    /// the hardening gate (generated code cannot run under Miri).
    ///
    /// Inline-only: the fault switch (`platform::fault`) is thread-local, so a
    /// background worker would not see it.
    #[cfg(not(feature = "jit_background"))]
    #[test]
    fn allocation_failure_falls_back_to_the_interpreter() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        const DEFINE: &[u8] = b"function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(1);";
        const CALL: &[u8] = b"f(500)";
        let name = EvalName::Name("jit-fault.js".into());

        let mut base_rt = crate::Runtime::create().unwrap();
        let mut base_ctx = base_rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        base_ctx
            .eval(DEFINE, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        let expected = base_ctx
            .eval(CALL, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .max_compile_attempts(1)
                .auto_poll(false) // inject the fault before the first compile
                .build(),
        )
        .unwrap();
        ctx.eval(DEFINE, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();

        platform::fault::set(true);
        jit.poll();
        platform::fault::set(false);

        let m = jit.metrics();
        assert_eq!(
            m.functions_compiled, 0,
            "code was published with no executable memory"
        );
        assert!(
            m.compile_failures > 0,
            "the refused mapping was not recorded"
        );
        assert!(
            m.functions_settled > 0,
            "the function did not settle in the interpreter"
        );
        assert_eq!(m.code_bytes, 0, "code bytes leaked despite the fault");

        let got = ctx
            .eval(CALL, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        assert_eq!(
            got.as_int(),
            expected.as_int(),
            "the fallback changed the result"
        );
    }

    /// The code cache quota refuses a publish the same way; the quota test
    /// above and this one together cover both failure entries of `publish`.
    #[test]
    fn code_cache_quota_refuses_then_settles_after_retries() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        // One byte of code budget: every publish is refused.
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .max_code_bytes(1)
                .build(),
        )
        .unwrap();

        ctx.eval(
            b"function f(a){ return a + 1; } f(1);",
            &EvalName::Name("jit-quota.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();

        settle(&jit);
        let m = jit.metrics();
        assert_eq!(m.queue_len, 0, "poll drained the queue");
        assert_eq!(m.functions_compiled, 0, "nothing fits the quota");
        assert!(m.compile_failures > 0, "the refused publish was recorded");
        assert!(m.functions_retried > 0, "the attempt was retried first");
        assert!(m.functions_settled > 0, "out of attempts -> settled");
    }

    /// Automatic polling without any host call. The in-eval point is the
    /// interpreter's trampoline: a hot callee that was just queued is compiled at
    /// the next frame boundary and then runs natively for the rest of the same
    /// `eval`. `auto_poll(false)` is the manual control.
    ///
    /// Inline-only: it asserts the compile landed *inside* one `eval`. Under
    /// `jit_background` completion is asynchronous, so the background suite
    /// checks the eventual result instead.
    #[cfg(not(feature = "jit_background"))]
    #[test]
    fn automatic_poll_compiles_without_a_host_call() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        const SRC: &[u8] =
            b"function inner(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } \
            var t = 0; for (var k = 0; k < 50; k++) t += inner(200); t;";

        let run = |label: &str, cfg: JitConfig| -> JitMetrics {
            let mut rt = crate::Runtime::create().unwrap();
            let mut ctx = rt
                .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
                .unwrap();
            let jit = Jit::attach_to(&rt, cfg).unwrap();
            ctx.eval(
                SRC,
                &EvalName::Name(format!("jit-auto-poll-{label}.js")),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();
            // No explicit poll anywhere: everything below is the engine's doing.
            jit.metrics()
        };
        let eager = || JitConfig::builder().call_threshold(1).loop_threshold(1);

        // Default: compile from the trampoline as soon as `inner` is hot, so the
        // rest of the *same* eval runs natively.
        let m = run("default", eager().build());
        assert!(m.functions_compiled > 0, "nothing compiled during the eval");
        assert!(
            m.native_entries > 0,
            "the hot callee never ran native inside the eval"
        );

        // `auto_poll(false)`: only an explicit `Jit::poll()` compiles anything.
        let m = run("manual", eager().auto_poll(false).build());
        assert_eq!(
            m.functions_compiled, 0,
            "auto_poll=false must leave everything to the host poll"
        );
    }

    /// N3 inline back-edge OSR: a top-level loop is never re-entered and never
    /// crosses a frame boundary, so the trampoline cannot poll inside the
    /// `eval`; the back edge is what compiles the frame and hands it to its
    /// native loop header. The signal is the same as the other native tests —
    /// interpreter back edges — plus the first native entry. `auto_poll(false)`
    /// must leave the whole thing interpreted.
    ///
    /// Inline-only: it asserts the back edge compiled the frame *within* one
    /// `eval`; the background suite covers the eventual back-edge entry.
    #[cfg(not(feature = "jit_background"))]
    #[test]
    fn a_top_level_loop_runs_native_inside_one_eval() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        const SRC: &[u8] = b"var s = 1; for (var i = 1; i < 50000; i++) s = (s + i) | 0; s;";

        let run = |auto_poll: bool| -> JitMetrics {
            let mut rt = crate::Runtime::create().unwrap();
            let mut ctx = rt
                .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
                .unwrap();
            let jit = Jit::attach_to(
                &rt,
                JitConfig::builder()
                    .call_threshold(1)
                    .loop_threshold(1)
                    .auto_poll(auto_poll)
                    .build(),
            )
            .unwrap();
            ctx.eval(
                SRC,
                &EvalName::Name("jit-osr-lib.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();
            // No host poll: the eval is the whole measurement.
            jit.metrics()
        };

        let m = run(true);
        assert!(m.functions_compiled > 0, "nothing compiled ({m:?})");
        assert!(
            m.native_entries > 0,
            "the loop never entered native code ({m:?})"
        );
        assert!(m.backedges < 100, "the loop stayed interpreted ({m:?})");

        let m = run(false);
        assert_eq!(
            (m.functions_compiled, m.native_entries),
            (0, 0),
            "auto_poll=false compiled or entered native anyway ({m:?})"
        );
    }

    /// Contention between the runtime thread and the workers: they compile
    /// while the interpreter keeps running, while the GC traces and collects,
    /// and while a tight code-cache quota retires live mappings (D-16
    /// `invalidate`) — with several workers and host polls interleaved.
    ///
    /// The design makes a data race impossible (only `Send`, pointer-free
    /// snapshots cross; the runtime thread is the sole writer), so this test
    /// checks the *logical* races those rules exist to prevent: a lost pin (the
    /// GC frees a compiling function), a stale completion installed over a
    /// retired mapping, or a record stranded in `Compiling`. Any of those shows
    /// up as a wrong value, a crash, or a `settle` that never reaches quiet.
    ///
    /// `auto_poll(false)` is deliberate: `poll` dispatches a batch, then a long
    /// interpreted loop and a collection run **while** the workers are still
    /// compiling, so the overlap is real rather than incidental.
    #[cfg(feature = "jit_background")]
    #[test]
    fn background_compilation_races_the_interpreter_gc_and_eviction() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        // A long, allocation-free loop that keeps the interpreter busy while the
        // workers lower the round's functions.
        const CHURN: &[u8] =
            b"var s = 0; for (var i = 0; i < 100000; i++) s = (s + (i & 7)) | 0; s;";
        const CHURN_SUM: i32 = 12_500 * 28; // (100000 / 8) * (0+1+..+7)

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        // Four workers, eager thresholds, two code pages (installs retire each
        // other), and no automatic poll so the overlap is explicit.
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .workers(4)
                .max_code_bytes(2 * 4096)
                .auto_poll(false)
                .build(),
        )
        .unwrap();
        let name = EvalName::Name("jit-race.js".into());

        for round in 0..20i32 {
            // Six fresh functions per round: the cache must retire the previous
            // rounds' mappings while this round's compiles are in flight.
            let mut define = String::new();
            for k in 0..6 {
                define.push_str(&format!(
                    "function f{round}_{k}(n){{ var s = 0; for (var i = 0; i < n; i++) s = (s + i * {} + {k}) | 0; return s; }}",
                    round + 1
                ));
            }
            define.push_str(&format!("f{round}_0(300); f{round}_1(300);"));
            ctx.eval(
                define.as_bytes(),
                &name,
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();

            // Dispatch the batch, then run the long loop and collect **while**
            // the workers are still lowering.
            jit.poll();
            let churn = ctx
                .eval(CHURN, &name, EvalType::Global, EvalFlags::empty())
                .unwrap();
            assert_eq!(
                churn.as_int(),
                Some(CHURN_SUM),
                "round {round}: the interpreter changed under contention"
            );
            ctx.with_root_scope(|mut cx| cx.rt_mut().gc_collect());
            jit.poll();

            let got = ctx
                .eval(
                    format!("f{round}_0(300)").as_bytes(),
                    &name,
                    EvalType::Global,
                    EvalFlags::empty(),
                )
                .unwrap();
            let expected: i32 = (0..300).map(|i| i * (round + 1) + 0).sum::<i32>();
            assert_eq!(
                got.as_int(),
                Some(expected),
                "round {round} diverged while compiles were in flight"
            );
        }

        settle(&jit);
        let m = jit.metrics();
        assert_eq!(
            (m.queue_len, m.functions_compiling),
            (0, 0),
            "an attempt was stranded by the contention ({m:?})"
        );
        assert!(
            m.code_bytes <= 2 * 4096,
            "the quota was exceeded under churn ({m:?})"
        );
    }

    /// D-16 retirement end to end: with a budget of one code page, compiling a
    /// second function retires the first live mapping instead of refusing the
    /// compile. The retired function must fall back to the interpreter and
    /// still return the right answer (a stale entry would be a UAF, not a
    /// wrong result, so this test's real value is that it does not crash).
    #[test]
    fn the_code_cache_retires_a_live_mapping_under_a_tight_quota() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        // One page (`CodeMem` rounds every mapping up to a page): one function.
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .tier_policy(JitTierPolicy::Tier1)
                .max_code_bytes(4096)
                .build(),
        )
        .unwrap();
        let name = EvalName::Name("jit-retire.js".into());

        ctx.eval(
            b"function a(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } a(10);",
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
        settle(&jit);
        let after_a = jit.metrics();
        assert!(after_a.functions_compiled >= 1, "a was not compiled");
        assert!(after_a.code_bytes <= 4096, "a exceeded the quota");

        ctx.eval(
            b"function b(n){ var s = 1; for (var i = 0; i < n; i++) s = s + 1; return s + n; } b(10);",
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
        settle(&jit);
        let after_b = jit.metrics();
        assert!(
            after_b.functions_compiled > after_a.functions_compiled,
            "b was not compiled; the quota refused it instead of retiring a"
        );
        assert!(
            after_b.code_bytes <= 4096,
            "the retired mapping was not reclaimed ({after_b:?})"
        );

        // `a`'s code is gone; running it again must not call the freed mapping.
        let got = ctx
            .eval(b"a(100)", &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        assert_eq!(got.as_int(), Some(4950), "the retired function is wrong");
    }
}
