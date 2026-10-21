// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Epic F1/F2/F5: tier-2 type feedback, numeric specialization and the
//! profitability gate.
//!
//! F1: the interpreter records the operand types it observes at arithmetic,
//! comparison, bitwise, branch and local-update sites. Recording must be
//! **observation-only** — it can never change a result — and gated per plan
//! D-5: only `Tier2`/`Automatic` policies record; `Interpreter`/`Tier1` record
//! nothing. The recorded sites ride into `CompileSnapshot`.
//!
//! F2: the compiler consumes that feedback. Where a site is int-monomorphic it
//! emits a speculative inline int fast path for the local-update ops
//! (`add_loc`/`inc_loc`/`dec_loc`) that today always pay an `extern "C"` helper
//! call. A guard miss or an `i32` overflow hands the op back to the helper —
//! which is the interpreter's own `arith_add`/`UnaryOp`, so it stays in native
//! code and the result is bit-identical.
//!
//! F5: specialization is not free, so `Automatic` only takes it where the
//! interpreter measured enough samples at the site
//! (`JitConfig::specialization_min_samples`); forced `Tier2` is a fixed policy
//! and takes every recorded site. The two policies must therefore disagree on
//! thin evidence — that divergence is what the F5 tests below pin down.

#![cfg(feature = "jit")]

use crate::common::settle;

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitConfigBuilder, JitMetrics,
    JitTierPolicy, Runtime, ScopedContext,
};

/// A numeric kernel exercising every feedback site kind: binary arithmetic
/// (`add`/`mul`), `add_loc`, comparisons (`lt`), bitwise (`and`), and a branch
/// on the loop condition. `warm` runs the loop twenty times so the sites carry
/// enough samples to clear the epic F5 profitability gate under `Automatic`.
const DEFINE: &str = "\
function kernel(n){ \
    var s = 0; \
    for (var i = 0; i < n; i++) { s = s + i * 2; } \
    if (s > 0) { s = s & 0xffff; } \
    return s; \
} \
kernel(20);";

/// The observed run; must produce the same value under every policy.
const RUN: &str = "kernel(500)";

struct FeedbackRun {
    result: String,
    metrics: JitMetrics,
}

fn eval(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

fn describe(ctx: &mut Context, res: JSResult) -> String {
    match res {
        Ok(v) => {
            let mut scope = ctx.root_scope();
            match v.to_string(&mut scope) {
                Ok(s) => s.content().encode_utf8(false).into_owned(),
                Err(code) => format!("<to_string:{code}>"),
            }
        }
        Err(code) => {
            let text = ctx.take_exception_as_text().message;
            format!("throw({code}):{text}")
        }
    }
}

/// Eager thresholds so a single warm-up call queues the function and `poll`
/// installs it before the observed run. F5 tests tweak the returned builder
/// (e.g. `specialization_min_samples`) before `build()`.
///
/// `auto_poll(false)` is required by every test here: they measure what the
/// compiler did with the feedback the *warm-up* recorded, so compilation must
/// happen at the explicit `poll` after it. With the automatic poll on, a hot
/// loop would compile at its first back edge (N3, inline OSR) with only the
/// observations made so far — far below the gate these tests exercise.
fn config(policy: JitTierPolicy) -> JitConfigBuilder {
    JitConfig::builder()
        .call_threshold(1)
        .loop_threshold(1)
        .auto_poll(false)
        .tier_policy(policy)
}

fn run(policy: JitTierPolicy) -> FeedbackRun {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(policy).build()).expect("attach");
    let name = EvalName::Name("jit-feedback.js".into());

    eval(&mut ctx, &name, DEFINE).expect("setup");
    settle(&jit);

    let res = eval(&mut ctx, &name, RUN);
    let result = describe(&mut ctx, res);

    FeedbackRun {
        result,
        metrics: jit.metrics(),
    }
}

/// Under `Automatic` the interpreter must record feedback sites, and the
/// recorded set must be non-empty after a numeric kernel has run.
#[test]
fn automatic_policy_records_feedback() {
    let run = run(JitTierPolicy::Automatic);
    assert!(
        run.metrics.feedback_sites > 0,
        "no feedback recorded under Automatic"
    );
}

