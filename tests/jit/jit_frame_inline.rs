// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! D-6 route B, step 2: the callee's body is copied into the caller.
//!
//! A monomorphic site whose callee is a pure, forward-only body runs that body
//! inline: no arena frame, no second activation. Operands outside the inline
//! subset (strings, floats, missing arguments, …) jump to the general call
//! path, which re-runs the callee from scratch, so every case below must agree
//! with the interpreter bit for bit in all four modes.

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

fn config(mode: Mode) -> JitConfig {
    JitConfig::builder()
        .call_threshold(1)
        .loop_threshold(1)
        .auto_poll(false)
        .tier_policy(mode.policy())
        .build()
}

fn run(mode: Mode, setup: &str, main: &str) -> (String, JitMetrics) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(mode)).expect("attach");
    let name = EvalName::Name("jit-frame-inline.js".into());

    eval(&mut ctx, &name, setup).expect("setup");
    settle(&jit);
    let res = eval(&mut ctx, &name, main);
    let result = describe(&mut ctx, res);
    settle(&jit);

    (result, jit.metrics())
}

/// Run `setup`/`main` in the interpreter and tier 1 and require the same
/// result, plus evidence that the callee's body was inlined.
fn agree_inlined(label: &str, setup: &str, main: &str) {
    let (expected, _) = run(Mode::Interpreter, setup, main);
    for mode in MODES {
        let (actual, metrics) = run(mode, setup, main);
        assert_eq!(
            actual,
            expected,
            "{label} in {} disagrees with the interpreter",
            mode.label()
        );
        if mode != Mode::Interpreter {
            assert!(
                metrics.inline_body_hits > 0,
                "{label} was not inlined in {}: {metrics:?}",
                mode.label()
            );
        }
    }
}

/// The same agreement check for a callee that must stay on the direct path:
/// no inline guard may pass.
fn agree_without_inlining(label: &str, setup: &str, main: &str) {
    let (expected, _) = run(Mode::Interpreter, setup, main);
    for mode in MODES {
        let (actual, metrics) = run(mode, setup, main);
        assert_eq!(
            actual,
            expected,
            "{label} in {} disagrees with the interpreter",
            mode.label()
        );
        assert_eq!(
            metrics.inline_body_hits,
            0,
            "{label} must not be inlined in {}: {metrics:?}",
            mode.label()
        );
    }
}

/// A pure callee is copied into the caller: the guard passes, and the result
/// is the interpreter's.
#[test]
fn a_pure_monomorphic_callee_is_inlined() {
    let setup = "\
        function callee(n){ var t = 0; return n * 2 + t; } \
        function caller(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + callee(i); } return s; } \
        caller(8);";
    let main = "caller(50)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "int:2450");
    assert!(
        metrics.inline_body_hits > 0,
        "the inline guard never passed: {metrics:?}"
    );
    assert_eq!(
        metrics.mono_call_misses, 0,
        "the guard never missed on a monomorphic site: {metrics:?}"
    );
}

