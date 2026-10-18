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
//! `jit` slot. Verification and codegen run inline in [`Jit::poll`]; the
//! background worker (epic D3) is a documented deviation for now.

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
    runtime::{CompileOutcome, Coordinator},
    snapshot::CompileSnapshot,
};

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
    time::Instant,
};

use cranelift_codegen::isa::TargetIsa;
use gc_lite::GcTraceCtx;

use crate::{
    JSValue,
    function::Bytecode,
    runtime::{Runtime, RuntimeImpl},
};

/// State hung off `RuntimeImpl` while a JIT is attached. Single-threaded: the
/// runtime and its interpreter run on one thread, so `Cell`/`RefCell` suffice.
pub(crate) struct JitState {
    cfg: JitConfig,
    isa: Arc<dyn TargetIsa>,
    metrics: Cell<JitMetrics>,
    /// Hotness bookkeeping, the compile queue and per-function lifecycle.
    coordinator: RefCell<Coordinator>,
    /// Published code, bounded by `cfg.max_code_bytes`.
    cache: RefCell<CodeCache>,
    /// Set by a loop back edge when there is queued work; the interpreter's
    /// trampoline polls at its next safe iteration (P6 5.1) so functions hot
    /// inside one `eval` can run natively.
    poll_requested: Cell<bool>,
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
            cfg.max_queue_len,
            cfg.max_compile_attempts,
            cfg.compile_timeout_ms,
        );
        Ok(Self {
            cache: RefCell::new(CodeCache::new(cfg.max_code_bytes)),
            cfg,
            isa,
            metrics: Cell::new(JitMetrics::default()),
            coordinator: RefCell::new(coordinator),
            poll_requested: Cell::new(false),
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
        if coordinator.record_call(&mut m, id, bc.opcodes(), measured) {
            coordinator.enqueue(&mut m, id, Some(*bc));
        }
        drop(coordinator);
        self.metrics.set(m);
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
            coordinator.enqueue(&mut m, id, Some(*bc));
        }
        drop(coordinator);
        self.metrics.set(m);
    }

    fn note_backedge(&self, bc: &Bytecode, pc: usize) {
        let _ = pc; // the pc is only needed once OSR lands (epic E)
        if self.disabled() {
            return;
        }
        let id = function_id(bc);
        let mut m = self.metrics.get();
        m.backedges = m.backedges.saturating_add(1);
        let measured = bc.feedback_samples();
        let mut coordinator = self.coordinator.borrow_mut();
        if coordinator.record_loop(&mut m, id, bc.opcodes(), measured) {
            coordinator.enqueue(&mut m, id, Some(*bc));
        }
        // P6 5.1: there is work for the compiler; ask the trampoline to poll at
        // its next safe iteration (a no-op unless the host opted in).
        if coordinator.queue_len() > 0 {
            self.poll_requested.set(true);
        }
        drop(coordinator);
        self.metrics.set(m);
    }

    /// P6 5.1: poll from the interpreter's trampoline when a back edge asked
    /// for it and the host enabled `poll_during_eval`. Runs between
    /// `execute_with_arena` calls, where the runtime is at a safe point.
    fn poll_deferred(&self) {
        if self.cfg.poll_during_eval && self.poll_requested.get() {
            self.poll_requested.set(false);
            self.poll();
        }
    }

    /// Verify, compile and install every queued function. Runs on the polling
    /// thread, outside `with_new_scope`. A failed attempt is retried in the
    /// same poll until it installs, is rejected, or runs out of attempts, so
    /// the queue is always drained when this returns.
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
        }

        m.polls = m.polls.saturating_add(1);
        m.queue_len = self.coordinator.borrow().queue_len() as u64;
        // D-16: reclaim the budget of mappings whose function is already gone,
        // so a long-running process can keep compiling after churn.
        self.cache.borrow_mut().reclaim();
        m.code_bytes = self.cache.borrow().used_bytes();
        self.metrics.set(m);
        if let Some(observe) = &self.cfg.metrics_observer {
            observe(&self.metrics.get());
        }
    }

    /// One attempt: verify → compile → publish. Never panics; any failure just
    /// leaves the function interpreted. `m` accumulates tier-2 counters.
    fn run_pipeline(&self, request: &runtime::Request, m: &mut JitMetrics) -> CompileOutcome {
        // Epic F5 tier selection: forced `Tier2` is a fixed policy and feeds
        // the compiler every recorded site (gate = 1 sample). `Automatic` is
        // the production default, so it applies the profitability gate — a
        // site needs measured evidence before it earns a speculative path.
        let min_samples = match self.cfg.tier_policy {
            JitTierPolicy::Tier2 => 1,
            _ => self.cfg.specialization_min_samples,
        };
        let mut snapshot =
            CompileSnapshot::from_bytecode(request.function_id, &request.bytecode, min_samples);
        // P7 7.2 tier-down: after too many guard misses, recompile without
        // speculation so the function stops paying the guards.
        snapshot.speculate = self.coordinator.borrow().speculate(request.function_id);
        let Ok(verified) = verify(snapshot, self.limits) else {
            return CompileOutcome::Rejected;
        };
        // A function whose entry opcode the baseline cannot lower fails the
        // same way every attempt; settle it fail-closed instead of retrying.
        if !compiler::entry_is_translatable(&verified) {
            return CompileOutcome::Rejected;
        }
        let specialized = Cell::new(0);
        let Ok(bytes) = compiler::compile(&verified, self.isa.as_ref(), &specialized) else {
            return CompileOutcome::Failed;
        };
        m.specialized_ops = m.specialized_ops.saturating_add(specialized.get());
        let mut cache = self.cache.borrow_mut();
        let published =
            runtime::publish(&mut cache, request.function_id, &request.bytecode, &bytes);
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

    /// Trace the functions held for an in-flight compile so the GC cannot free
    /// their bytecode (and its `jit` slot) before the result is installed.
    pub(crate) fn trace(&self, gcx: &mut GcTraceCtx) {
        self.coordinator.borrow().trace_pinned(gcx);
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

    /// Drive the coordinator. Call only from outside `with_new_scope`: it may
    /// run host callbacks and touch the runtime. Verifies and compiles every
    /// queued function.
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

/// Called when a backward branch is taken (loop back edge).
#[inline]
pub(crate) fn note_backedge(rt: &RuntimeImpl, bc: &Bytecode, pc: usize) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_backedge(bc, pc);
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
/// # Safety
/// The caller (the interpreter) must be at the function entry (`pc == 0`), with
/// no pending exception, and the frame pointers must belong to `bc`'s current
/// activation.
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
    };
    // SAFETY: `entry` was produced by `compile` for this exact bytecode layout
    // and ABI, and the slot is only populated while the code cache keeps the
    // mapping alive; the frame outlives the call.
    let raw = unsafe { entry(&mut frame) };
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

    Some((kind, frame))
}