/// Plan D-5: `Tier1` is baseline-only, so it must never pay the recording
/// cost — even though it still compiles.
#[test]
fn tier1_policy_records_nothing() {
    let run = run(JitTierPolicy::Tier1);
    assert_eq!(
        run.metrics.feedback_sites, 0,
        "Tier1 must not record feedback"
    );
    // ... but it still compiles the kernel.
    assert!(run.metrics.functions_compiled > 0, "Tier1 compiled nothing");
}

/// Plan D-5: `Interpreter` is fully inert — no compile, no feedback.
#[test]
fn interpreter_policy_records_nothing() {
    let run = run(JitTierPolicy::Interpreter);
    assert_eq!(run.metrics.feedback_sites, 0);
    assert_eq!(run.metrics.functions_compiled, 0);
}

/// The core safety property: recording must not change the observable result.
/// The interpreter policy is the reference.
#[test]
fn recording_does_not_change_results() {
    let reference = run(JitTierPolicy::Interpreter);
    for policy in [JitTierPolicy::Tier1, JitTierPolicy::Automatic] {
        let got = run(policy);
        assert_eq!(
            got.result, reference.result,
            "policy {policy:?} changed the result"
        );
    }
}

/// Feedback is per-function and must survive a `poll` that compiles other
/// functions: a second kernel run keeps accumulating at the same sites rather
/// than resetting. We assert the count is stable (not growing unboundedly with
/// repeated identical runs once sites are established).
#[test]
fn site_count_is_stable_across_repeated_runs() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(JitTierPolicy::Automatic).build()).expect("attach");
    let name = EvalName::Name("jit-feedback-stable.js".into());

    eval(&mut ctx, &name, DEFINE).expect("setup");
    settle(&jit);
    let after_first = jit.metrics().feedback_sites;
    assert!(after_first > 0, "no feedback after first run");

    // Repeated runs hit the *same* pcs; the distinct-site count must not grow.
    for _ in 0..10 {
        eval(&mut ctx, &name, RUN).expect("rerun");
    }
    assert_eq!(
        jit.metrics().feedback_sites,
        after_first,
        "repeated identical runs grew the distinct-site count"
    );
}

/// The table keys its sites by the bytecode pc and hashes them with the JIT's
/// fast mixer. A hash that dropped a site or merged two of them would starve
/// the tier-2 gate — what the gate reads is a site's *samples* — so every site
/// must keep accumulating them, and the answer must stay the interpreter's.
#[test]
fn every_site_keeps_its_samples() {
    const DEFINE: &str = "\
        function f(n){ \
          var s = 0; \
          for (var i = 0; i < n; i++) { s = s + i * 2 - 1; } \
          return s; \
        } \
        f(50);";
    let reference = run_script(JitTierPolicy::Interpreter, DEFINE, "f(500)").0;
    let (out, m) = run_script(JitTierPolicy::Automatic, DEFINE, "f(500)");
    assert_eq!(out, reference, "the recorded feedback changed the result");
    // One site per pc: the loop condition, the increment, the three
    // arithmetic ops and the accumulator update.
    assert_eq!(
        m.feedback_sites, 6,
        "the recorded sites changed (metrics: {m:?})"
    );
    // The samples accumulated: the profitability gate wants eight of them,
    // so a site that kept its samples is what lets the specialization through.
    assert!(
        m.specialized_ops > 0,
        "no site reached the gate (metrics: {m:?})"
    );
}

// ── Epic F2: feedback-driven numeric specialization ──

/// A tag-exact description, so an `Int` that silently became a `Float64`
/// (the overflow path) cannot masquerade as the same value.
fn tag_exact(res: JSResult) -> String {
    match res {
        Ok(v) => {
            if let Some(i) = v.as_int() {
                format!("int:{i}")
            } else if let Some(f) = v.as_float() {
                format!("f64:{f:?}")
            } else if let Some(s) = v.as_string(false) {
                format!("str:{}", s.content().encode_utf8(false))
            } else if v.is_undefined() {
                "undefined".into()
            } else if v.is_null() {
                "null".into()
            } else if let Some(b) = v.as_bool() {
                format!("bool:{b}")
            } else {
                "other".into()
            }
        }
        Err(code) => format!("throw({code})"),
    }
}