/// All four modes must agree, inline path or not.
#[test]
fn every_mode_agrees_with_the_interpreter() {
    let setup = "\
        function callee(n){ var t = 1; return n * 3 - t; } \
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

/// A forward branch is inside the subset: both `return` sites feed the same
/// result slot.
#[test]
fn a_forward_branch_is_inlined() {
    let setup = "\
        function abs(n){ if (n < 0) return -n; return n; } \
        function caller(n){ var s = 0; for (var i = -n; i < n; i++) { s = s + abs(i); } return s; } \
        caller(4);";
    let main = "caller(20)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    // sum of |i| for i in [-20, 20)
    assert_eq!(result, "int:400");
    assert!(
        metrics.inline_body_hits > 0,
        "the branch-shaped callee was not inlined: {metrics:?}"
    );
}

/// T7.1: a loop inside the copied body runs inline; its back edge polls the
/// caller frame's safepoint and bails if the poll asks to stop.
#[test]
fn a_loop_inside_the_callee_is_inlined() {
    let setup = "\
        function sum(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + i; } return s; } \
        function caller(n){ var t = sum(n); return t * 2; } \
        caller(4);";
    let main = "caller(50)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "int:2450");
    assert!(
        metrics.inline_body_hits > 0,
        "the loop-shaped callee was not inlined: {metrics:?}"
    );
    assert_eq!(
        metrics.mono_call_misses, 0,
        "the guard never missed on a monomorphic site: {metrics:?}"
    );
}

/// A collection requested by the host runs at the inlined loop's back edge,
/// with the caller frame (and its call window) as the only roots. The WeakRef
/// proves the argument survived the collection.
#[test]
fn a_gc_at_an_inlined_backedge_keeps_the_frame_valid() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(Mode::Tier1)).expect("attach");
    let name = EvalName::Name("jit-inline-gc.js".into());

    eval(
        &mut ctx,
        &name,
        "function sum(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + i; } return s; } \
         function caller(o, n){ \
             var wr = new WeakRef(o); \
             var s = sum(n); \
             if (wr.deref() !== o) throw new Error('collected'); \
             return o.v + s; \
         } \
         caller({ v: 1 }, 4);",
    )
    .expect("setup");
    settle(&jit);

    // The next safepoint inside the inlined loop collects.
    ctx.rt_mut().gc_request();
    let res = eval(&mut ctx, &name, "caller({ v: 5 }, 10)");
    assert_eq!(describe(&mut ctx, res), "int:50");
    assert!(
        jit.metrics().inline_body_hits > 0,
        "the loop was not inlined: {:?}",
        jit.metrics()
    );
}

/// T7.2: a call inside the copied body is copied too, when its own site is
/// monomorphic. `mid` is inlined into `caller`, and `leaf` into `mid`.
#[test]
fn a_nested_call_is_inlined() {
    let setup = "\
        function leaf(x){ return x + 1; } \
        function mid(f, x){ return f(x) * 2; } \
        function caller(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + mid(leaf, i); } return s; } \
        caller(4);";
    let main = "caller(20)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    // sum over i in [0, 20): 2 * (i + 1)
    assert_eq!(result, "int:420");
    assert!(
        metrics.inline_body_hits > 0,
        "the outer call chain was not inlined: {metrics:?}"
    );
    assert!(
        metrics.nested_inline_hits > 0,
        "the nested call was not inlined: {metrics:?}"
    );
}

/// T7.2 copies a three-body chain (the snapshot depth bound is two nested
/// levels): caller -> a -> b -> c.
#[test]
fn a_three_body_call_chain_is_inlined() {
    let setup = "\
        function c(x){ return x + 1; } \
        function b(f, x){ return f(x) + 1; } \
        function a(f, g, x){ return f(g, x) + 1; } \
        function caller(n){ return a(b, c, n); } \
        caller(3);";
    let main = "caller(5)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "int:8");
    assert!(
        metrics.nested_inline_hits >= 2,
        "the chain did not inline to its depth bound: {metrics:?}"
    );
}

/// T7.3: a `try/catch` body is copied. The non-throwing path runs inline; a
/// `throw` bails to the general path, which owns the catch scan — so a handler
/// inside the copied body still wins exactly as it would.
#[test]
fn a_try_catch_body_is_inlined() {
    let setup = "\
        function f(x){ try { if (x) throw 1; return 7; } catch (e) { return 9; } } \
        function caller(x){ return f(x); } \
        caller(0);";
    let main = "caller(0) + ':' + caller(1)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:7:9");
    assert!(
        metrics.inline_body_hits > 0,
        "the try/catch body was not inlined: {metrics:?}"
    );
}

/// A throw with no handler in the copied body propagates through the caller's
/// own `catch`, exactly like the interpreter.
#[test]
fn a_throw_through_an_inlined_body_is_caught_by_the_caller() {
    let setup = "\
        function f(x){ if (x) throw 1; return 1; } \
        function caller(x){ try { return f(x); } catch (e) { return 'caught:' + e; } } \
        caller(0);";
    let main = "caller(0) + ':' + caller(1)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:1:caught:1");
    assert!(
        metrics.inline_body_hits > 0,
        "the throwing body was not inlined: {metrics:?}"
    );
}

