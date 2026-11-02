// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! M2b-4: a real interrupt delivered from inside generated code. The safepoint
//! on the loop back edge must exit through `JitExitKind::Interrupt`, be recorded
//! in `metrics.native_interrupts`, behave exactly like the interpreter (an
//! interrupt is control flow, not a JavaScript exception), and leave the runtime
//! usable afterwards. The interpreter-only run is the reference.

#![cfg(feature = "jit")]

use crate::common::settle;

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitMetrics, Runtime,
    ScopedContext,
};

/// `spin` compiles (loop back edge -> `HELPER_SAFEPOINT`); `guarded` wraps the
/// same call in `try`/`catch`, which the interpreter-only reference shows how
/// far an interrupt propagates.
const DEFINE: &str = "\
function spin(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } \
function guarded(n){ \
    try { return 'ran:' + spin(n); } \
    catch (e) { return 'caught:' + e; } \
} \
function spin_inner(n){ \
    var s = 0; \
    try { for (var i = 0; i < n; i++) s = (s + i) | 0; } \
    catch (e) { return 'inner-caught:' + e; } \
    return 'done:' + s; \
} \
spin(1); guarded(1); spin_inner(1);";

/// Long enough to expire the interrupt counter (10000 safepoints) comfortably
/// inside one native run.
const LONG_RUN: &str = "guarded(300000);";
/// The `catch` lives in the same frame as the interrupted loop.
const LONG_RUN_SAME_FRAME: &str = "spin_inner(300000);";

struct InterruptRun {
    /// What the long run produced.
    outcome: String,
    /// Whether an interrupt exit code was recorded for the run.
    interrupted: bool,
    metrics: JitMetrics,
    /// A short run after the handler is removed.
    recovered: String,
}

fn eval(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

fn describe(ctx: &mut Context, res: JSResult) -> String {
    match res {
        Ok(v) => {
            let mut scope = ctx.root_scope();
            match v.to_string(&mut scope) {
                Ok(s) => format!("value:{}", s.content().encode_utf8(false)),
                Err(code) => format!("value:<to_string:{code}>"),
            }
        }
        Err(code) => {
            let text = ctx.take_exception_as_text().message;
            format!("err({code}):{text}")
        }
    }
}

fn run(with_jit: bool, long_run: &str) -> InterruptRun {
    run_with(
        with_jit,
        DEFINE,
        "for (var w = 0; w < 40; w++) spin(2);",
        long_run,
        "spin(50) + '/' + guarded(3)",
    )
}

fn run_with(
    with_jit: bool,
    define: &str,
    prime: &str,
    long_run: &str,
    recover: &str,
) -> InterruptRun {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = with_jit.then(|| {
        Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .build(),
        )
        .expect("attach")
    });
    let name = EvalName::Name("jit-interrupt.js".into());

    eval(&mut ctx, &name, define).expect("setup");
    if let Some(jit) = &jit {
        settle(jit);
        assert!(
            jit.metrics().functions_compiled > 0,
            "the loop must be compiled for this test to mean anything"
        );
    }

    // The per-context interrupt counter starts at 0, so the very first poll
    // would fire. Prime it with a handler that never fires, so the countdown
    // only starts inside the long run below.
    ctx.rt_mut().set_interrupt_handler(|_| false);
    eval(&mut ctx, &name, prime).expect("priming run");
    // Arm it: the countdown now expires inside the loop's back edge.
    ctx.rt_mut().set_interrupt_handler(|_| true);
    let res = eval(&mut ctx, &name, long_run);
    let outcome = describe(&mut ctx, res);
    let interrupted = ctx.rt_mut().take_interrupt().is_some();
    ctx.rt_mut().unset_interrupt_handler();

    let res = eval(&mut ctx, &name, recover);
    let recovered = describe(&mut ctx, res);
    let metrics = jit.as_ref().map_or_else(JitMetrics::default, Jit::metrics);

    InterruptRun {
        outcome,
        interrupted,
        metrics,
        recovered,
    }
}