/// Define `define`, warm it once, then observe `run` under `cfg`.
fn run_with(cfg: JitConfig, define: &str, run: &str) -> (String, JitMetrics) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, cfg).expect("attach");
    let name = EvalName::Name("jit-f2.js".into());

    eval(&mut ctx, &name, define).expect("setup");
    settle(&jit);

    let res = eval(&mut ctx, &name, run);
    let out = tag_exact(res);
    (out, jit.metrics())
}

/// [`run_with`] under the stock eager config for `policy`.
fn run_script(policy: JitTierPolicy, define: &str, run: &str) -> (String, JitMetrics) {
    run_with(config(policy).build(), define, run)
}

/// A for-loop's `i++` compiles to `inc_loc`, which the baseline always routed
/// through an `extern "C"` helper. Under `Automatic` F1 records the site as
/// int-monomorphic and F2 replaces it with an inline fast path.
#[test]
fn f2_specializes_a_hot_int_loop() {
    let reference = run_script(JitTierPolicy::Interpreter, DEFINE, "kernel(500)").0;
    let (out, m) = run_script(JitTierPolicy::Automatic, DEFINE, "kernel(500)");
    assert!(m.specialized_ops > 0, "Automatic specialized no site");
    assert_eq!(out, reference, "specialization changed the result");
}

/// `sum += i` is the canonical `add_loc` shape; assert it too gets specialized
/// so the F2 path is exercised for both `add_loc` and `inc_loc`.
#[test]
fn f2_specializes_the_accumulator_pattern() {
    const ACC: &str =
        "function acc(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } acc(20);";
    let reference = run_script(JitTierPolicy::Interpreter, ACC, "acc(500)").0;
    let (out, m) = run_script(JitTierPolicy::Automatic, ACC, "acc(500)");
    assert!(m.specialized_ops > 0, "Automatic specialized no site");
    assert_eq!(out, reference);
}

/// Plan D-5: the baseline policies never record feedback, so they can never
/// specialize — the fast path is unreachable for them.
#[test]
fn f2_baseline_policies_never_specialize() {
    for policy in [JitTierPolicy::Interpreter, JitTierPolicy::Tier1] {
        let (_, m) = run_script(policy, DEFINE, "kernel(50)");
        assert_eq!(m.specialized_ops, 0, "{policy:?} specialized a site");
    }
}

/// Epic P7 (task 7.1): an `inc_loc` whose *result* overflows to `Float64` is
/// recorded as non-int, so the compiler refuses to specialize it — its inline
/// fast path would bail to the helper on every trip. The sibling `k++` site in
/// the same function (result stays `Int`) still specializes, so the two sites
/// are demonstrably judged apart. Forced `Tier2` keeps the gate at one sample.
#[test]
fn an_overflowing_result_is_not_specialized() {
    const CLEAN: &str =
        "function f(){ var i = 0; for (var k = 0; k < 30; k++){} i++; return i + k; } f();";
    const OVERFLOW: &str =
        "function f(){ var i = 2147483647; for (var k = 0; k < 30; k++){} i++; return i + k; } f();";

    let (clean_out, clean) = run_script(JitTierPolicy::Tier2, CLEAN, "f()");
    let (over_out, over) = run_script(JitTierPolicy::Tier2, OVERFLOW, "f()");
    assert_eq!(
        run_script(JitTierPolicy::Interpreter, OVERFLOW, "f()").0,
        over_out,
        "the gate must not change the result"
    );
    assert_eq!(
        run_script(JitTierPolicy::Interpreter, CLEAN, "f()").0,
        clean_out
    );
    assert!(
        clean.specialized_ops > over.specialized_ops,
        "the clean `i++` site must specialize while the overflowing one does not \
         (clean={}, overflow={})",
        clean.specialized_ops,
        over.specialized_ops
    );
}

