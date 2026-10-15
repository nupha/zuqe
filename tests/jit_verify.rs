// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Epic B acceptance gate (M0b): run the frontend over the repository's JS
//! corpus, confirm every hot function is either verified or rejected (never
//! both, never dropped), and confirm attaching the JIT does not change any
//! observable result.

#![cfg(feature = "jit")]

use zuqe::{
    EvalFlags, EvalName, EvalType, Jit, JitConfig, JitMetrics, JSValue, Runtime, ScopedContext,
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
    let jit = attach.map(|cfg| Jit::attach(&rt, cfg).unwrap());
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
    let jit = Jit::attach(&rt, eager_config()).unwrap();
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
    let define = b"function t(n){ var x = 0; for (var i = 0; i < n; i++) x = x + 1n; return x; } t(0);";

    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach(&rt, eager_config()).unwrap();
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