/// T7.4: an empty object literal is allocated inside the copied body and
/// returned. Two calls must produce two distinct objects.
#[test]
fn an_allocation_inside_the_callee_is_inlined() {
    let setup = "\
        function make(){ return {}; } \
        function caller(){ var a = make(); var b = make(); return a === b; } \
        caller();";
    let main = "caller()";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "bool:false");
    assert!(
        metrics.inline_body_hits > 0,
        "the allocating body was not inlined: {metrics:?}"
    );
}

/// The empty-array literal uses `array_from 0`; the same scratch path.
#[test]
fn an_empty_array_literal_is_inlined() {
    let setup = "\
        function make(){ return []; } \
        function caller(){ var a = make(); var b = make(); return Array.isArray(a) && a !== b; } \
        caller();";
    let main = "caller()";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "bool:true");
    assert!(
        metrics.inline_body_hits > 0,
        "the array-allocating body was not inlined: {metrics:?}"
    );
}

/// A body that both allocates and loops is not inlined: the back edge's
/// safepoint could collect an object that only lives in a register.
#[test]
fn an_allocation_plus_a_loop_stays_on_the_direct_path() {
    let setup = "\
        function f(n){ var o = {}; for (var i = 0; i < n; i++) { } return o; } \
        function caller(n){ return f(n); } \
        caller(1);";
    let main = "typeof caller(3)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:object");
    assert_eq!(
        metrics.inline_body_hits, 0,
        "allocation + safepoint must not inline: {metrics:?}"
    );
}

/// A host-requested collection runs at the caller's next back edge, after the
/// inlined allocation returned its object; the object must survive.
#[test]
fn a_gc_after_an_inlined_allocation_keeps_the_object() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config(Mode::Tier1)).expect("attach");
    let name = EvalName::Name("jit-inline-alloc-gc.js".into());

    eval(
        &mut ctx,
        &name,
        "function make(){ return {}; } \
         function caller(n){ \
             var o = make(); \
             var wr = new WeakRef(o); \
             for (var i = 0; i < n; i++) { } \
             return wr.deref() === o; \
         } \
         caller(2);",
    )
    .expect("setup");
    settle(&jit);

    ctx.rt_mut().gc_request();
    let res = eval(&mut ctx, &name, "caller(4)");
    assert_eq!(describe(&mut ctx, res), "bool:true");
    assert!(
        jit.metrics().inline_body_hits > 0,
        "the allocating body was not inlined: {:?}",
        jit.metrics()
    );
}

/// The nested guard misses when the copied body calls a different function;
/// the whole call bails to the general path and still agrees.
#[test]
fn a_nested_guard_miss_falls_back() {
    let setup = "\
        function leaf(x){ return x + 1; } \
        function other(x){ return x + 2; } \
        function mid(f, x){ return f(x) * 10; } \
        function caller(f, n){ return mid(f, n); } \
        caller(leaf, 1);";
    let main = "caller(leaf, 3) + ':' + caller(other, 3)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:40:50");
    assert!(
        metrics.inline_body_hits > 0,
        "the monomorphic outer site was not inlined: {metrics:?}"
    );
    assert!(
        metrics.nested_inline_hits > 0,
        "the recorded nested callee never hit: {metrics:?}"
    );
}

/// A non-`Int` operand bails to the general path; the frame still holds the
/// original call window, so the callee simply runs there. Both shapes must be
/// right at the same monomorphic site.
#[test]
fn a_non_int_operand_bails_to_the_general_path() {
    let setup = "\
        function add(a, b){ return a + b; } \
        function caller(a, b){ return add(a, b); } \
        caller(1, 2);";
    let main = "caller(1, 2) + ':' + caller('a', 'b')";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:3:ab");
    assert!(
        metrics.inline_body_hits > 0,
        "the monomorphic site never passed the inline guard: {metrics:?}"
    );
}