#[cfg(test)]
mod tests {
    use super::*;

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

        jit.poll();
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

        jit.poll();
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

        jit.poll();
        let m = jit.metrics();
        assert_eq!(m.queue_len, 0, "poll drained the queue");
        assert_eq!(m.functions_compiled, 0, "nothing fits the quota");
        assert!(m.compile_failures > 0, "the refused publish was recorded");
        assert!(m.functions_retried > 0, "the attempt was retried first");
        assert!(m.functions_settled > 0, "out of attempts -> settled");
    }

    /// P6 5.1: with `poll_during_eval`, a function that becomes hot *inside* one
    /// `eval` is compiled by the interpreter's trampoline, so later calls run
    /// natively without the host ever polling. This is what lets a Test262
    /// test's own local functions reach native code.
    #[test]
    fn poll_during_eval_compiles_inside_one_eval() {
        use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

        const SRC: &[u8] =
            b"function inner(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } \
            var t = 0; for (var k = 0; k < 50; k++) t += inner(200); t;";

        let mut rt = crate::Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .poll_during_eval(true)
                .build(),
        )
        .unwrap();
        ctx.eval(
            SRC,
            &EvalName::Name("jit-inline-poll.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
        let m = jit.metrics();
        assert!(m.functions_compiled > 0, "nothing compiled during the eval");
        assert!(m.native_entries > 0, "no native code ran during the eval");

        // Control: the same script with the default (host polls only) has
        // compiled nothing when the eval returns.
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
        ctx.eval(
            SRC,
            &EvalName::Name("jit-inline-poll-ctl.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
        assert_eq!(
            jit.metrics().functions_compiled,
            0,
            "the default must not poll during the eval"
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
        jit.poll();
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
        jit.poll();
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