/// Scripts that drive a specialized int local *out* of the int domain. The F2
/// guard must hand each one back to the helper, and every result must be
/// bit-identical to the interpreter's.
const EDGE_CASES: &[(&str, &str)] = &[
    // i32 overflow: the accumulator promotes to Float64 partway through, so
    // both the guard and the overflow branch are taken.
    (
        "function f(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } f(1);",
        "f(100000)",
    ),
    // i32::MAX boundary for `inc_loc`: `i` hits 2147483647 exactly.
    (
        "function f(){ var i = 2147483647; i++; return i; } f();",
        "f()",
    ),
    // i32::MIN boundary for `dec_loc`.
    (
        "function f(){ var i = -2147483648; i--; return i; } f();",
        "f()",
    ),
    // A local flipped to a string mid-loop: `+=` becomes string concat.
    (
        "function f(n){ var s = 0; for (var i = 0; i < n; i++){ if (i === 4) s = 'k'; s += i; } return s; } f(1);",
        "f(200)",
    ),
    // The *operand* (not the accumulator) is sometimes a string.
    (
        "function f(n){ var s = 0; for (var i = 0; i < n; i++){ var v = (i === 3) ? 'q' : i; s += v; } return s; } f(1);",
        "f(200)",
    ),
    // A local that flips to a float, so `inc_loc`'s guard misses every call.
    (
        "function f(n){ var s = 0.5; for (var i = 0; i < n; i++) s++; return s; } f(1);",
        "f(200)",
    ),
];

/// Guard miss, type change and overflow must all deoptimize to the helper and
/// reproduce the interpreter's answer exactly — tag included.
///
/// The JIT leg runs forced `Tier2`: these bodies warm for a single call, which
/// the epic F5 gate would refuse under `Automatic`, and the point here is that
/// the *specialized* path deoptimizes correctly, not that it was chosen.
#[test]
fn f2_guard_miss_and_overflow_stay_correct() {
    let mut specialized = 0u64;
    for (define, run) in EDGE_CASES {
        let reference = run_script(JitTierPolicy::Interpreter, define, run).0;
        let (out, m) = run_script(JitTierPolicy::Tier2, define, run);
        assert_eq!(
            out, reference,
            "script {define:?} diverged under specialization (specialized_ops={})",
            m.specialized_ops
        );
        specialized += m.specialized_ops;
    }
    // Anti-vacuity: if nothing specialized, the guard paths above were never
    // exercised and the assertions only proved the baseline stays correct.
    assert!(
        specialized > 0,
        "no edge case reached the specialized path at all"
    );
}

/// P7 7.2 instrumentation: a specialized `inc_loc` that meets a non-`int` local
/// must be counted as a guard miss, so the host can read how much a speculation
/// costs. A baseline tier never specializes and must count nothing.
#[test]
fn guard_misses_are_counted_for_speculative_sites() {
    // Warm `i++` with an int so F2 specializes it, then call with a float.
    const DEFINE: &str =
        "function f(n, x){ var i = x; for (var k = 0; k < n; k++){ i++; } return i; } f(3, 1);";
    let r = "f(3, 1.5)";
    let reference = run_script(JitTierPolicy::Interpreter, DEFINE, r).0;
    let (out, m) = run_script(JitTierPolicy::Tier2, DEFINE, r);
    assert_eq!(out, reference, "the bail changed the result");
    assert!(
        m.specialized_ops > 0,
        "the inc_loc site was not specialized"
    );
    assert!(
        m.specialized_guard_misses > 0,
        "a specialized site that bailed was not counted as a guard miss"
    );
    let (_, base) = run_script(JitTierPolicy::Tier1, DEFINE, r);
    assert_eq!(
        base.specialized_guard_misses, 0,
        "a baseline tier counted a guard miss"
    );
}

/// P7 7.2 tier-down: a function whose speculative site keeps missing is
/// recompiled without speculation, after which it stops paying the guards.
#[test]
fn a_miss_heavy_site_is_recompiled_without_speculation() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(JitTierPolicy::Tier2).build()).expect("attach");
    let name = EvalName::Name("jit-tierdown.js".into());

    eval(
        &mut ctx,
        &name,
        "function f(n, x){ var i = x; for (var k = 0; k < n; k++){ i++; } return i; } f(3, 1);",
    )
    .expect("setup");
    settle(&jit);
    let m0 = jit.metrics();
    assert!(
        m0.specialized_ops > 0,
        "the inc_loc site was not specialized"
    );

    // Enough float calls to miss past the tier-down threshold; the crossing
    // activation queues the non-speculative recompile itself.
    eval(&mut ctx, &name, "f(200, 1.5)").expect("miss run");
    let after = jit.metrics();
    assert!(
        after.specialized_guard_misses > 0,
        "no guard miss was counted"
    );
    assert!(after.functions_queued > m0.functions_queued, "no requeue");

    settle(&jit);
    let before = jit.metrics().specialized_guard_misses;
    eval(&mut ctx, &name, "f(200, 1.5)").expect("post-tier-down run");
    assert_eq!(
        jit.metrics().specialized_guard_misses,
        before,
        "the tier-down code still speculated"
    );
}