/// The interrupt is delivered by generated code (`metrics.native_interrupts`)
/// and every observable agrees with the interpreter-only run.
#[test]
fn native_loop_takes_the_interrupt_exit() {
    let native = run(true, LONG_RUN);
    let reference = run(false, LONG_RUN);

    assert!(
        native.interrupted,
        "no interrupt exit code was recorded for the native run"
    );
    assert!(
        native.metrics.native_interrupts > 0,
        "no native interrupt exit was observed (metrics: {:?})",
        native.metrics
    );
    assert!(native.metrics.native_entries > 0);
    assert_eq!(
        native.outcome, reference.outcome,
        "JIT and interpreter disagree on the interrupted run"
    );
    assert_eq!(
        native.interrupted, reference.interrupted,
        "interrupt recording differs from the interpreter"
    );
    println!(
        "cross-frame: outcome={} native_interrupts={}",
        native.outcome, native.metrics.native_interrupts
    );
}

/// An interrupt is not a JavaScript exception: a `catch` in the very frame the
/// interrupt fired in must not swallow it — and the JIT must behave exactly like
/// the interpreter, which owns that rule.
#[test]
fn interrupt_is_not_swallowed_by_a_catch_in_the_same_frame() {
    let native = run(true, LONG_RUN_SAME_FRAME);
    let reference = run(false, LONG_RUN_SAME_FRAME);

    assert!(
        native.metrics.native_interrupts > 0,
        "the same-frame loop never exited through native code"
    );
    assert!(
        !native.outcome.contains("inner-caught"),
        "the interrupted loop's own catch swallowed the interrupt: {}",
        native.outcome
    );
    assert!(native.interrupted, "no interrupt exit code was recorded");
    assert_eq!(
        native.outcome, reference.outcome,
        "JIT and interpreter disagree on the same-frame interrupt"
    );
    println!(
        "same-frame: outcome={} native_interrupts={}",
        native.outcome, native.metrics.native_interrupts
    );
}

/// T7.1: the loop inside the *inlined* body polls the caller frame and bails
/// to the general path when the poll asks to stop; the interrupt is then
/// delivered from the callee, exactly as the interpreter-only run does.
#[test]
fn an_inlined_loop_takes_the_interrupt_exit() {
    const DEFINE_INLINE: &str = "\
function sum(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } \
function inline_caller(n){ return sum(n) + 1; } \
inline_caller(1);";

    let native = run_with(
        true,
        DEFINE_INLINE,
        "for (var w = 0; w < 40; w++) inline_caller(2);",
        "inline_caller(300000);",
        "inline_caller(50);",
    );
    let reference = run_with(
        false,
        DEFINE_INLINE,
        "for (var w = 0; w < 40; w++) inline_caller(2);",
        "inline_caller(300000);",
        "inline_caller(50);",
    );

    assert!(
        native.metrics.inline_body_hits > 0,
        "the callee was not inlined, so the test does not cover T7.1: {:?}",
        native.metrics
    );
    assert!(native.interrupted, "no interrupt exit code was recorded");
    assert!(
        native.metrics.native_interrupts > 0,
        "no native interrupt exit was observed (metrics: {:?})",
        native.metrics
    );
    assert_eq!(
        native.outcome, reference.outcome,
        "JIT and interpreter disagree on the interrupted inlined loop"
    );
    assert_eq!(
        native.recovered, reference.recovered,
        "post-interrupt recovery differs from the interpreter"
    );
}

/// After an interrupt the handler is removed and the runtime must keep working.
#[test]
fn runtime_recovers_after_an_interrupt() {
    let native = run(true, LONG_RUN);
    let reference = run(false, LONG_RUN);

    assert!(
        native.recovered.starts_with("value:"),
        "runtime unusable after an interrupt: {}",
        native.recovered
    );
    assert_eq!(
        native.recovered, reference.recovered,
        "post-interrupt recovery differs from the interpreter"
    );
    assert_eq!(
        native.recovered, "value:1225/ran:3",
        "unexpected recovery result"
    );
}
