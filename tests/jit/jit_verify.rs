// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Epic B acceptance gate (M0b): run the frontend over the repository's JS
//! corpus, confirm every hot function is either verified or rejected (never
//! both, never dropped), and confirm attaching the JIT does not change any
//! observable result.

#![cfg(feature = "jit")]

use zuqe::{
    EvalFlags, EvalName, EvalType, JSValue, Jit, JitConfig, JitMetrics, Runtime, ScopedContext,
};

/// Files that benchmark or run indefinitely, or need a harness this test does
/// not provide.
const DENYLIST: &[&str] = &[
    "bench_del.js",
    "long-run.js",
    "mb.js",
    "microbench.js",
    "microbench.mjs",
    "performance_test.js",
];

/// A deterministic description of an eval outcome: comparing raw `JSValue`s
/// could compare GC addresses.
fn observe(result: Result<JSValue, i32>) -> String {
    match result {
        Ok(value) if value.is_undefined() => "undefined".into(),
        Ok(value) if value.is_null() => "null".into(),
        Ok(value) if value.is_int() => format!("int:{}", value.as_int().unwrap()),
        Ok(value) if value.is_bool() => format!("bool:{}", value.as_bool().unwrap()),
        Ok(_) => "other".into(),
        Err(code) => format!("err:{code}"),
    }
}

fn eval_once(source: &[u8], attach: Option<JitConfig>) -> (String, Option<JitMetrics>) {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = attach.map(|cfg| Jit::attach_to(&rt, cfg).unwrap());
    let name = EvalName::Name("jit-verify.js".into());
    let result = ctx.eval(source, &name, EvalType::Global, EvalFlags::empty());
    let observed = observe(result);
    let metrics = jit.as_ref().map(|jit| {
        // Outside `with_new_scope`: verify everything the run queued.
        jit.poll();
        jit.metrics()
    });
    (observed, metrics)
}

/// Small thresholds so every entered function (and any loop) is snapshot and
/// verified within one short run.
fn eager_config() -> JitConfig {
    JitConfig::builder()
        .call_threshold(1)
        .loop_threshold(1)
        .build()
}

fn assert_same(source: &[u8], metrics_out: &mut JitMetrics) -> bool {
    let (baseline, _) = eval_once(source, None);
    let (with_jit, metrics) = eval_once(source, Some(eager_config()));
    assert_eq!(baseline, with_jit, "JIT changed the result");
    let metrics = match metrics {
        Some(m) if m.functions_queued > 0 => m,
        _ => return false,
    };
    // Every queued snapshot is either verified or rejected exactly once.
    assert_eq!(
        metrics.functions_verified + metrics.functions_rejected,
        metrics.functions_queued,
        "a queued function was neither verified nor rejected"
    );
    assert_eq!(metrics.queue_len, 0, "queue not drained by poll");
    metrics_out.functions_queued += metrics.functions_queued;
    metrics_out.functions_verified += metrics.functions_verified;
    metrics_out.functions_rejected += metrics.functions_rejected;
    true
}

#[test]
fn repository_corpus_verifies_without_changing_results() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut total = JitMetrics::default();
    let mut files = 0usize;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("js" | "mjs")
            )
        })
        .collect();
    entries.sort();

    for path in entries {
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if DENYLIST.contains(&name.as_str()) {
            continue;
        }
        let Ok(source) = std::fs::read(&path) else {
            continue;
        };
        if assert_same(&source, &mut total) {
            files += 1;
        }
    }

    // The gate must observe a real distribution, not an empty queue.
    assert!(total.functions_queued > 0, "no function was ever queued");
    assert!(
        total.functions_verified > 0,
        "no function passed verification"
    );
    println!(
        "jit corpus: {files} files, queued={} verified={} rejected={}",
        total.functions_queued, total.functions_verified, total.functions_rejected
    );
}

