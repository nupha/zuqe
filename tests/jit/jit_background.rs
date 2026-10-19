// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Epic D3 / plan §12: the background compiler (`jit_background`, in
//! `default`). Compilation moves to worker threads while the runtime thread only
//! dispatches, drains and installs, so `Jit::poll` no longer blocks on codegen.
//!
//! With `jit_background` off (`--no-default-features`, `jit` on) compilation is
//! inline: the whole queue is compiled before `poll` returns. Here `poll` is
//! non-blocking and an attempt may still be in flight, so every assertion drives
//! the JIT **to rest** first ([`settle`]), except the ones that check the
//! asynchronous handoff itself.
//!
//! The suite compiles to nothing without the feature, so the inline build run
//! and the `jit`-only integration run are unaffected.

#![cfg(all(feature = "jit", feature = "jit_background"))]

use std::{thread, time::Duration};

use crate::common::{eval_src, observe, settle};
use zuqe::{
    Context, EvalFlags, EvalName, EvalType, Jit, JitConfig, JitTierPolicy, Runtime, ScopedContext,
};

/// An eager config so the first warm-up queues the function under test.
fn eager() -> zuqe::JitConfigBuilder {
    JitConfig::builder().call_threshold(1).loop_threshold(1)
}

fn eval_observed(ctx: &mut Context, src: &[u8], name: &str) -> String {
    let result = ctx.eval(
        src,
        &EvalName::Name(name.into()),
        EvalType::Global,
        EvalFlags::empty(),
    );
    observe(ctx, result)
}

/// D3 gate: the background compiler must not change any observable result.
/// Each case defines and warms a function, the background JIT is driven to rest
/// so the code is installed, then the function is called again (through the
/// installed entry) and compared bit-for-bit with the interpreter.
#[test]
fn background_compilation_is_bit_identical() {
    const CASES: &[(&str, &str)] = &[
        (
            "function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } fib(10);",
            "fib(18)",
        ),
        (
            "function sum(a){ var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; return s; } sum([1,2,3]);",
            "sum([10,20,30,40,50,60,70,80])",
        ),
        (
            "function mix(a, b){ var r = 0; for (var i = 0; i < 2000; i++) r = (r + a * i - b) | 0; return r; } mix(3, 7);",
            "mix(11, 5)",
        ),
    ];

    for (index, (setup, call)) in CASES.iter().enumerate() {
        let expected = {
            let mut rt = Runtime::create().unwrap();
            let mut ctx = rt
                .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
                .unwrap();
            let name = EvalName::Name(format!("bg-ref-{index}.js"));
            assert!(eval_src(&mut ctx, &name, setup).is_ok(), "setup threw");
            eval_observed(&mut ctx, call.as_bytes(), &format!("bg-ref-call-{index}"))
        };

        // Background: define + warm, settle (compile off-thread), then call.
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(&rt, eager().build()).unwrap();
        let name = EvalName::Name(format!("bg-{index}.js"));
        assert!(eval_src(&mut ctx, &name, setup).is_ok(), "setup threw");
        settle(&jit);
        let got = eval_observed(&mut ctx, call.as_bytes(), &format!("bg-call-{index}"));
        assert_eq!(got, expected, "case {index} diverged from the interpreter");

        let m = jit.metrics();
        assert!(
            m.functions_compiled > 0,
            "case {index} compiled nothing ({m:?})"
        );
    }
}

/// The point of the feature: a compile is handed to a worker, not run inline.
/// With `auto_poll` off, one `poll` only dispatches — it must return with an
/// attempt in flight and nothing installed yet. A later poll installs it.
#[test]
fn compilation_happens_off_the_runtime_thread() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager().auto_poll(false).build()).unwrap();

    assert!(
        eval_src(
            &mut ctx,
            &EvalName::Name("bg-offthread-def.js".into()),
            "function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(1);",
        )
        .is_ok()
    );

    // One poll: dispatch only. The completion is not drained in the same call,
    // so this is deterministic, not a race.
    jit.poll();
    let dispatched = jit.metrics();
    assert!(
        dispatched.functions_compiling > 0,
        "one poll should leave an attempt in flight ({dispatched:?})"
    );
    assert_eq!(
        dispatched.functions_compiled, 0,
        "the attempt was compiled inline, not on a worker ({dispatched:?})"
    );

    settle(&jit);
    let settled = jit.metrics();
    assert!(
        settled.functions_compiled > 0,
        "the in-flight attempt never installed ({settled:?})"
    );
    assert_eq!(
        settled.functions_compiling, 0,
        "still compiling ({settled:?})"
    );
}

