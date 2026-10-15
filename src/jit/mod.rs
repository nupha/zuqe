// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Tiered bytecode JIT (feature `jit`).
//!
//! The interpreter stays the source of truth: it records hotness at function
//! entries and loop backedges, and any op it cannot handle natively falls back
//! to it. This module owns the host-facing handle, the per-runtime state hung
//! off `RuntimeImpl`, and the interpreter hook entry points.
//!
//! Code generation, the frontend verifier and the background coordinator land in
//! later epics; for now the hooks only count and enqueue.

mod config;
mod error;
mod metrics;
mod policy;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use cranelift_codegen::isa::TargetIsa;

use crate::runtime::{Runtime, RuntimeImpl};

pub use config::{JitConfig, JitConfigBuilder, JitTierPolicy};
pub use error::JitError;
pub use metrics::JitMetrics;

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
    pending: RefCell<VecDeque<u64>>,
}

impl JitState {
    fn new(cfg: JitConfig) -> Result<Self, JitError> {
        let isa = build_host_isa()?;
        Ok(Self {
            cfg,
            isa,
            metrics: Cell::new(JitMetrics::default()),
            hot: RefCell::new(HashMap::new()),
            pending: RefCell::new(VecDeque::new()),
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

    fn note_enter(&self, id: u64) {
        if self.disabled() {
            return;
        }
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
            self.enqueue(id);
        }
    }

    fn note_backedge(&self, id: u64) {
        if self.disabled() {
            return;
        }
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
            self.enqueue(id);
        }
    }

    fn enqueue(&self, id: u64) {
        let mut pending = self.pending.borrow_mut();
        let m = self.metrics.get();
        if pending.len() >= self.cfg.max_queue_len {
            self.metrics.set(JitMetrics {
                queue_drops: m.queue_drops + 1,
                ..m
            });
            return;
        }
        pending.push_back(id);
        self.metrics.set(JitMetrics {
            functions_queued: m.functions_queued + 1,
            queue_len: pending.len() as u64,
            ..m
        });
    }

    /// Drain compile requests. The coordinator/compiler arrive in epics B–D, so
    /// today this only counts polls and notifies the observer.
    fn poll(&self) {
        let m = self.metrics.get();
        self.metrics.set(JitMetrics {
            polls: m.polls + 1,
            queue_len: self.pending.borrow().len() as u64,
            ..m
        });
        if let Some(observe) = &self.cfg.metrics_observer {
            observe(&self.metrics.get());
        }
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

/// Called on entry to a bytecode function. `function_id` identifies the
/// function's bytecode; it is stable for the lifetime of the runtime because
/// the GC never moves nodes.
#[inline]
pub(crate) fn note_enter(rt: &RuntimeImpl, function_id: u64) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_enter(function_id);
    }
}

/// Called when a backward branch is taken (loop back edge).
#[inline]
pub(crate) fn note_backedge(rt: &RuntimeImpl, function_id: u64, pc: usize) {
    let _ = pc; // the pc is only needed once OSR lands (epic E)
    if let Some(state) = rt.jit.as_deref() {
        state.note_backedge(function_id);
    }
}

/// Tier-2 feedback hook. Wired at arithmetic/branch/call sites in epic F.
#[allow(dead_code)]
#[inline]
pub(crate) const fn note_feedback(_rt: &RuntimeImpl, _function_id: u64, _site: usize) {}

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
}