#[test]
fn curated_corpus_covers_accept_and_reject_paths() {
    // Each snippet is a whole script; the trailing expression is the observed
    // value. Together they span the accepted opcode set (arithmetic, locals,
    // closures, properties, arrays, calls) and the reject set (for-of, class,
    // generators).
    let snippets: &[&str] = &[
        "var s = 0; for (var i = 0; i < 500; i++) s += i; s;",
        "function fib(n){ if (n < 2) return n; return fib(n-1) + fib(n-2); } fib(15);",
        "function add(a, b){ return a + b; } add(2, 3);",
        "var o = { a: 1, b: 2 }; var t = 0; for (var i = 0; i < 100; i++) t += o.a + o.b; t;",
        "var a = [1,2,3,4,5]; var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; s;",
        "function make(n){ return function(){ return n; }; } make(7)();",
        "var x = 0; while (x < 200) x = x + 2; x;",
        "var acc = 0; var s = 'abcdef'; for (var i = 0; i < s.length; i++) acc ^= s.charCodeAt(i); acc;",
        "function f(a){ return a * 2 - 1; } var r = 0; for (var i = 0; i < 300; i++) r = f(i); r;",
        // Reject paths: iterator, class machinery, generator.
        "var s = 0; for (var v of [1,2,3]) s += v; s;",
        "class C { constructor(){ this.x = 1; } m(){ return this.x; } } new C().m();",
        "function* g(){ yield 1; yield 2; } var it = g(); it.next().value;",
    ];

    let mut total = JitMetrics::default();
    for snippet in snippets {
        assert_same(snippet.as_bytes(), &mut total);
    }
    assert!(
        total.functions_verified > 0,
        "nothing was verified in the curated corpus"
    );
    assert!(
        total.functions_rejected > 0,
        "nothing was rejected in the curated corpus"
    );
}

/// Compile `define` on a JIT runtime, then run `call` natively and compare with
/// an interpreter-only run. Returns the JIT metrics after the native call.
fn native_vs_interpreter(define: &[u8], call: &str) -> JitMetrics {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-native.js".into());

    // Run once to get the function hot, then poll to compile and install it.
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    jit.poll();
    assert!(
        jit.metrics().functions_compiled > 0,
        "nothing was compiled for {}",
        call
    );

    // Now the call runs through the installed native entry.
    let native = ctx
        .eval(call.as_bytes(), &name, EvalType::Global, EvalFlags::empty())
        .unwrap();

    // Same call, fresh interpreter-only runtime.
    let mut base_rt = Runtime::create().unwrap();
    let mut base_ctx = base_rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    base_ctx
        .eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    let expected = base_ctx
        .eval(call.as_bytes(), &name, EvalType::Global, EvalFlags::empty())
        .unwrap();

    assert_eq!(
        observe(Ok(expected)),
        observe(Ok(native)),
        "native result differs for {call}"
    );
    let metrics = jit.metrics();
    assert!(
        metrics.native_entries > 0,
        "the native entry for {call} never ran"
    );
    metrics
}

#[test]
fn native_execution_matches_interpreter() {
    // Iterative fib: fully native (no calls). Ints, comparisons, loop.
    let fib = b"function fib(n){ var a = 0, b = 1; for (var i = 0; i < n; i++) { var t = a + b; a = b; b = t; } return b; } fib(1);";
    native_vs_interpreter(fib, "fib(30)");
    native_vs_interpreter(fib, "fib(50)");

    // Numeric kernels spanning the helper set: floats, bitwise, comparisons,
    // unary operators, modulo, typeof, booleans/null/undefined, logical ops.
    let kernels = b"\
        function flt(n){ var s = 0.5; for (var i = 0; i < n; i++) s = s * 1.5 + 0.25; return s; } \
        function bits(n){ var x = 0; for (var i = 0; i < n; i++) x = (x << 1) ^ (i & 7) | (i >>> 2); return x; } \
        function cmps(n){ var c = 0; for (var i = 0; i < n; i++){ if (i < n && i >= 0 && i != 3 && i == i) c = c + 1; } return c; } \
        function unary(n){ var x = 0; for (var i = 0; i < n; i++){ x = -i; x = +x; x = ~x; } return x; } \
        function logi(n){ var c = 0; for (var i = 0; i < n; i++){ if (!(i % 2)) c = c + 1; } return c; } \
        function tpat(n){ var s = 0; for (var i = 0; i < n; i++){ if (typeof i === 'number') s = s + 1; } return s; } \
        function bools(n){ var c = 0; for (var i = 0; i < n; i++){ if (i === 0) c = c + 1; if (null) c = c + 100; if (undefined) c = c + 100; if (true) c = c + 1; } return c; } \
        function shifts(n){ var x = 1; for (var i = 0; i < n; i++){ x = ((x >> 1) ^ (x << 2)) | (x & 255); } return x; } \
        function loose(n){ var c = 0; for (var i = 0; i < n; i++){ if (i == n) c = c + 1; if (i != 3) c = c + 1; } return c; } \
        function logical(n){ var c = 0; for (var i = 0; i < n; i++){ if ((i > 2 && i < n) || i == 0) c = c + 1; } return c; } \
        flt(1); bits(1); cmps(1); unary(1); logi(1); tpat(1); bools(1); shifts(1); loose(1); logical(1);";
    for kernel in [
        "flt", "bits", "cmps", "unary", "logi", "tpat", "bools", "shifts", "loose", "logical",
    ] {
        native_vs_interpreter(kernels, &format!("{kernel}(1000)"));
    }

    // Strings exercise the helpers' coercion slow paths.
    let coercions = b"\
        function cat(n){ var s = ''; for (var i = 0; i < n; i++) s = s + 'x'; return s.length; } \
        cat(1);";
    native_vs_interpreter(coercions, "cat(200)");

    // Recursion resumes to the interpreter at each call, but must still match.
    let rec = b"function fib(n){ if (n < 2) return n; return fib(n - 1) + fib(n - 2); } fib(1);";
    native_vs_interpreter(rec, "fib(20)");
}