/// A missing argument is `undefined`, outside the integer path: the general
/// path must produce the interpreter's `NaN`.
#[test]
fn a_missing_argument_bails() {
    let setup = "\
        function sub(a, b){ return a - b; } \
        function caller(a){ return sub(a); } \
        caller(5);";
    let main = "String(caller(5))";

    let (result, _) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:NaN");
}

/// `-0` is a Float64: the inline integer path must bail rather than return an
/// `Int` 0, and `0 * -1` must not lose the sign either.
#[test]
fn negative_zero_bails() {
    let setup = "\
        function neg(n){ return -n; } \
        function mul(a, b){ return a * b; } \
        function caller(a, b){ return (1 / neg(a)) + ':' + (1 / mul(a, b)); } \
        caller(1, 2);";
    let main = "caller(0, -1)";

    let (result, _) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:-Infinity:-Infinity");
}

/// An `Int` overflow leaves the i32 domain; the general path owns the
/// `Float64` result.
#[test]
fn an_int_overflow_bails() {
    let setup = "\
        function add(a, b){ return a + b; } \
        function caller(a, b){ return add(a, b); } \
        caller(1, 2);";
    let main = "caller(2147483647, 1)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "f64:2147483648.0");
    assert!(
        metrics.inline_body_hits > 0,
        "the inline guard was not reached: {metrics:?}"
    );
}

/// A string condition poisons the body: the general path runs the callee, and
/// a string `+` keeps its concatenation semantics.
#[test]
fn strings_bail_to_the_general_path() {
    let setup = "\
        function pick(v){ if (v) return 1; return 0; } \
        function cat(a, b){ return a + b; } \
        function caller(v){ return pick(v) + ':' + cat(v, '!'); } \
        caller(1);";
    let main = "caller('') + '|' + caller('x')";

    let (result, _) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:0:!|1:x!");
}

/// Two inlined callees in the same function must not share Cranelift variable
/// ids (the builder forbids declaring a variable twice).
#[test]
fn two_inline_sites_get_separate_variable_blocks() {
    let setup = "\
        function a(x){ return x + 1; } \
        function b(x){ return x + 2; } \
        function caller(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + a(i) + b(i); } return s; } \
        caller(4);";
    let main = "caller(20)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    // sum over i in [0, 20): (i + 1) + (i + 2)
    assert_eq!(result, "int:440");
    assert!(
        metrics.inline_body_hits >= 2,
        "both sites must pass the inline guard: {metrics:?}"
    );
}

/// A local written on one branch and read after the join must still be the
/// right value inline.
#[test]
fn a_local_crosses_a_forward_branch() {
    let setup = "\
        function f(n){ var x = 0; if (n > 0) { x = 2; } return x + n; } \
        function caller(n){ var s = 0; for (var i = -n; i < n; i++) { s = s + f(i); } return s; } \
        caller(4);";
    let main = "caller(20)";

    let (result, _) = run(Mode::Tier1, setup, main);
    // sum over i in [-20, 20): i <= 0 -> i, i > 0 -> i + 2
    assert_eq!(result, "int:18");
}

/// `call_method` sites carry the receiver below the callee; the inline path
/// must find the arguments after it and leave the result in the receiver's
/// slot.
#[test]
fn a_pure_method_is_inlined() {
    let setup = "\
        var o = { m: function (n){ var t = 2; return n + t; } }; \
        function caller(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + o.m(i); } return s; } \
        caller(8);";
    let main = "caller(40)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "int:860");
    assert!(
        metrics.inline_body_hits > 0,
        "the method site never passed the inline guard: {metrics:?}"
    );
}

