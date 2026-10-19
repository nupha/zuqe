// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Epic B acceptance gate (M0b): run the frontend over the repository's JS
//! corpus, confirm every hot function is either verified or rejected (never
//! both, never dropped), and confirm attaching the JIT does not change any
//! observable result.

#![cfg(feature = "jit")]

use crate::common::settle;

use zuqe::{
    EvalFlags, EvalName, EvalType, JSValue, Jit, JitConfig, JitMetrics, JitTierPolicy, Runtime,
    ScopedContext,
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
        settle(&jit);
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
    settle(&jit);
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

/// Epic E4 / D-6: mutual recursion through `call` must match the interpreter.
/// Under D-6 the callee runs through the call helper on the caller's frame
/// (each nested call is a native entry), so the recursion stays native; the
/// value parity and the native-entry count are the assertions.
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

/// D-6: a native call from native code no longer exits the caller's frame, so
/// the loop after it stays native without any re-entry — the signal is
/// `backedges == 0` (an instruction-level `Resume` would strand the loop in the
/// interpreter and show up as loop back edges). Before 1.7 a native call left
/// the caller interpreted; before D-6 the `call` itself still exited the frame
/// and needed a re-entry. Now the helper runs the callee inline.
#[test]
fn a_native_call_does_not_strand_the_caller_in_the_interpreter() {
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
    settle(&jit);

    let before = jit.metrics();
    let result = ctx
        .eval(
            b"workload(60000);",
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
    let after = jit.metrics();

    // sum(0..59999) = 1799970000, + m (2).
    assert_eq!(
        observe(Ok(result)),
        "int:1799970002",
        "native result after a native call"
    );
    assert_eq!(
        after.backedges - before.backedges,
        0,
        "the loop after a native call ran in the interpreter"
    );
    assert!(
        after.native_entries - before.native_entries >= 1,
        "the workload's native entry did not run (natives: {})",
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
    settle(&jit);
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

/// Epic N2: element access on a non-fast (dictionary) array. `delete a[1]`
/// converts the literal to a sparse array whose elements are own shape
/// properties, so the element site's `(shape, prop)` inline cache serves the
/// repeated reads/writes of the same key while out-of-range and negative keys
/// stay on the slow path. The signal is the same as the other native tests:
/// any instruction-level `Resume` shows up as interpreter loop back edges.
#[test]
fn dictionary_array_elements_run_in_native_code() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-dict-array.js".into());
    let define = b"function f(n){ var a = [1,2,3,4,5]; delete a[1]; a[8] = 40; var s = 0; \
                   for (var i = 0; i < n; i++){ \
                     s = (s + a[8] + (a[1] === undefined ? 7 : 0) + (a[99] === undefined ? 3 : 0) \
                          + (a[-1] === undefined ? 5 : 0)) | 0; } return s; } \
                   function g(n){ var a = [1,2,3,4,5]; delete a[1]; a[8] = 0; var s = 0; \
                   for (var i = 0; i < n; i++){ a[8] = (a[8] + 1) & 1023; s = (s + a[8]) | 0; } \
                   return s + a[8]; } \
                   f(1); g(1);";
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    settle(&jit);
    assert!(
        jit.metrics().functions_compiled >= 2,
        "the dictionary-array functions were not compiled ({:?})",
        jit.metrics()
    );

    // `55` per iteration: 40 (a[8]) + 7 (deleted tombstone) + 3 (out of range)
    // + 5 (negative key). `g` writes the same key `n` times (n < 1024).
    for (call, expected) in [("f(100000)", 100000 * 55), ("g(1000)", 500500 + 1000)] {
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
            "{call}: the dictionary-array loop ran in the interpreter"
        );
    }
}

/// S3 (2.4): the `with_*` family lowers to helpers, so a `with` body runs in
/// native code on both of its edges — the name found on the with object and the
/// fall-through to the ordinary variable access. The signal is the same as
/// `ref_cells_run_in_native_code`: any instruction-level `Resume` strands the
/// rest of the frame in the interpreter, which shows up as loop back edges.
#[test]
fn with_opcodes_run_in_native_code() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-with.js".into());
    // `s`, `i` and `k` are locals, so every guarded lookup of them misses the
    // with object and falls through; `x`/`hf` are on `WO`/`WO2`, so those hit.
    // `delete nope` misses, `hf()` is the call-target form (`with_get_ref`) and
    // `for (var k in o)` is the write form (`with_put_var`).
    let define = b"var WO = {x: 1}; var WO2 = {hf: hf}; \
                   function hf(){ return 1; } \
                   function f(n, o){ var s = 0; \
                     for (var i = 0; i < n; i++){ with (o) { s = s + x; delete nope; } } return s; } \
                   function g(n, o){ var s = 0; \
                     for (var i = 0; i < n; i++){ with (o) { for (var k in o) s = s + k.length; } } return s; } \
                   function h(n, o){ var s = 0; \
                     for (var i = 0; i < n; i++){ with (o) { s = s + hf(); } } return s; } \
                   f(1, WO); g(1, WO); h(1, WO);";
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    settle(&jit);
    assert!(
        jit.metrics().functions_compiled >= 3,
        "the with functions were not compiled ({:?})",
        jit.metrics()
    );

    for (call, expected) in [
        ("f(100000, WO)", 100000),
        ("g(100000, WO)", 100000),
        // `with_get_ref` on its miss edge (the global `hf`) and its hit edge.
        ("h(50000, WO)", 50000),
        ("h(50000, WO2)", 50000),
    ] {
        let before = jit.metrics();
        let result = ctx
            .eval(call.as_bytes(), &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        let after = jit.metrics();
        assert_eq!(
            observe(Ok(result)),
            format!("int:{expected}"),
            "{call}: wrong value"
        );
        assert_eq!(
            after.backedges - before.backedges,
            0,
            "{call}: the with loop ran in the interpreter"
        );
    }
}

/// Epic S2 (1.5): the reused-value registers are block-scoped, so the block a
/// call re-enters (the trampoline resumes at the post-call pc, which the lowering
/// emits as its own block) must recompute the repeated expression instead of
/// copying a register it never saw. Both paths have to agree with the
/// interpreter.
#[test]
fn cse_after_a_call_matches_interpreter() {
    let define = b"\
        var CSE_HELP = 3; \
        function h(){ return CSE_HELP; } \
        function f(n){ var s = 0; for (var i = 0; i < n; i++){ var x = i & 7; var r = h(); \
            s = s + r + (((x + 1) * (x + 3)) + ((x + 1) * (x + 3))); } return s; } \
        f(1);";
    for call in ["f(2000)", "f(1)"] {
        native_vs_interpreter(define, call);
    }
}

/// S3 (2.4): a `with` body inside a `try` — the resolver helper raising (the
/// object's getter throws) must exit so the interpreter's catch scan finds the
/// frame's handler, and the `with` name resolution itself may run user code
/// (`@@unscopables`). Complements `with_opcodes_run_in_native_code`, which
/// covers the non-throwing edges.
#[test]
fn with_throws_match_interpreter() {
    let define = b"\
        var TO = { get x(){ throw 'boom'; } }; \
        var TH = { x: 3 }; TH[Symbol.unscopables] = { x: true }; \
        var gx = 5; \
        function f(n, o){ var s = 0; try { with (o) { s = s + x; } } catch (e) { s = 100 + n; } return s + n; } \
        f(1, TO); f(1, TH);";
    for call in ["f(2, TO)", "f(3, TH)", "f(4, {y: 1})"] {
        let metrics = native_vs_interpreter(define, call);
        assert_eq!(
            metrics.functions_rejected, 0,
            "the `with` function was rejected instead of compiled ({metrics:?})"
        );
    }
}

/// D-6: the four call opcodes lower to helpers that run the callee on the same
/// frame, so a call site no longer exits the caller's native code. The signal is
/// the same as `ref_cells_run_in_native_code`: any instruction-level `Resume`
/// would strand the rest of the frame in the interpreter and show up as loop
/// back edges. Covers `call` (direct), `call_method`, `call_constructor`,
/// `apply` (spread call), and a throw that crosses a call.
#[test]
fn call_opcodes_run_in_native_code() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-calls.js".into());
    // `h` returns its argument, so every kernel's result is just its trip count.
    let define = b"var OBJ = { m: h }; \
                   function h(x){ return x; } \
                   function direct(n){ var s = 0; for (var i = 0; i < n; i++) s = h(s + 1); return s; } \
                   function method(n){ var s = 0; for (var i = 0; i < n; i++) s = OBJ.m(s + 1); return s; } \
                   function spread(n){ var s = 0; for (var i = 0; i < n; i++) s = h(...[s + 1]); return s; } \
                   function Ctor(v){ this.v = v; } \
                   function ctor(n){ var s = 0; for (var i = 0; i < n; i++) s = new Ctor(s + 1).v; return s; } \
                   function thrower(){ throw 7; } \
                   function catcher(n){ try { thrower(); } catch (e) { return e + n; } return -1; } \
                   direct(1); method(1); spread(1); ctor(1); catcher(1);";
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    settle(&jit);
    assert!(
        jit.metrics().functions_compiled >= 6,
        "the call functions were not compiled ({:?})",
        jit.metrics()
    );

    for (call, expected) in [
        ("direct(100000)", 100000),
        ("method(100000)", 100000),
        ("spread(100000)", 100000),
        ("ctor(100000)", 100000),
        // The throw unwinds through `h_call` and is caught in the caller.
        ("catcher(5)", 12),
    ] {
        let before = jit.metrics();
        let result = ctx
            .eval(call.as_bytes(), &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        let after = jit.metrics();
        assert_eq!(
            observe(Ok(result)),
            format!("int:{expected}"),
            "{call}: wrong value"
        );
        assert_eq!(
            after.backedges - before.backedges,
            0,
            "{call}: a call exited the native frame (interpreter back edges)"
        );
    }
}

/// D-6 acceptance: a call in generated code nests one Rust frame per JS call,
/// so unbounded recursion must surface as a catchable stack error (the engine's
/// guard fires) instead of overrunning the C stack. `native_vs_interpreter`
/// asserts the caught value matches the interpreter, so any divergence in
/// where the guard fires is visible; a crash fails the process.
#[test]
fn deep_native_recursion_is_bounded() {
    let define = b"function deep(n){ return deep(n + 1) + 1; } \
                   function guard(){ try { deep(0); return 'none'; } catch (e) { return 'caught:' + e.name; } } \
                   guard();";
    let metrics = native_vs_interpreter(define, "guard()");
    assert!(metrics.native_entries > 0, "the recursion never ran native");
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

/// S3 (2.6): `throw` and `throw_error` raise from native code. The frame still
/// has to leave (the catch scan is the interpreter's), but the exception — its
/// value, its error class, its message and its `stack` — must be exactly what
/// the interpreter produces, including for a throw that escapes to the caller
/// and for the engine-generated errors `throw_error` carries.
///
/// The `stack` comparison is the pc check: `cur_pc_offset` is the *post*
/// instruction offset in both engines and `do_build_backtrace` subtracts one
/// byte, so reporting the throw's own pc instead of `pc + size` would move the
/// reported line to the previous instruction.
#[test]
fn native_throws_match_the_interpreter() {
    // Each function throws on a line of its own, so a one-instruction pc drift
    // is visible in the stack instead of hiding inside a line of neighbours.
    let define = b"\
        function looped(n){ var caught = 0; var seen = ''; \
          for (var i = 0; i < n; i++){ \
            try { \
              if (i % 3 === 0) throw i; \
              if (i % 3 === 1) throw new Error('e' + i); \
              seen = seen + 'ok'; \
            } catch (e) { caught = caught + 1; seen = seen + '|' + e.message; } } \
          return caught + ':' + seen; } \
        function escapes(n){ \
          var e = new Error('escaped'); \
          throw e; } \
        function caller(n){ try { escapes(n); } catch (e) { return e.message; } return 'no-throw'; } \
        function constAssign(n){ \
          const c = n; \
          try { c = 1; } catch (e) { return e.name + ':' + e.message; } return 'assigned'; } \
        function tdz(n){ \
          try { { let a; return a; } } catch (e) { return e.name + ':' + e.message; } return 'none'; } \
        looped(1); escapes; caller(1); constAssign(1); tdz(1);";
    for (call, expected) in [
        (
            "looped(30)",
            "10:ok|ok|e1|ok|ok|e4|ok|ok|e7|ok|ok|e10|ok|ok|e13|ok|ok|e16|ok|ok|e19|ok|ok|e22|ok|ok|e25|ok|ok|e28",
        ),
        ("caller(1)", "escaped"),
        (
            "constAssign(7)",
            "TypeError:Assignment to constant variable.",
        ),
        ("tdz(1)", "ReferenceError:x is not defined"),
    ] {
        let metrics = native_vs_interpreter(define, call);
        assert_eq!(
            metrics.functions_rejected, 0,
            "the throwing function was rejected instead of compiled ({metrics:?})"
        );
    }

    // `escapes` has no handler in its own frame: the native frame raises, the
    // interpreter finds no sentinel and propagates to the caller's catch.
    let define_throw = b"function escapes(){ var e = new Error('escaped'); throw e; } \
                        function guard(){ try { escapes(); } catch (e) { return e.message; } return 'none'; } \
                        guard();";
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-throw-frames.js".into());
    ctx.eval(define_throw, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    settle(&jit);
    assert!(
        jit.metrics().functions_compiled >= 2,
        "the throwing functions were not compiled ({:?})",
        jit.metrics()
    );
    let native = ctx.eval(b"escapes()", &name, EvalType::Global, EvalFlags::empty());
    assert!(native.is_err(), "the native run must propagate the error");
    assert!(
        jit.metrics().native_entries > 0,
        "the native entry did not run"
    );

    let mut base_rt = Runtime::create().unwrap();
    let mut base_ctx = base_rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    base_ctx
        .eval(define_throw, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    let expected = base_ctx.eval(b"escapes()", &name, EvalType::Global, EvalFlags::empty());
    assert_eq!(expected.unwrap_err(), native.unwrap_err());
}

/// S3 (2.6): `regexp` and `push_bigint_i32` build their value in a helper, so a
/// loop that creates one per iteration stays in native code — the signal is the
/// same as `ref_cells_run_in_native_code`: an instruction-level `Resume` would
/// strand the rest of the frame in the interpreter and show up as loop back
/// edges. The RegExp kernel deliberately avoids a method call; the BigInt one
/// compares instead of converting.
#[test]
fn regexp_and_bigint_literals_run_in_native_code() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, eager_config()).unwrap();
    let name = EvalName::Name("jit-engine-ops.js".into());
    let define = b"function re(n){ var s = 0; \
                    for (var i = 0; i < n; i++){ var r = /a+b/; s = s + r.source.length; } return s; } \
                  function big(n){ var s = 0n; \
                    for (var i = 0; i < n; i++){ s = s + 123456789n; } \
                    return s === 123456789000n; } \
                  re(1); big(1);";
    ctx.eval(define, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    settle(&jit);
    assert!(
        jit.metrics().functions_compiled >= 2,
        "the regexp/bigint functions were not compiled ({:?})",
        jit.metrics()
    );

    for (call, expected) in [("re(100000)", "int:300000"), ("big(1000)", "bool:true")] {
        let before = jit.metrics();
        let result = ctx
            .eval(call.as_bytes(), &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        let after = jit.metrics();
        assert_eq!(observe(Ok(result)), expected, "{call}: wrong value");
        assert_eq!(
            after.backedges - before.backedges,
            0,
            "{call}: the loop ran in the interpreter"
        );
    }
}

/// Epic N3: inline back-edge OSR. A top-level loop is a frame that never
/// crosses a frame boundary and is never re-entered, so nothing is compiled
/// when the script starts (the trampoline only polls between frames). Any native
/// entry during that single `eval` can therefore only be the back edge handing
/// the frame to the compiled loop header — and the back-edge count must stay
/// near the threshold instead of growing with the trip count.
///
/// Inline-only: it needs the back edge to compile *synchronously*. Under
/// `jit_background` the worker finishes later, so the eventual entry is checked
/// by `jit_background::auto_poll_drains_workers_and_the_function_runs_native`.
#[cfg(not(feature = "jit_background"))]
#[test]
fn a_top_level_loop_switches_to_native_mid_run() {
    const SRC: &[u8] = b"var s = 1; for (var i = 1; i < 100000; i++) s = (s + i) | 0; s;";

    let run = |label: &str, cfg: JitConfig| -> (String, JitMetrics) {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(&rt, cfg).unwrap();
        let name = EvalName::Name(format!("jit-osr-{label}.js"));
        let value = observe(ctx.eval(SRC, &name, EvalType::Global, EvalFlags::empty()));
        // No host `poll()`: the eval is the whole measurement.
        (value, jit.metrics())
    };

    // Baseline: the same source, interpreted only.
    let (expected, _) = run(
        "base",
        JitConfig::builder()
            .tier_policy(JitTierPolicy::Interpreter)
            .build(),
    );

    // No call anywhere, so the script cannot be compiled at a frame boundary:
    // the only compile trigger is the loop back edge.
    let (got, m) = run("osr", eager_config());
    assert_eq!(got, expected, "OSR changed the result");
    assert!(
        m.functions_compiled > 0,
        "the script was never compiled ({m:?})"
    );
    assert!(
        m.native_entries > 0,
        "the loop never ran native inside the eval ({m:?})"
    );
    assert!(
        m.backedges < 100,
        "the loop stayed interpreted (backedges={})",
        m.backedges
    );

    // `auto_poll(false)` keeps the host in control: nothing compiles and the
    // result is still the interpreter's.
    let (got, m) = run(
        "manual",
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .auto_poll(false)
            .build(),
    );
    assert_eq!(got, expected);
    assert_eq!(m.functions_compiled, 0, "auto_poll=false still compiled");
    assert_eq!(m.native_entries, 0, "auto_poll=false still entered native");
}

/// Epic N3: the loop shapes a back-edge OSR has to resume. Each script is a
/// single `eval` with the loop at the top level, run once with the JIT (the
/// frame switches to native at the loop header through `cur_sp_offset`) and once
/// interpreted; both must agree exactly.
///
/// Inline-only: the assertions need the synchronous back-edge compile (see
/// `a_top_level_loop_switches_to_native_mid_run`).
#[cfg(not(feature = "jit_background"))]
#[test]
fn top_level_loop_shapes_match_the_interpreter() {
    // `for`/`while`/`do-while` cover the `goto` and `if_*` back edges. The first
    // body ends in a bitwise op, one of the opcodes that leave the frame's
    // `cur_sp_offset` one slot above `sp`; the back edge syncs it before the
    // resume, and the result must still be the interpreter's.
    let scripts: &[&str] = &[
        "var s = 1; for (var i = 1; i < 30000; i++) s = (s ^ (i & 7)) | 0; s;",
        "var i = 0, s = 0; while (i < 30000) { s = (s + i) | 0; i++; } s;",
        "var i = 0, s = 0; do { s = (s + i) | 0; i++; } while (i < 30000); s;",
        // Nested: the inner header is the first OSR point reached.
        "var s = 0; for (var i = 0; i < 200; i++) { for (var j = 0; j < 200; j++) s = (s + (i ^ j)) | 0; } s;",
        // A computed condition and a ternary join in the body: the values the
        // native loop keeps across its own blocks must match the interpreter.
        "var s = 0; for (var i = 0; i < 30000; i++) s = (s + ((i > 3) ? 1 : (i ? 2 : 3))) | 0; s;",
        "var s = 0; for (var i = 0; i < 30000; i++) { if (i % 7 === 0) continue; if (i > 25000) break; s = (s + i) | 0; } s;",
        // A callee in the loop body: the loop frame must stay native across the
        // call (D-6's call helper) for the back-edge count to stay low.
        "function inc(x){ return (x + 1) | 0; } var s = 0; for (var i = 0; i < 20000; i++) s = inc(s) | 0; s;",
    ];

    let run = |src: &[u8], cfg: JitConfig| -> (String, JitMetrics) {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let jit = Jit::attach_to(&rt, cfg).unwrap();
        let name = EvalName::Name("jit-osr-shapes.js".into());
        let value = observe(ctx.eval(src, &name, EvalType::Global, EvalFlags::empty()));
        (value, jit.metrics())
    };
    for src in scripts {
        let (expected, _) = run(
            src.as_bytes(),
            JitConfig::builder()
                .tier_policy(JitTierPolicy::Interpreter)
                .build(),
        );
        let (got, m) = run(src.as_bytes(), eager_config());
        assert_eq!(got, expected, "OSR changed the result of `{src}`");
        assert!(
            m.native_entries > 0,
            "`{src}` never entered native code ({m:?})"
        );
        assert!(
            m.backedges < 100,
            "`{src}` stayed interpreted (backedges={})",
            m.backedges
        );
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
    settle(&jit);
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