/// Epic E1: the integer fast paths (two-int add/sub/mul/compare/bitwise) plus
/// their fallbacks, all returning an int so `observe` compares the value.
#[test]
fn integer_fast_paths_match_interpreter() {
    let define = b"\
        function ovf(n){ var s = 0; for (var i = 0; i < n; i++){ s = s + 2000000000; if (s > 2000000000) s = s - 3999999999; if (s < -2000000000) s = s + 3999999999; } return s | 0; } \
        function bigshift(n){ var x = 1; for (var i = 0; i < n; i++){ x = (x << 33) ^ (i >> 40); } return x; } \
        function quirks(n){ var x = 0; for (var i = 0; i < n; i++){ x = x + ((-1 | 0) >>> 31); x = x ^ (i & 0); } return x | 0; } \
        function cmps(n){ var c = 0; for (var i = -3; i < n; i++){ if (i < 0) c = c + 1; if (i >= -1 && i <= 2) c = c + 1; if (i == -2 || i != 0) c = c + 1; } return c; } \
        function mixed(n){ var s = 0; for (var i = 0; i < n; i++){ s = s + 7; s = 2147483647 + 1; } return s; } \
        ovf(1); bigshift(1); quirks(1); cmps(1); mixed(1);";
    for kernel in ["ovf", "bigshift", "quirks", "cmps", "mixed"] {
        native_vs_interpreter(define, &format!("{kernel}(500)"));
    }
}

/// Epic E3: global-variable and array-element helpers match the interpreter.
#[test]
fn globals_and_arrays_match_interpreter() {
    let define = b"\
        var G = 0; \
        function gv(n){ var s = 0; for (var i = 0; i < n; i++){ G = G + 1; s = s + G; } return s; } \
        function gundef(n){ var s = 0; for (var i = 0; i < n; i++){ if (typeof NOPE === 'undefined') s = s + 1; } return s; } \
        function ar(n){ var a = [10, 20, 30, 40]; var s = 0; for (var i = 0; i < n; i++){ s = s + a[i & 3]; } return s; } \
        function astore(n){ var a = [0, 0, 0, 0]; var s = 0; for (var i = 0; i < n; i++){ a[i & 3] = i; s = s + a[i & 3]; } return s; } \
        gv(1); gundef(1); ar(1); astore(1);";
    for kernel in ["gv", "gundef", "ar", "astore"] {
        native_vs_interpreter(define, &format!("{kernel}(500)"));
    }
}