/// If the callee changes after the caller compiled, the inline guard misses
/// and the general path runs the new callee.
#[test]
fn a_guard_miss_falls_back() {
    let setup = "\
        function a(n){ return n + 1; } \
        function b(n){ return n + 2; } \
        function caller(f, n){ return f(n); } \
        caller(a, 1);";
    let main = "caller(a, 5) + ':' + caller(b, 5)";

    let (result, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(result, "str:6:7");
    assert!(
        metrics.inline_body_hits > 0,
        "the recorded callee was never inlined: {metrics:?}"
    );
    assert!(
        metrics.mono_call_misses > 0,
        "the changed callee never missed the guard: {metrics:?}"
    );
}

// ── The whole inline subset ──

/// Every binary/unary operator the inline lowering implements. The table is
/// the coverage contract: an opcode added to `inline_call::supported` should
/// get a row here, and a row that stops being inlined fails the test.
#[test]
fn every_inline_operator_agrees() {
    for (name, expression) in [
        ("add", "a + b"),
        ("sub", "a - b"),
        ("mul", "a * b"),
        ("lt", "a < b"),
        ("lte", "a <= b"),
        ("gt", "a > b"),
        ("gte", "a >= b"),
        ("eq", "a == b"),
        ("neq", "a != b"),
        ("strict_eq", "a === b"),
        ("strict_neq", "a !== b"),
        ("and", "a & b"),
        ("or", "a | b"),
        ("xor", "a ^ b"),
        ("shl", "a << b"),
        ("sar", "a >> b"),
        ("shr", "a >>> b"),
    ] {
        let setup = format!(
            "function f(a, b){{ return {expression}; }} \
             function caller(a, b){{ return f(a, b); }} caller(2, 3);"
        );
        agree_inlined(name, &setup, "caller(7, 3)");
    }

    for (name, expression) in [
        ("plus", "+a"),
        ("neg", "-a"),
        ("inc", "++a"),
        ("dec", "--a"),
        ("lnot", "!a"),
        ("not", "~a"),
    ] {
        let setup = format!(
            "function f(a){{ return {expression}; }} \
             function caller(a){{ return f(a); }} caller(2);"
        );
        agree_inlined(name, &setup, "caller(7)");
    }
}

/// Constants, locals, stack shuffles and both return forms. Each case also
/// warms the same monomorphic site from `setup`.
#[test]
fn every_inline_shape_agrees() {
    let cases = [
        (
            "push_i32",
            "function f(){ return 5; } function caller(){ return f(); } caller();",
            "caller()",
        ),
        (
            "undefined",
            "function f(){ return void 0; } function caller(){ return String(f()); } caller();",
            "caller()",
        ),
        (
            "null",
            "function f(){ return null; } function caller(){ return String(f()); } caller();",
            "caller()",
        ),
        (
            "true",
            "function f(){ return true; } function caller(){ return String(f()); } caller();",
            "caller()",
        ),
        (
            "false",
            "function f(){ return false; } function caller(){ return String(f()); } caller();",
            "caller()",
        ),
        (
            "return_undef",
            "function f(a){ if (a) return; return 1; } function caller(a){ return String(f(a)); } caller(1);",
            "caller(1)",
        ),
        (
            "nop",
            "function f(a){ ; return a; } function caller(a){ return f(a); } caller(1);",
            "caller(2)",
        ),
        (
            "drop",
            "function f(a){ return (a, 1); } function caller(a){ return f(a); } caller(1);",
            "caller(2)",
        ),
        (
            "dup / get_loc / put_loc",
            "function f(a){ var x = a, y = a; return x + y; } function caller(a){ return f(a); } caller(1);",
            "caller(2)",
        ),
        (
            "set_loc",
            "function f(a){ var x = 0; x = a; return x; } function caller(a){ return f(a); } caller(1);",
            "caller(2)",
        ),
        (
            "put_arg",
            "function f(a){ a = a + 1; return a; } function caller(a){ return f(a); } caller(1);",
            "caller(2)",
        ),
        (
            "no args",
            "function f(){ return 7; } function caller(){ return f(1, 2, 3); } caller();",
            "caller()",
        ),
        (
            "extra args",
            "function f(a){ return a; } function caller(){ return f(5, 6); } caller();",
            "caller()",
        ),
        (
            "return object",
            "var box = { v: 7 }; function f(x){ return x; } function caller(){ return f(box).v; } caller();",
            "caller()",
        ),
        (
            "return string",
            "function f(x){ return x; } function caller(){ return f('ok'); } caller();",
            "caller()",
        ),
        (
            "return float",
            "function f(x){ return x; } function caller(){ return f(1.5); } caller();",
            "caller()",
        ),
        (
            "forward goto",
            "function f(a){ if (a) { return 1; } else { return 2; } } function caller(a){ return f(a); } caller(1);",
            "caller(0)",
        ),
    ];
    for (label, setup, main) in cases {
        agree_inlined(label, setup, main);
    }
}

/// Operand types outside the integer path must fall back, and the fallback
/// result must still be the interpreter's.
#[test]
fn every_fallback_operand_agrees() {
    for (label, arguments) in [
        ("float", "1.5, 2"),
        ("string", "'a', 'b'"),
        ("bool", "true, true"),
        ("null", "null, 1"),
        ("undefined", "undefined, 1"),
        ("bigint", "1n, 2n"),
    ] {
        let setup = "\
            function add(a, b){ return a + b; } \
            function caller(a, b){ return add(a, b); } caller(1, 2);";
        let main = format!("String(caller({arguments}))");
        agree_inlined(label, setup, &main);
    }
}

/// A poisoned operand must not run the callee twice: `valueOf` is observed
/// exactly once (the general path), not once inline and again on the rerun.
#[test]
fn a_poisoned_operand_runs_user_code_once() {
    let setup = "\
        var calls = 0; \
        var boxed = { valueOf: function (){ calls = calls + 1; return 3; } }; \
        function add(a, b){ return a + b; } \
        function caller(a, b){ return add(a, b); } \
        caller(1, 2);";
    let main = "caller(boxed, 1) + ':' + calls";

    let (expected, _) = run(Mode::Interpreter, setup, main);
    let (actual, metrics) = run(Mode::Tier1, setup, main);
    assert_eq!(actual, expected);
    assert_eq!(actual, "str:4:1", "`valueOf` ran more than once");
    assert!(
        metrics.inline_body_hits > 0,
        "the monomorphic site never reached the inline guard: {metrics:?}"
    );
}

/// Every shape outside the subset must stay on the direct path (or the general
/// path) and still compute the interpreter's result.
#[test]
fn every_ineligible_shape_stays_on_the_direct_path() {
    let cases = [
        (
            "callee with a call",
            "function g(o){ return o.v; } function f(o){ return g(o); } \
             function caller(o){ return f(o); } caller({ v: 1 });",
            "caller({ v: 2 })",
        ),
        (
            "callee catch with a global",
            "var g = 1; function f(x){ try { if (x) throw 1; return 7; } catch (e) { return 9 + g; } } \
             function caller(x){ return f(x); } caller(0);",
            "caller(0) + ':' + caller(1)",
        ),
        (
            "try/finally",
            "function f(x){ var y = x; try { return y + 1; } finally { y = 0; } } \
             function caller(x){ return f(x); } caller(1);",
            "caller(2)",
        ),
        (
            "recursive callee",
            "function rec(f, n){ if (n < 2) return n; return f(f, n - 1) + f(f, n - 2); } \
             function caller(n){ return rec(rec, n); } caller(3);",
            "caller(6)",
        ),
        (
            "this",
            "var o = { v: 2, m: function (x){ return this.v + x; } }; \
             function caller(x){ return o.m(x); } caller(1);",
            "caller(2)",
        ),
        (
            "closure",
            "function make(){ var c = 1; return function (x){ return x + c; }; } \
             var c = make(); function caller(x){ return c(x); } caller(1);",
            "caller(2)",
        ),
        (
            "global",
            "var gv = 3; function f(x){ return x + gv; } \
             function caller(x){ return f(x); } caller(1);",
            "caller(2)",
        ),
        (
            "constant pool",
            "function f(x){ return x + 10000000000; } \
             function caller(x){ return f(x); } caller(1);",
            "caller(2)",
        ),
        (
            "TDZ",
            "function f(n){ if (n > 0) { let a = n; return a; } { b; let b = 1; return b; } } \
             function caller(n){ try { return 'v:' + f(n); } catch (e) { return 'e:' + e.name; } } \
             caller(1);",
            "caller(0)",
        ),
        (
            "default parameter",
            "function f(a = 1){ return a; } function caller(x){ return f(x); } caller(2);",
            "caller(3)",
        ),
        (
            "rest parameter",
            "function f(...a){ return a.length; } function caller(){ return f(1, 2); } caller();",
            "caller()",
        ),
        (
            "constructor",
            "function C(x){ this.x = x; } function caller(){ return new C(1).x; } caller();",
            "caller()",
        ),
        (
            "apply",
            "function f(x){ return x + 1; } function caller(x){ return f.apply(null, [x]); } caller(1);",
            "caller(2)",
        ),
        (
            "generator",
            "function* gen(){ yield 1; } function caller(){ return gen().next().value; } caller();",
            "caller()",
        ),
    ];
    for (label, setup, main) in cases {
        agree_without_inlining(label, setup, main);
    }
}

/// Sign/edge cases where the engine's boxing differs from plain `i32`:
/// binary logic ops box as `u32`, shifts mask the count, and unary `~` boxes
/// as `i32`.
#[test]
fn bitwise_boundaries_agree() {
    for (label, setup, main) in [
        (
            "or with a negative operand",
            "function f(a, b){ return a | b; } function caller(a, b){ return f(a, b); } caller(1, 2);",
            "caller(-1, 0)",
        ),
        (
            "unsigned shift of a negative value",
            "function f(a, b){ return a >>> b; } function caller(a, b){ return f(a, b); } caller(1, 2);",
            "caller(-1, 0)",
        ),
        (
            "shift count is masked",
            "function f(a, b){ return a << b; } function caller(a, b){ return f(a, b); } caller(1, 2);",
            "caller(1, 40)",
        ),
        (
            "arithmetic shift of a negative value",
            "function f(a, b){ return a >> b; } function caller(a, b){ return f(a, b); } caller(1, 2);",
            "caller(-8, 1)",
        ),
        (
            "unary not of a negative value",
            "function f(a){ return ~a; } function caller(a){ return f(a); } caller(1);",
            "caller(-1)",
        ),
    ] {
        agree_inlined(label, setup, main);
    }
}

/// `++` / unary `-` at the i32 boundary leave the integer domain and must
/// fall back, exactly like `+` overflow.
#[test]
fn unary_overflow_bails() {
    for (label, setup, main) in [
        (
            "increment at i32::MAX",
            "function f(a){ return ++a; } function caller(a){ return f(a); } caller(1);",
            "caller(2147483647)",
        ),
        (
            "decrement at i32::MIN",
            "function f(a){ return --a; } function caller(a){ return f(a); } caller(1);",
            "caller(-2147483648)",
        ),
        (
            "negate i32::MIN",
            "function f(a){ return -a; } function caller(a){ return f(a); } caller(1);",
            "caller(-2147483648)",
        ),
    ] {
        agree_inlined(label, setup, main);
    }
}

/// Truthiness outside the four simple tags (`Int`/`Bool`/`Null`/`Undefined`)
/// must fall back; both the result and the side-effect count must agree.
#[test]
fn truthiness_fallbacks_agree() {
    for value in ["0.0", "NaN", "''", "'x'", "({})", "0n", "1n"] {
        let setup = "\
            function pick(v){ if (v) return 1; return 0; } \
            function caller(v){ return pick(v); } caller(1);";
        let main = format!("caller({value})");
        agree_inlined(value, setup, &main);
    }
}

/// Comparisons on non-`Int` operands exercise the shared poison path too.
#[test]
fn comparison_fallbacks_agree() {
    for (label, arguments) in [
        ("strings", "'a', 'b'"),
        ("floats", "1.5, 1"),
        ("bool", "true, false"),
        ("null and undefined", "null, undefined"),
    ] {
        let setup = "\
            function less(a, b){ return a < b; } \
            function caller(a, b){ return less(a, b); } caller(1, 2);";
        let main = format!("caller({arguments})");
        agree_inlined(label, setup, &main);
    }
}