/// Automatic polling must make progress and converge without a host call: the
/// event-loop/trampoline safe points drain the workers, so a second eval runs
/// the function natively.
#[test]
fn auto_poll_drains_workers_and_the_function_runs_native() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager().build()).unwrap();
    let name = EvalName::Name("bg-autopoll.js".into());

    assert!(
        eval_src(
            &mut ctx,
            &name,
            "function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(1000);",
        )
        .is_ok()
    );
    // No host poll: whatever happened is the engine's automatic polling.
    let during = jit.metrics();
    assert!(during.functions_queued > 0, "nothing queued ({during:?})");

    // Drive to rest, then call again through the installed entry.
    settle(&jit);
    let before = jit.metrics();
    assert!(
        before.functions_compiled > 0,
        "nothing installed ({before:?})"
    );
    assert!(eval_src(&mut ctx, &name, "f(1000)").is_ok());
    let after = jit.metrics();
    assert!(
        after.native_entries > before.native_entries,
        "the compiled function never ran natively ({after:?})"
    );
}

/// `compile_timeout_ms` is a real deadline in the background model: an attempt
/// past it is abandoned (counted) and its late completion is dropped by attempt
/// id. Sleeping past the budget before the next poll makes this deterministic —
/// expiry runs before the drain.
#[test]
fn an_attempt_past_the_budget_is_abandoned() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager().auto_poll(false).compile_timeout_ms(1).build()).unwrap();

    assert!(
        eval_src(
            &mut ctx,
            &EvalName::Name("bg-timeout.js".into()),
            "function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(1);",
        )
        .is_ok()
    );

    jit.poll(); // dispatch
    thread::sleep(Duration::from_millis(20)); // past the 1 ms budget
    jit.poll(); // expire, then drain (the late result is discarded)
    let m = jit.metrics();
    assert!(m.compile_timeouts > 0, "the budget never fired ({m:?})");

    // The abandoned attempt is requeued; a quiet drive must still terminate
    // (with a budget below one compile every retry is abandoned too, so the
    // function ends up parked in the interpreter instead of spinning).
    settle(&jit);
    let m = jit.metrics();
    assert_eq!(m.queue_len, 0, "the queue never drained ({m:?})");
    assert_eq!(m.functions_compiling, 0, "still compiling ({m:?})");
}

/// Dropping the runtime (and with it the `JitState`) must shut every worker
/// down; a leaked join would hang this test.
#[test]
fn dropping_a_jit_with_work_in_flight_joins_every_worker() {
    for _ in 0..8 {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(&rt, eager().workers(4).auto_poll(false).build()).unwrap();
        assert!(
            eval_src(
                &mut ctx,
                &EvalName::Name("bg-drop.js".into()),
                "function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(1);",
            )
            .is_ok()
        );
        // Dispatch and drop without draining: the workers are mid-flight.
        jit.poll();
        drop(jit);
        drop(ctx);
        drop(rt);
    }
}

/// `workers` now parallelizes: several functions queued at once are handed to
/// distinct workers and all settle.
#[test]
fn multiple_workers_compile_the_backlog() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager().workers(4).auto_poll(false).build()).unwrap();

    assert!(
        eval_src(
            &mut ctx,
            &EvalName::Name("bg-parallel.js".into()),
            "function a(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; }
             function b(n){ var s = 1; for (var i = 0; i < n; i++) s = (s * 3 + i) | 0; return s; }
             function c(n){ var s = 2; for (var i = 0; i < n; i++) s = (s - i) | 0; return s; }
             function d(n){ var s = 3; for (var i = 0; i < n; i++) s = (s ^ i) | 0; return s; }
             a(1); b(1); c(1); d(1);",
        )
        .is_ok()
    );

    // A single poll dispatches up to `workers` attempts at once.
    jit.poll();
    let m = jit.metrics();
    assert!(
        m.functions_compiling > 1,
        "the backlog was not spread over the workers ({m:?})"
    );

    settle(&jit);
    let m = jit.metrics();
    assert!(
        m.functions_compiled >= 4,
        "not every function compiled ({m:?})"
    );
    assert_eq!(m.queue_len, 0, "the queue did not drain ({m:?})");
}

/// The tier-policy knob still applies: `Interpreter` must compile nothing even
/// with the background feature on.
#[test]
fn interpreter_policy_still_compiles_nothing() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager().tier_policy(JitTierPolicy::Interpreter).build()).unwrap();

    assert!(
        eval_src(
            &mut ctx,
            &EvalName::Name("bg-interp.js".into()),
            "function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(1000);",
        )
        .is_ok()
    );
    settle(&jit);
    let m = jit.metrics();
    assert_eq!(
        (m.functions_queued, m.functions_compiled, m.native_entries),
        (0, 0, 0),
        "the interpreter policy reached the compiler ({m:?})"
    );
}
