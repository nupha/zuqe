// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! D-6 route B: the monomorphic call.
//!
//! The interpreter records the callee at every call site (the *call-site
//! feedback*). A site that only ever saw one callee is *monomorphic*: the
//! compiler then emits the direct-call helper for it, which enters that
//! callee's compiled code without the interpreter's call protocol (no
//! `call_internal`, no trampoline, no interpreter frame prologue).
//!
//! The guard is the recorded callee: a call whose callee is not it, or whose
//! callee the direct path cannot serve (a generator, a cross-realm callee, one
//! that allocates GC nodes), falls back to the general call path. So every
//! case below must agree with the interpreter, bit for bit, in all four modes.

#![cfg(feature = "jit")]

use crate::common::{MODES, Mode, fmt_value, settle};

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitMetrics, Runtime,
    ScopedContext,
};

fn eval(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

fn describe(ctx: &mut Context, res: JSResult) -> String {
    match res {
        Ok(v) => fmt_value(ctx, v),
        Err(code) => {
            let text = ctx.take_exception_as_text().message;
            format!("throw({code}):{text}")
        }
    }
}

/// The policy's config, with the automatic poll off.
///
/// The call-site feedback is a *snapshot* of what the interpreter had seen
/// when the function was compiled. With the automatic poll on, the compile
/// lands at the first frame boundary — i.e. after the setup's *first* call —
/// and the site's callee would be whatever that one call saw. With the poll
/// off, the compile happens at the explicit `settle` after the whole setup
/// script, so the recorded sites are the ones the setup produced.
fn config(mode: Mode) -> JitConfig {
    JitConfig::builder()
        .call_threshold(1)
        .loop_threshold(1)
        .auto_poll(false)
        .tier_policy(mode.policy())
        .build()
}

/// Run `setup` (which defines and warms the functions), then `main`, and
/// report the result and the metrics.
fn run(mode: Mode, setup: &str, main: &str) -> (String, JitMetrics) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(mode)).expect("attach");
    let name = EvalName::Name("jit-inline-call.js".into());

    eval(&mut ctx, &name, setup).expect("setup");
    settle(&jit);
    let res = eval(&mut ctx, &name, main);
    let result = describe(&mut ctx, res);
    settle(&jit);

    (result, jit.metrics())
}

/// A monomorphic call site: `caller` always calls `callee`, and both compile.
/// The direct path must be taken, and the result must match the interpreter.
#[test]
fn a_monomorphic_site_takes_the_direct_path() {
    let setup = "\
        function callee(n){ var t = 0; return n * 2 + t; } \
        function caller(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + callee(i); } return s; } \
        caller(8);";
    let main = "caller(50)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(
        result, "int:2450",
        "the direct path must not change the result"
    );
    assert!(
        metrics.inline_call_hits > 0,
        "the monomorphic site never took the direct path: {metrics:?}"
    );
    assert_eq!(
        metrics.inline_call_misses, 0,
        "the guard never missed on a monomorphic site: {metrics:?}"
    );
}

/// The result must be identical in every mode, and the direct path must not
/// change what the four modes compute.
#[test]
fn every_mode_agrees_with_the_interpreter() {
    let setup = "\
        function callee(n){ var t = 0; return n * 2 + t; } \
        function caller(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + callee(i); } return s; } \
        caller(8);";
    let main = "caller(50)";

    let mut results = Vec::new();
    for mode in MODES {
        let (result, _) = run(mode, setup, main);
        results.push((mode.label(), result));
    }
    let (_, first) = &results[0];
    for (label, result) in &results {
        assert_eq!(result, first, "{label} disagrees with the interpreter");
    }
}

/// A *polymorphic* site (the same call instruction, two different callees)
/// must not take the direct path: the guard would miss every time. The result
/// must still be right.
#[test]
fn a_polymorphic_site_falls_back() {
    let setup = "\
        function a(n){ var t = 0; return n + 1 + t; } \
        function b(n){ var t = 0; return n + 2 + t; } \
        function caller(which, n){ var f = which ? a : b; return f(n); } \
        caller(1, 8); caller(0, 8);";
    let main = "caller(1, 20) + ':' + caller(0, 20)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:21:22");
    assert_eq!(
        metrics.inline_call_hits, 0,
        "a polymorphic site must not take the direct path: {metrics:?}"
    );
}

/// A site that was monomorphic when the caller compiled, but calls a
/// different function later: the guard misses and the general path serves the
/// call. The result must still be right.
#[test]
fn a_guard_miss_falls_back() {
    let setup = "\
        function a(n){ var t = 0; return n + 1 + t; } \
        function b(n){ var t = 0; return n + 2 + t; } \
        function caller(f, n){ return f(n); } \
        caller(a, 1);";
    let main = "caller(a, 5) + ':' + caller(b, 5)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:6:7");
    assert!(
        metrics.inline_call_hits > 0,
        "the recorded callee never took the direct path: {metrics:?}"
    );
    assert!(
        metrics.inline_call_misses > 0,
        "the changed callee never missed the guard: {metrics:?}"
    );
}

/// The direct path must carry an exception out of the callee into the
/// caller's `catch`, with the callee's frame in the backtrace.
#[test]
fn an_exception_in_the_callee_reaches_the_callers_catch() {
    let setup = "\
        function boom(n){ var t = 0; if (n > 2) { throw new Error('boom'); } return n + t; } \
        function caller(n){ try { return boom(n); } catch (e) { return 'caught:' + e.message; } } \
        caller(1);";
    let main = "caller(5)";

    let (result, _) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:caught:boom");
}

/// A callee that allocates GC nodes needs a scope of its own; the direct path
/// gives it one and promotes the return value, exactly as the trampoline
/// does. The result must be right.
#[test]
fn a_callee_that_allocates_agrees() {
    let setup = "\
        function alloc(n){ var o = { v: n }; return o.v + 1; } \
        function caller(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + alloc(i); } return s; } \
        caller(8);";
    let main = "caller(50)";

    let (result, _) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "int:1275");
}

/// The callee's frame must be in the error's backtrace: the direct path builds
/// it before the frame goes away.
#[test]
fn the_callees_frame_is_in_the_backtrace() {
    let setup = "\
        function boom(){ throw new Error('deep'); } \
        function caller(go){ if (go) { return boom(); } return 0; } \
        caller(0);";
    let main = "try { caller(1); } catch (e) { e.stack; }";

    let (result, _) = run(Mode::Tier1, setup, main);
    assert!(
        result.contains("boom"),
        "the backtrace must name the callee: {result}"
    );
}

/// A callee that hits a temporal dead zone reads its binding through a guard
/// that hands the op back to the interpreter — the direct path must resume
/// that callee there, and the error must be the interpreter's.
#[test]
fn a_callee_that_needs_the_interpreter_resumes_there() {
    let setup = "\
        function tdz(n){ if (n > 0) { let a = n; return a; } { b; let b = 1; return b; } } \
        function caller(n){ try { return 'v:' + tdz(n); } catch (e) { return 'e:' + e.name; } } \
        caller(1);";
    let main = "caller(0)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:e:ReferenceError");
    assert!(
        metrics.inline_call_hits > 0,
        "the direct path was not taken: {metrics:?}"
    );
}