// ── Epic F5: profitability gate and tier selection ──

/// One `f(3)` warm-up: every local-update site holds three samples when the
/// eager thresholds queue the function — below the default gate of 8.
const THIN: &str =
    "function f(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } f(3);";
/// Same body, warmed fifty times: above the default gate.
const THICK: &str =
    "function f(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } f(50);";

/// F5: `Automatic` will not spend a speculative path on three observations —
/// but the function still compiles (baseline), and the answer is unchanged.
#[test]
fn f5_automatic_gates_thin_evidence() {
    let reference = run_script(JitTierPolicy::Interpreter, THIN, "f(10)").0;
    let (out, m) = run_script(JitTierPolicy::Automatic, THIN, "f(10)");
    assert!(
        m.functions_compiled > 0,
        "the function must still be compiled (metrics: {m:?})"
    );
    assert_eq!(
        m.specialized_ops, 0,
        "Automatic specialized a three-sample site (metrics: {m:?})"
    );
    assert_eq!(out, reference, "the gate must not change the result");
}

/// F5: forced `Tier2` is a fixed policy — the same three samples are enough,
/// so the two policies now demonstrably disagree.
#[test]
fn f5_forced_tier2_skips_the_gate() {
    let reference = run_script(JitTierPolicy::Interpreter, THIN, "f(10)").0;
    let (out, m) = run_script(JitTierPolicy::Tier2, THIN, "f(10)");
    assert!(
        m.specialized_ops > 0,
        "Tier2 refused to specialize thin evidence (metrics: {m:?})"
    );
    assert_eq!(out, reference, "specialization changed the result");
}

/// F5: enough evidence and `Automatic` reaches tier 2 exactly as before — the
/// gate must not freeze production out of specialization.
#[test]
fn f5_automatic_specializes_once_the_gate_clears() {
    let reference = run_script(JitTierPolicy::Interpreter, THICK, "f(10)").0;
    let (out, m) = run_script(JitTierPolicy::Automatic, THICK, "f(10)");
    assert!(
        m.specialized_ops > 0,
        "a fifty-sample site must clear the default gate (metrics: {m:?})"
    );
    assert_eq!(out, reference, "specialization changed the result");
}

/// F5: the gate is a host-tunable knob, not a hidden constant — lowering it
/// lets the three-sample site through under the same `Automatic` policy.
#[test]
fn f5_gate_is_host_tunable() {
    let cfg = config(JitTierPolicy::Automatic)
        .specialization_min_samples(2)
        .build();
    let (_, m) = run_with(cfg, THIN, "f(10)");
    assert!(
        m.specialized_ops > 0,
        "specialization_min_samples(2) should accept three samples (metrics: {m:?})"
    );
}

// ── TH2: the two gates (plan §13.6.5 item 1) ──

/// A kernel with a hot int loop: the shape the tier-2 specialization pays
/// off on.
const TIERED: &str =
    "function f(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } f(50);";

