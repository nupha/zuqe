// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Shared harness for the M2b integration suites: one way to run a script
//! under a tier policy and describe its outcome exactly, so the differential,
//! opcode and semantic gates cannot drift apart.

#![allow(dead_code)]

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, JSValue, Jit, JitConfig, JitMetrics,
    JitTierPolicy, Runtime, ScopedContext,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Policy `Interpreter`: the JIT is attached but every hook is inert.
    Interpreter,
    /// Forced baseline tier.
    Tier1,
    /// Forced optimizing tier: feedback-driven specialization (epic F1/F2).
    Tier2,
    /// Production default: pick the tier from hotness.
    Automatic,
}

pub const MODES: [Mode; 4] = [
    Mode::Interpreter,
    Mode::Tier1,
    Mode::Tier2,
    Mode::Automatic,
];

impl Mode {
    pub const fn label(self) -> &'static str {
        match self {
            Mode::Interpreter => "interpreter",
            Mode::Tier1 => "tier1",
            Mode::Tier2 => "tier2",
            Mode::Automatic => "automatic",
        }
    }

    pub const fn policy(self) -> JitTierPolicy {
        match self {
            Mode::Interpreter => JitTierPolicy::Interpreter,
            Mode::Tier1 => JitTierPolicy::Tier1,
            Mode::Tier2 => JitTierPolicy::Tier2,
            Mode::Automatic => JitTierPolicy::Automatic,
        }
    }

    /// Eager thresholds so a single warm-up call queues the function and `poll`
    /// installs it before the observed run.
    pub fn config(self) -> JitConfig {
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(self.policy())
            .build()
    }
}

pub struct Case {
    pub dim: &'static str,
    pub name: &'static str,
    /// Defines the functions under test; must not throw and must not call
    /// them (the warm-up call is `warm`).
    pub setup: &'static str,
    /// Call that makes the defined functions hot. Empty when `setup` already
    /// warms them up (the differential suite's cases do).
    pub warm: &'static str,
    /// The observed computation. Returns the value under test, or throws.
    pub main: &'static str,
    /// Expression summarising global side effects after `main`.
    pub probe: &'static str,
}

/// A deterministic, tag-exact description of a result: comparing raw `JSValue`s
/// could compare addresses, and `String(v)` would hide int-vs-double tags.
pub fn fmt_value(ctx: &mut Context, v: JSValue) -> String {
    if v.is_undefined() {
        return "undefined".into();
    }
    if v.is_null() {
        return "null".into();
    }
    if let Some(b) = v.as_bool() {
        return format!("bool:{b}");
    }
    if let Some(i) = v.as_int() {
        return format!("int:{i}");
    }
    if let Some(f) = v.as_float() {
        // `{:?}` keeps -0.0 and spells NaN, unlike `{}`
        return format!("f64:{f:?}");
    }
    if let Some(s) = v.as_string(false) {
        return format!("str:{}", s.content().encode_utf8(false));
    }
    // BigInt and everything else go through ToPrimitive; the tag prefix keeps
    // them distinguishable.
    let tag = if v.is_bigint() { "bigint" } else { "other" };
    let mut scope = ctx.root_scope();
    match v.to_string(&mut scope) {
        Ok(s) => format!("{tag}:{}", s.content().encode_utf8(false)),
        Err(code) => format!("{tag}:<to_string:{code}>"),
    }
}

pub fn observe(ctx: &mut Context, res: JSResult) -> String {
    match res {
        Ok(v) => fmt_value(ctx, v),
        Err(code) => {
            // An interrupt carries no JS exception; an empty message still
            // compares equal across modes.
            let text = ctx.take_exception_as_text().message;
            format!("throw({code}):{text}")
        }
    }
}

pub fn eval_src(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

/// Run one case in one mode. Returns `(main, probe, metrics)`.
pub fn run_case(mode: Mode, case: &Case) -> (String, String, JitMetrics) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, mode.config()).expect("attach");
    let name = EvalName::Name(format!("diff-{}-{}.js", case.dim, case.name));

    let setup = eval_src(&mut ctx, &name, case.setup);
    assert!(setup.is_ok(), "setup must not throw: {}", case.name);
    jit.poll();

    let res = eval_src(&mut ctx, &name, case.main);
    let main = observe(&mut ctx, res);
    let res = eval_src(&mut ctx, &name, case.probe);
    let probe = observe(&mut ctx, res);

    (main, probe, jit.metrics())
}

/// Result of [`run_case_polled`]: both metric snapshots, so a caller can
/// assert on what `setup` queued (that is where the case's own function is
/// compiled or rejected) separately from what `main` ran.
pub struct PolledRun {
    pub main: String,
    pub probe: String,
    /// After the definition script's poll.
    pub after_definition: JitMetrics,
    /// After the warm-up's poll: the warm-up script and the functions it called.
    /// Metrics are cumulative, so a per-phase count is the delta against
    /// [`Self::after_definition`].
    pub after_warm: JitMetrics,
    /// After `main`'s poll: everything the observed run queued, drained.
    pub after_main: JitMetrics,
}

/// Like [`run_case`], but polls again after `main` and snapshots the metrics at
/// both points, so every queued function has been compiled or rejected. The
/// opcode gate needs that to assert "this function never compiled".
pub fn run_case_polled(mode: Mode, case: &Case) -> PolledRun {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, mode.config()).expect("attach");
    let name = EvalName::Name(format!("op-{}.js", case.name));

    // Definitions only: whatever this script queues is out of the assertions.
    let setup = eval_src(&mut ctx, &name, case.setup);
    assert!(setup.is_ok(), "setup must not throw: {}", case.name);
    jit.poll();
    let after_definition = jit.metrics();

    if !case.warm.is_empty() {
        let warm = eval_src(&mut ctx, &name, case.warm);
        assert!(warm.is_ok(), "warm-up must not throw: {}", case.name);
    }
    jit.poll();
    let after_warm = jit.metrics();

    let res = eval_src(&mut ctx, &name, case.main);
    let main = observe(&mut ctx, res);
    jit.poll();
    let after_main = jit.metrics();

    let res = eval_src(&mut ctx, &name, case.probe);
    let probe = observe(&mut ctx, res);

    PolledRun {
        main,
        probe,
        after_definition,
        after_warm,
        after_main,
    }
}
