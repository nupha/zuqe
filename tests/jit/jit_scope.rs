// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The GC scope's LOCAL list must not grow with the iteration count.
//!
//! The shape is a JIT frame whose callee reads a property **and** allocates, so
//! the callee's allocations and its promoted results land in the shared caller
//! scope. When that scope is never cleared, the list grows linearly and the
//! constructor path's `remove_local` (a linear scan of it) turns the whole loop
//! quadratic.
//!
//! `scope_local_peak` is the high-water mark of the LOCAL lists sampled at the
//! JIT's safepoints, so it is a deterministic witness: with the leak it tracks
//! the iteration count, without it stays flat.

#![cfg(feature = "jit")]

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitMetrics, JitTierPolicy,
    Runtime, ScopedContext,
};

use crate::common::settle;

/// Calls a constructor, a method that allocates and a method that reads a
/// property, once per iteration.
const DEFINE: &str = "\
function Vec(x, y){ this.x = x; this.y = y; }
Vec.prototype.dot = function (other){ return this.x * other.x + this.y * other.y; };
Vec.prototype.scale = function (k){ return new Vec(this.x * k, this.y * k); };
function workload(iterations, seed){ var sum = 0;
  for (var i = 0; i < iterations; i++){ var v = new Vec(i & 7, seed + 1);
    sum += v.dot(v.scale(2)); }
  return sum; }
workload(10, 1);
";

fn eval(ctx: &mut Context, src: &str) -> JSResult {
    ctx.eval(
        src.as_bytes(),
        &EvalName::Name("scope-locals.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
}

/// Run `workload(iterations, 1)` under `policy` in a fresh runtime.
fn run(policy: JitTierPolicy, iterations: u32) -> (String, JitMetrics) {
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
    eval(&mut ctx, DEFINE).expect("define");
    settle(&jit);
    let value = eval(&mut ctx, &format!("workload({iterations}, 1);")).expect("workload");
    let text = value
        .as_int()
        .map(|i| i.to_string())
        .expect("workload returns an int");
    settle(&jit);
    (text, jit.metrics())
}

#[test]
fn the_scope_local_list_does_not_grow_with_iterations() {
    let (small, small_metrics) = run(JitTierPolicy::Automatic, 5_000);
    let (large, large_metrics) = run(JitTierPolicy::Automatic, 50_000);
    let (expected, _) = run(JitTierPolicy::Interpreter, 5_000);

    assert_eq!(small, expected, "automatic disagreed with the interpreter");
    assert!(large_metrics.functions_compiled > 0, "nothing compiled");

    // The metric is only sampled when a safepoint runs a collection; a run
    // that never collects proves nothing.
    assert!(
        large_metrics.scope_local_peak > 0,
        "no collection was sampled at a safepoint (metrics: {large_metrics:?})"
    );
    // With the leak this is the iteration count (50k here); the fixed scope is
    // drained at every collection, so it stays in the hundreds.
    const BOUND: u64 = 4096;
    assert!(
        large_metrics.scope_local_peak < BOUND,
        "the LOCAL list grew with the iteration count: peak={} (5k run: {}, metrics: {large_metrics:?})",
        large_metrics.scope_local_peak,
        small_metrics.scope_local_peak,
    );
    println!(
        "scope locals: peak 5k={} 50k={} (bound {BOUND})",
        small_metrics.scope_local_peak, large_metrics.scope_local_peak
    );
}
