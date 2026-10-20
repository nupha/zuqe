// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The cost gate (P1-3, IMP-2): a function is compiled only when it runs
//! often enough to pay for its compile.
//!
//! The **cost** side is measured (how long a compile takes, `jit::rate`); the
//! **benefit** side is the host's estimate of what native execution saves
//! (`JitConfig::native_saving_ns_per_bytecode`). The gate is a floor under
//! the thresholds, so a function that is called a handful of times is not
//! compiled at all.
//!
//! The test compares two JITs in one process: with the gate off and with the
//! gate on. The difference does not depend on the machine, only the *order*
//! does — the gate needs a few measured compiles before it trusts the rate,
//! so the test warms it up first.

#![cfg(feature = "jit")]

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitMetrics, Runtime,
    ScopedContext,
};

/// A function called `CALLS` times: enough to cross the production call
/// threshold (32), far too few to pay for a compile.
const CALLS: usize = 40;

/// The definitions: `f0` … `f9` plus the function under test.
const DEFINE: &str = "\
function f0(){ return 0; } function f1(){ return 1; } function f2(){ return 2; }
function f3(){ return 3; } function f4(){ return 4; } function f5(){ return 5; }
function f6(){ return 6; } function f7(){ return 7; } function f8(){ return 8; }
function f9(){ return 9; }
function target(){ return 42; }";

fn eval(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

/// The production thresholds (32 calls / 56 back edges) with the cost gate
/// set to `saving` (0 turns it off).
fn config(saving: u64) -> JitConfig {
    JitConfig::builder()
        .call_threshold(32)
        .loop_threshold(56)
        .native_saving_ns_per_bytecode(saving)
        .build()
}

/// Call `f0` … `f9` `CALLS` times each: they cross the call threshold and are
/// compiled, which is what warms the measured rate up. Returns the metrics
/// after the warm-up, so a caller can measure what happens next.
fn warm_the_rate(jit: &Jit, ctx: &mut Context) -> JitMetrics {
    let name = EvalName::Name("jit-cost-warm.js".into());
    for i in 0..10 {
        let src = format!("for (var k = 0; k < {CALLS}; k++) f{i}();");
        eval(ctx, &name, &src).expect("warm-up");
    }
    settle(jit);
    jit.metrics()
}

/// Poll until the JIT is quiet (the background model compiles off-thread).
fn settle(jit: &Jit) {
    use std::thread;
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        jit.poll();
        let m = jit.metrics();
        if m.queue_len == 0 && m.functions_compiling == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "jit never settled: {m:?}");
        thread::sleep(Duration::from_micros(50));
    }
}

/// Run `target()` `CALLS` times and report how many compiles happened
/// **after** the warm-up.
fn run(saving: u64) -> (u64, JitMetrics) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(saving)).expect("attach");
    let name = EvalName::Name("jit-cost.js".into());

    eval(&mut ctx, &name, DEFINE).expect("define");
    settle(&jit);
    let warm = warm_the_rate(&jit, &mut ctx);

    let src = format!("for (var k = 0; k < {CALLS}; k++) target();");
    eval(&mut ctx, &name, &src).expect("run");
    settle(&jit);
    let m = jit.metrics();
    (
        m.functions_compiled.saturating_sub(warm.functions_compiled),
        m,
    )
}

/// With the gate off, the function crosses the configured threshold and is
/// compiled — the cost of the compile is paid for nothing (it is called 40
/// times, which cannot pay for a compile that costs milliseconds).
#[test]
fn without_the_gate_the_function_is_compiled() {
    let (compiled, m) = run(0);
    assert_eq!(compiled, 1, "the function was not compiled: {m:?}");
}

/// With the gate on, the same function is **not** compiled: its compile
/// cannot pay for itself.
#[test]
fn the_cost_gate_holds_back_a_function_that_cannot_pay_for_its_compile() {
    let (compiled, m) = run(1);
    assert_eq!(
        compiled, 0,
        "the cost gate let a function through that cannot pay for its compile: {m:?}"
    );
}

/// The measured rate is what the gate uses: after a few compiles it is
/// known, and it is the cost side of the decision.
///
/// The rate is process-wide, so this test cannot claim anything about the
/// state *before* the first compile (another test in the same binary may
/// have compiled already).
#[test]
fn the_compile_rate_is_measured() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(0)).expect("attach");
    let name = EvalName::Name("jit-cost-rate.js".into());

    eval(&mut ctx, &name, DEFINE).expect("define");
    settle(&jit);
    warm_the_rate(&jit, &mut ctx);
    let rate = jit
        .compile_rate_ns_per_bytecode()
        .expect("the rate is known after a few compiles");
    assert!(rate > 0, "a measured rate of zero: {rate}");
}