/// Epic E2: property access (inline-cache guards) matches the interpreter.
/// Objects live in globals so the functions under test contain no object
/// literal (`define_field` is fail-closed).
#[test]
fn properties_match_interpreter() {
    let define = b"\
        var OBJ = { a: 1, b: 2, c: 3 }; \
        var MO = { m: function(){ return 5; } }; \
        function props(n){ var s = 0; for (var i = 0; i < n; i++){ s = s + OBJ.a + OBJ.b + OBJ.c; OBJ.a = s & 7; } return s; } \
        function methods(n){ var s = 0; for (var i = 0; i < n; i++){ s = s + MO.m(); } return s; } \
        function shapes(n){ var s = 0; for (var i = 0; i < n; i++){ s = s + OBJ.a; } return s; } \
        props(1); methods(1); shapes(1);";
    for kernel in ["props", "methods", "shapes"] {
        native_vs_interpreter(define, &format!("{kernel}(500)"));
    }
}

/// Epic E1 remainder: pow / in / instanceof / delete / post-inc / add_loc.
#[test]
fn misc_operators_match_interpreter() {
    let define = b"\
        var OBJ2 = { k: 1, m: function(){ return 1; } }; \
        function powk(n){ var s = 1; for (var i = 0; i < n; i++){ s = (s + (2 ** 3)) & 255; } return s; } \
        function ink(n){ var s = 0; for (var i = 0; i < n; i++){ if (i in OBJ2) s = s + 1; } return s; } \
        function instk(n){ var s = 0; for (var i = 0; i < n; i++){ if (OBJ2.m instanceof Object) s = s + 1; } return s; } \
        function delk(n){ var s = 0; for (var i = 0; i < n; i++){ delete OBJ2.missing; s = s + 1; } return s; } \
        function postk(n){ var x = 0; var s = 0; for (var i = 0; i < n; i++){ s = s + (x++); } return s; } \
        function addk(n){ var s = 0; for (var i = 0; i < n; i++){ s += 2; } return s; } \
        powk(1); ink(1); instk(1); delk(1); postk(1); addk(1);";
    for kernel in ["powk", "ink", "instk", "delk", "postk", "addk"] {
        native_vs_interpreter(define, &format!("{kernel}(500)"));
    }
}

/// Epic E4: a call exits to the trampoline, which runs the callee and then
/// re-enters the caller's native code at the post-call pc. Mutual recursion
/// stresses that re-entry (every return lands back in native code).
#[test]
fn calls_reenter_native() {
    let define = b"\
        function isEven(n){ if (n === 0) return true; return isOdd(n - 1); } \
        function isOdd(n){ if (n === 0) return false; return isEven(n - 1); } \
        isEven(1);";
    let mut total = 0u64;
    for arg in [20u32, 73, 200] {
        let metrics = native_vs_interpreter(define, &format!("isEven({arg}) ? 1 : 0"));
        total += metrics.native_entries;
    }
    assert!(total > 0, "no native entries");
}

/// Epic 1.7: a native (C/Rust) callee returning inline must not strand the
/// caller in the interpreter. Before this, a single `Math.max(...)` before a
/// hot loop left that loop interpreted for the rest of the frame (the loop
/// back-edge hook fired once per iteration); now the post-call pc re-enters
/// native.
#[test]
fn a_native_call_reenters_the_callers_native_code() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-reentry.js".into());
    let define = b"function workload(n){ var m = Math.max(1, 2); var s = 0; \
                   for (var i = 0; i < n; i++) s = s + i; return s + m; } workload(1);";
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    jit.poll();

    let before = jit.metrics();
    ctx.eval(
        b"workload(100000);",
        &name,
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();
    let after = jit.metrics();

    assert_eq!(
        after.backedges - before.backedges,
        0,
        "the loop after a native call ran in the interpreter"
    );
    assert!(
        after.native_entries - before.native_entries >= 2,
        "no native re-entry after the inline native call (natives: {})",
        after.native_entries - before.native_entries
    );
}