/// The two gates, end to end.
///
/// The first gate compiles the **baseline** tier: native code without the
/// tier-2 specialization. The second gate promotes the function to the
/// **optimizing** tier. The promotion is driven by the work the *native* tier
/// reports back — the interpreter stops counting a function's entries and back
/// edges once its code is installed, so without that report the second gate
/// could never be crossed.
#[test]
fn the_second_gate_promotes_a_hot_function() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let cfg = JitConfig::builder()
        .call_threshold(2)
        .loop_threshold(4)
        .tier2_call_threshold(4)
        .tier2_loop_threshold(8)
        .auto_poll(false)
        .tier_policy(JitTierPolicy::Automatic)
        // This test exercises the *mechanics* of the two gates, not the
        // economics: the cost gate (P1-3) would hold the function back,
        // because it is not called often enough to pay for a compile.
        .native_saving_ns_per_bytecode(0)
        .build();
    let jit = Jit::attach_to(&rt, cfg).expect("attach");
    let name = EvalName::Name("jit-tiering.js".into());

    eval(&mut ctx, &name, TIERED).expect("setup");
    settle(&jit);
    let first = jit.metrics();
    assert!(
        first.functions_compiled > 0,
        "the first gate did not compile the function (metrics: {first:?})"
    );
    assert_eq!(
        first.specialized_ops, 0,
        "the first gate compiles the baseline tier (metrics: {first:?})"
    );
    assert_eq!(
        first.tier2_promotions, 0,
        "the promotion belongs to the second gate (metrics: {first:?})"
    );

    // The function now runs natively; its work feeds the second gate.
    let reference = tag_exact(eval(&mut ctx, &name, "f(500)"));
    settle(&jit);
    let second = jit.metrics();
    assert_eq!(
        second.tier2_promotions, 1,
        "the second gate did not promote the function (metrics: {second:?})"
    );
    assert!(
        second.specialized_ops > 0,
        "the optimizing tier must specialize (metrics: {second:?})"
    );
    assert_eq!(
        second.functions_recompiled, 1,
        "the promotion is a recompile (metrics: {second:?})"
    );

    // The promotion must not change the answer.
    let out = tag_exact(eval(&mut ctx, &name, "f(500)"));
    assert_eq!(out, reference, "the promotion changed the result");
    assert_eq!(
        tag_exact(eval(&mut ctx, &name, "f(500)")),
        reference,
        "the second run must agree too"
    );
}

/// The tier-2 gate is a host-tunable knob, like the first one.
#[test]
fn the_second_gate_is_host_tunable() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    // A tier-2 gate no function can reach: the promotion never happens.
    let cfg = JitConfig::builder()
        .call_threshold(2)
        .loop_threshold(4)
        .tier2_call_threshold(1_000_000)
        .tier2_loop_threshold(1_000_000)
        .auto_poll(false)
        .tier_policy(JitTierPolicy::Automatic)
        // The cost gate (P1-3) is off here: this test is about the gates'
        // mechanics, not about whether a compile pays for itself.
        .native_saving_ns_per_bytecode(0)
        .build();
    let jit = Jit::attach_to(&rt, cfg).expect("attach");
    let name = EvalName::Name("jit-tiering-unreachable.js".into());

    eval(&mut ctx, &name, TIERED).expect("setup");
    settle(&jit);
    eval(&mut ctx, &name, "f(5000)").expect("run");
    settle(&jit);
    let m = jit.metrics();
    assert!(m.functions_compiled > 0, "the first gate compiled");
    assert_eq!(
        m.tier2_promotions, 0,
        "an unreachable second gate must not promote (metrics: {m:?})"
    );
    assert_eq!(m.specialized_ops, 0, "the baseline tier never specializes");
}

/// The forced policies ask for a fixed tier, so there is only one gate: the
/// `Tier1` policy never specializes and the `Tier2` policy specializes right
/// away, whatever the thresholds say.
#[test]
fn the_forced_policies_ignore_the_gates() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let cfg = JitConfig::builder()
        .call_threshold(2)
        .loop_threshold(4)
        .tier2_call_threshold(1_000_000)
        .tier2_loop_threshold(1_000_000)
        .auto_poll(false)
        .tier_policy(JitTierPolicy::Tier2)
        // The cost gate (P1-3) is off here: this test is about the forced
        // policies, not about whether a compile pays for itself.
        .native_saving_ns_per_bytecode(0)
        .build();
    let jit = Jit::attach_to(&rt, cfg).expect("attach");
    let name = EvalName::Name("jit-forced-tier.js".into());

    eval(&mut ctx, &name, TIERED).expect("setup");
    settle(&jit);
    let m = jit.metrics();
    assert!(
        m.specialized_ops > 0,
        "the forced tier-2 policy specializes"
    );
    assert_eq!(
        m.tier2_promotions, 0,
        "a single-tier policy has no promotion (metrics: {m:?})"
    );
}
