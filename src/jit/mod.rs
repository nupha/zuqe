// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Tiered bytecode JIT (feature `jit`).
//!
//! The interpreter stays the source of truth: it records hotness at function
//! entries and loop backedges, and any op it cannot handle natively falls back
//! to it. This module owns the host-facing handle, the per-runtime state hung
//! off `RuntimeImpl`, and the interpreter hook entry points.
//!
//! The frontend (`bytecode` + `snapshot`) is in place: a hot function is copied
//! into a pointer-free snapshot and either verified or rejected fail-closed.
//! Verified functions are compiled to machine code by the baseline lowering in
//! `compiler` and installed here synchronously (the background worker is epic D).

mod abi;
mod bytecode;
mod compiler;
mod config;
mod error;
mod metrics;
mod platform;
mod policy;
mod snapshot;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use cranelift_codegen::isa::TargetIsa;

use crate::{
    JSValue,
    function::Bytecode,
    runtime::{Runtime, RuntimeImpl},
};

use abi::{HELPERS, JitExecFrame, NativeEntry};
use bytecode::{VerifyLimits, verify};
use platform::CodeMem;
use snapshot::CompileSnapshot;

pub use config::{JitConfig, JitConfigBuilder, JitTierPolicy};
pub use error::JitError;
pub use metrics::JitMetrics;

pub(crate) use abi::JitExitKind;

/// Per-function hotness. Replaced by the real state machine in epic D.
#[derive(Default)]
struct HotEntry {
    calls: u32,
    backedges: u32,
    /// A compile request is already queued for this function.
    queued: bool,
}

/// State hung off `RuntimeImpl` while a JIT is attached. Single-threaded: the
/// runtime and its interpreter run on one thread, so `Cell`/`RefCell` suffice.
pub(crate) struct JitState {
    cfg: JitConfig,
    isa: Arc<dyn TargetIsa>,
    metrics: Cell<JitMetrics>,
    hot: RefCell<HashMap<u64, HotEntry>>,
    /// Snapshots captured when a function crossed a hotness threshold, waiting
    /// to be verified by the next `poll`. Owned and pointer-free, so the worker
    /// (epic D) can take them straight off this queue.
    pending: RefCell<VecDeque<CompileSnapshot>>,
    limits: VerifyLimits,
    /// Installed native entries, keyed by function id.
    entries: RefCell<HashMap<u64, NativeEntry>>,
    /// Owns the executable mappings so entries stay valid for the JIT's life.
    code: RefCell<Vec<CodeMem>>,
}