/// S3 (2.5): the ref-cell family (`make_loc_ref`, `get_ref_value`,
/// `put_ref_value`, …) lowers to helpers, so a destructuring loop runs entirely
/// in native code. Before this the assignment targets hit an instruction-level
/// `Resume`, which stranded the loop in the interpreter — measured here as
/// interpreter loop back edges, the same signal as epic 1.7.
#[test]
fn ref_cells_run_in_native_code() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-refcell.js".into());
    // `[a, b] = p` builds a ref object per local target (make_loc_ref) and
    // writes the element back through it (put_ref_value); `s += a + b` reads
    // the locals. `for (v of p)` is the same machinery for the loop target.
    let define = b"function f(n, p){ var s = 0; var a = 0, b = 0; \
                   for (var i = 0; i < n; i++){ [a, b] = p; s += a + b; } return s; } \
                   function g(n, p){ var s = 0; \
                   for (var i = 0; i < n; i++){ for (var v of p) s += v; } return s; } \
                   f(1, [1, 2]); g(1, [1, 2]);";
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    jit.poll();
    assert!(
        jit.metrics().functions_compiled >= 2,
        "the ref-cell functions were not compiled ({:?})",
        jit.metrics()
    );

    for (call, expected) in [("f(100000, [3, 4])", 700000), ("g(50000, [3, 4])", 350000)] {
        let before = jit.metrics();
        let result = ctx
            .eval(call.as_bytes(), &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        let after = jit.metrics();
        assert_eq!(
            observe(Ok(result)),
            format!("int:{expected}"),
            "{call} returned the wrong value"
        );
        assert_eq!(
            after.backedges - before.backedges,
            0,
            "{call}: the ref-cell loop ran in the interpreter"
        );
    }
}

/// A call re-enters the caller's native code; the caller must still see its own
/// `this` afterwards (the callee may have had a different one).
#[test]
fn this_after_call_matches_interpreter() {
    let define = b"\
        var O5 = { p: 7, h: function(){ var a = (function(){ return 0; })(); return this.p + a; } }; \
        O5.h();";
    native_vs_interpreter(define, "O5.h()");
}

/// Epic E5: functions with `try`/`catch`/`finally` now compile (the `catch`
/// sentinel is pushed natively; a native exception exits and the interpreter's
/// catch scan runs; `finally` uses gosub/ret).
#[test]
fn exceptions_match_interpreter() {
    let define = b"\
        function tc(n){ var s = 0; for (var i = 0; i < n; i++){ try { if (i % 3 === 0) throw i; s = s + i; } catch (e) { s = s + 1; } } return s; } \
        function tc2(n){ var s = 0; for (var i = 0; i < n; i++){ try { var o = null; s = s + o.x; } catch (e) { s = s + 1; } } return s; } \
        function fin(n){ var s = 0; try { for (var i = 0; i < n; i++){ try { s = s + i; if (i === 5) throw 1; } finally { s = s + 100; } } } catch (e) { s = s + 10000; } return s; } \
        function nested(n){ var s = 0; for (var i = 0; i < n; i++){ try { try { if (i === 2) throw 1; s = s + 1; } finally { s = s + 10; } } catch (e) { s = s + 1000; } } return s; } \
        tc(1); tc2(1); fin(1); nested(1);";
    for kernel in ["tc", "tc2", "fin", "nested"] {
        native_vs_interpreter(define, &format!("{kernel}(20)"));
    }
}

#[test]
fn native_loop_handles_gc_and_interrupt_safepoints() {
    // A long allocation loop: the back-edge safepoint must run collections
    // without corrupting the frame, and the result must match the interpreter.
    let define = b"\
        function build(n){ var last = ''; for (var i = 0; i < n; i++){ var s = 'v' + i; last = s; } return last; } \
        build(1);";
    let metrics = native_vs_interpreter(define, "build(20000)");
    assert!(metrics.native_entries > 0);
}

#[test]
fn native_exception_matches_interpreter() {
    // `x + 1n` throws (cannot mix BigInt and Number) from the `add` helper; the
    // function is otherwise fully native, so the exception exits the native run
    // with `pc`/`sp` the interpreter can unwind from.
    let define =
        b"function t(n){ var x = 0; for (var i = 0; i < n; i++) x = x + 1n; return x; } t(0);";

    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-throw.js".into());
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    jit.poll();
    assert!(jit.metrics().functions_compiled > 0, "t was not compiled");

    let native = ctx.eval(b"t(3)", &name, EvalType::Global, EvalFlags::empty());
    assert!(native.is_err(), "native run must throw");
    assert!(jit.metrics().native_entries > 0, "native entry did not run");

    let mut base_rt = Runtime::create().unwrap();
    let mut base_ctx = base_rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    base_ctx
        .eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    let expected = base_ctx.eval(b"t(3)", &name, EvalType::Global, EvalFlags::empty());
    assert_eq!(expected.unwrap_err(), native.unwrap_err());
}
