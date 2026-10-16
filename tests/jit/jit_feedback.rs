// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Epic F1: tier-2 type feedback.
//!
//! The interpreter records the operand types it observes at arithmetic,
//! comparison, bitwise, branch and local-update sites. Recording must be
//! **observation-only** — it can never change a result — and gated per plan
//! D-5: only `Tier2`/`Automatic` policies record; `Interpreter`/`Tier1` record
//! nothing. The recorded sites ride into `CompileSnapshot` where tier-2
//! specialization (F2) consumes them.

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