impl JitState {
    fn new(cfg: JitConfig) -> Result<Self, JitError> {
        let isa = build_host_isa()?;
        let limits = VerifyLimits {
            max_metadata_bytes: cfg.max_metadata_bytes,
            max_snapshot_bytes: cfg.max_code_bytes.max(cfg.max_metadata_bytes),
            ..VerifyLimits::default()
        };
        Ok(Self {
            cfg,
            isa,
            metrics: Cell::new(JitMetrics::default()),
            hot: RefCell::new(HashMap::new()),
            pending: RefCell::new(VecDeque::new()),
            limits,
            entries: RefCell::new(HashMap::new()),
            code: RefCell::new(Vec::new()),
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

    fn note_enter(&self, bc: &Bytecode) {
        if self.disabled() {
            return;
        }
        let id = bc.gc_node_ptr().as_ptr() as u64;
        let should_queue = {
            let mut hot = self.hot.borrow_mut();
            let first = !hot.contains_key(&id);
            let e = hot.entry(id).or_default();
            e.calls = e.calls.saturating_add(1);
            let q = !e.queued && e.calls >= self.cfg.call_threshold;
            if q {
                e.queued = true;
            }
            (first, q)
        };
        let m = self.metrics.get();
        self.metrics.set(JitMetrics {
            function_entries: m.function_entries + 1,
            distinct_functions: m.distinct_functions + should_queue.0 as u64,
            ..m
        });
        if should_queue.1 {
            self.enqueue(id, bc);
        }
    }

    fn note_backedge(&self, bc: &Bytecode, pc: usize) {
        let _ = pc; // the pc is only needed once OSR lands (epic E)
        if self.disabled() {
            return;
        }
        let id = bc.gc_node_ptr().as_ptr() as u64;
        let m = self.metrics.get();
        self.metrics.set(JitMetrics {
            backedges: m.backedges + 1,
            ..m
        });
        let should_queue = {
            let mut hot = self.hot.borrow_mut();
            let e = hot.entry(id).or_default();
            e.backedges = e.backedges.saturating_add(1);
            let q = !e.queued && e.backedges >= self.cfg.loop_threshold;
            if q {
                e.queued = true;
            }
            q
        };
        if should_queue {
            self.enqueue(id, bc);
        }
    }

    /// Capture a pointer-free snapshot for the verifier. The bytecode copy is
    /// the one place the hook allocates; it happens once per function.
    fn enqueue(&self, id: u64, bc: &Bytecode) {
        let mut pending = self.pending.borrow_mut();
        let m = self.metrics.get();
        if pending.len() >= self.cfg.max_queue_len {
            self.metrics.set(JitMetrics {
                queue_drops: m.queue_drops + 1,
                ..m
            });
            return;
        }
        pending.push_back(CompileSnapshot::from_bytecode(id, bc));
        self.metrics.set(JitMetrics {
            functions_queued: m.functions_queued + 1,
            queue_len: pending.len() as u64,
            ..m
        });
    }

    /// Verify every queued snapshot, then compile and install the ones that
    /// pass. The background worker (epic D) will take over compilation; for now
    /// this runs on the polling thread.
    fn poll(&self) {
        let prepared = {
            let mut pending = self.pending.borrow_mut();
            let mut accepted = 0u64;
            let mut rejected = 0u64;
            let mut compiled = 0u64;
            let mut code_bytes = 0u64;
            while let Some(snapshot) = pending.pop_front() {
                match verify(snapshot, self.limits) {
                    Ok(verified) => {
                        accepted += 1;
                        if let Some(entry) = self.compile_and_install(&verified) {
                            compiled += 1;
                            let _ = entry;
                        }
                    }
                    Err(_error) => rejected += 1,
                }
            }
            code_bytes += self.code_bytes();
            (accepted, rejected, compiled, code_bytes)
        };
        let m = self.metrics.get();
        self.metrics.set(JitMetrics {
            polls: m.polls + 1,
            functions_verified: m.functions_verified + prepared.0,
            functions_rejected: m.functions_rejected + prepared.1,
            functions_compiled: m.functions_compiled + prepared.2,
            code_bytes: prepared.3,
            queue_len: self.pending.borrow().len() as u64,
            ..m
        });
        if let Some(observe) = &self.cfg.metrics_observer {
            observe(&self.metrics.get());
        }
    }

    fn code_bytes(&self) -> u64 {
        self.code.borrow().iter().map(|mem| mem.len() as u64).sum()
    }

    /// Lower, publish and install one verified function. Returns the entry on
    /// success; a compilation failure simply leaves the function interpreted.
    fn compile_and_install(&self, verified: &bytecode::VerifiedFunction) -> Option<NativeEntry> {
        let bytes = match compiler::compile(verified, self.isa.as_ref()) {
            Ok(bytes) => bytes,
            Err(_) => return None,
        };
        let mem = CodeMem::alloc(bytes.len()).ok()?;
        mem.write(&bytes).ok()?;
        mem.make_executable().ok()?;
        // SAFETY: `mem` is executable and lives in `self.code` for as long as
        // the JIT does; `entry` is a plain function pointer.
        let entry: NativeEntry = unsafe { std::mem::transmute(mem.entry()) };
        self.code.borrow_mut().push(mem);
        self.entries
            .borrow_mut()
            .insert(verified.snapshot().function_id, entry);
        Some(entry)
    }
}

/// Build the Cranelift target for the host machine (cached once per runtime).
fn build_host_isa() -> Result<Arc<dyn TargetIsa>, JitError> {
    let builder = cranelift_native::builder().map_err(JitError::UnsupportedTarget)?;
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    builder
        .finish(flags)
        .map_err(|e| JitError::Cranelift(e.to_string()))
}

/// Host handle to a runtime's JIT. Created with [`Jit::attach`]; holds a strong
/// reference to the runtime so it stays valid as long as the handle does.
pub struct Jit {
    rt: Runtime,
}

impl Jit {
    /// Attach a JIT to `rt` with `cfg`. Fails if a JIT is already attached, or
    /// if the host has no Cranelift backend.
    pub fn attach(rt: &Runtime, cfg: JitConfig) -> Result<Jit, JitError> {
        let state = JitState::new(cfg)?;
        rt.jit_install(Box::new(state))?;
        Ok(Jit { rt: rt.clone() })
    }

    /// Drive the coordinator. Call only from outside `with_new_scope`: it may
    /// run host callbacks and touch the runtime.
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

/// Called on entry to a bytecode function. The function id is derived from the
/// node address, stable for the runtime's lifetime because the GC never moves
/// nodes.
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

/// Tier-2 feedback hook. Wired at arithmetic/branch/call sites in epic F.
#[allow(dead_code)]
#[inline]
pub(crate) const fn note_feedback(_rt: &RuntimeImpl, _function_id: u64, _site: usize) {}

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
) -> Option<(JitExitKind, JitExecFrame)> {
    let state = rt.jit.as_deref()?;
    let id = bc.gc_node_ptr().as_ptr() as u64;
    let entry = *state.entries.borrow().get(&id)?;
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
        out_value: JSValue::UNDEFINED,
        out_pc: 0,
        out_sp: 0,
    };
    // SAFETY: `entry` was produced by `compile` for this exact bytecode layout
    // and ABI; the frame outlives the call.
    let raw = unsafe { entry(&mut frame) };
    let m = state.metrics.get();
    state.metrics.set(JitMetrics {
        native_entries: m.native_entries + 1,
        ..m
    });
    Some((JitExitKind::from_raw(raw), frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_reports_metrics_and_target() {
        let rt = crate::Runtime::create().unwrap();
        let jit = Jit::attach(&rt, JitConfig::default()).unwrap();
        assert_eq!(jit.metrics(), JitMetrics::default());
        assert!(jit.target_isa().is_some());
    }

    #[test]
    fn second_attach_is_rejected() {
        let rt = crate::Runtime::create().unwrap();
        let _jit = Jit::attach(&rt, JitConfig::default()).unwrap();
        assert!(matches!(
            Jit::attach(&rt, JitConfig::default()),
            Err(JitError::AlreadyAttached)
        ));
    }

    #[test]
    fn poll_counts() {
        let rt = crate::Runtime::create().unwrap();
        let jit = Jit::attach(&rt, JitConfig::default()).unwrap();
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

        let jit = Jit::attach(&rt, JitConfig::default()).unwrap();
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
        let jit = Jit::attach(
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
}
