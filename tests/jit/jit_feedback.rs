// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Epic F1/F2: tier-2 type feedback and numeric specialization.
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

#![cfg(feature = "jit")]

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, Jit, JitConfig, JitMetrics, JitTierPolicy, JSResult,
    Runtime, ScopedContext,
};

/// A numeric kernel exercising every feedback site kind: binary arithmetic
/// (`add`/`mul`), `add_loc`, comparisons (`lt`), bitwise (`and`), and a branch
/// on the loop condition. `warm` forces it hot so the JIT attaches.
const DEFINE: &str = "\
function kernel(n){ \
    var s = 0; \
    for (var i = 0; i < n; i++) { s = s + i * 2; } \
    if (s > 0) { s = s & 0xffff; } \
    return s; \
} \
kernel(1);";

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

fn run(policy: JitTierPolicy) -> FeedbackRun {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(policy)
            .build(),
    )
    .expect("attach");
    let name = EvalName::Name("jit-feedback.js".into());

    eval(&mut ctx, &name, DEFINE).expect("setup");
    jit.poll();

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
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(JitTierPolicy::Automatic)
            .build(),
    )
    .expect("attach");
    let name = EvalName::Name("jit-feedback-stable.js".into());

    eval(&mut ctx, &name, DEFINE).expect("setup");
    jit.poll();
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

/// Define `define`, warm it once, then observe `run` under `policy`.
fn run_script(policy: JitTierPolicy, define: &str, run: &str) -> (String, JitMetrics) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(policy)
            .build(),
    )
    .expect("attach");
    let name = EvalName::Name("jit-f2.js".into());

    eval(&mut ctx, &name, define).expect("setup");
    jit.poll();

    let res = eval(&mut ctx, &name, run);
    let out = tag_exact(res);
    (out, jit.metrics())
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
        "function acc(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } acc(1);";
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
#[test]
fn f2_guard_miss_and_overflow_stay_correct() {
    for (define, run) in EDGE_CASES {
        let reference = run_script(JitTierPolicy::Interpreter, define, run).0;
        let (out, m) = run_script(JitTierPolicy::Automatic, define, run);
        assert_eq!(
            out, reference,
            "script {define:?} diverged under specialization (specialized_ops={})",
            m.specialized_ops
        );
    }
}
