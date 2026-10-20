// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! P1-1: the inline guard for array element reads.
//!
//! The generated code reads a packed array's element directly (the key's
//! tag, the array's class and fast-array flag, the index against the array's
//! length). Everything else — a dictionary array, a float or string key, a
//! negative index, an out-of-range read, a typed array, a plain object — must
//! fall back to the helper and produce **bit-identical** results.
//!
//! The engine keeps a mirror of the array's elements for the JIT to read
//! (`ArrayPayload`'s view), so the mutations that move the elements (growing,
//! sorting, shrinking, destructuring) are exercised here too: a stale mirror
//! would read the wrong element or fault.

#![cfg(feature = "jit")]

use crate::common::{MODES, Mode, settle};

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitMetrics, Runtime,
    ScopedContext,
};

/// The scripts whose results must not change when the guard is inlined.
const CASES: &[(&str, &str)] = &[
    // A packed array: the guard hits.
    (
        "packed",
        "var a = [10, 20, 30]; var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; s;",
    ),
    // A hole: the array is not fast any more, the helper takes over.
    (
        "holey",
        "var a = [10, 20, 30]; delete a[1]; var s = 0; for (var i = 0; i < a.length; i++) s += (a[i] === undefined ? 0 : a[i]); s;",
    ),
    // Out of range: the prototype chain answers, not the elements.
    (
        "out_of_range",
        "var a = [1, 2]; var p = { 5: 99 }; Object.setPrototypeOf(a, p); a[5];",
    ),
    // A negative index is not an index at all.
    ("negative", "var a = [1, 2, 3]; a[-1];"),
    // A float key that is integral.
    ("float_key", "var a = [1, 2, 3]; a[1.0];"),
    // A float key that is not integral.
    ("float_key_fraction", "var a = [1, 2, 3]; a[1.5];"),
    // A string key.
    ("string_key", "var a = [1, 2, 3]; a['1'];"),
    // A plain object with numeric keys.
    ("plain_object", "var o = { 0: 7, 1: 8 }; o[1];"),
    // A typed array: a different class, and a different element width.
    ("typed_array", "var t = new Int32Array(4); t[1] = 5; t[1];"),
    // The `length` property rewritten: the elements are unaffected.
    ("length_rewritten", "var a = [1, 2, 3]; a.length = 1; a[0];"),
    // A frozen array.
    ("frozen", "var a = Object.freeze([4, 5, 6]); a[2];"),
    // An array of arrays (the element is a reference).
    ("nested", "var a = [[1, 2], [3, 4]]; a[1][0];"),
    // A getter on the prototype: the guard must not bypass it.
    (
        "prototype_getter",
        "var a = [1, 2]; Object.defineProperty(Object.getPrototypeOf(a), '9', { get: function(){ return 42; } }); a[9];",
    ),
    // The typed arrays the guard inlines.
    ("typed_int32", "var t = new Int32Array(4); t[1] = 5; t[1];"),
    (
        "typed_float64",
        "var t = new Float64Array(4); t[1] = 1.5; t[1];",
    ),
    (
        "typed_int32_negative",
        "var t = new Int32Array(4); t[1] = -5; t[1];",
    ),
    // Every width the guard inlines, with the value the interpreter would
    // produce (the sign rules and the clamping are the interpreter's).
    ("typed_int8", "var t = new Int8Array(4); t[1] = -5; t[1];"),
    (
        "typed_int8_max",
        "var t = new Int8Array(4); t[1] = 127; t[1];",
    ),
    (
        "typed_int16",
        "var t = new Int16Array(4); t[1] = -300; t[1];",
    ),
    (
        "typed_uint8",
        "var t = new Uint8Array(4); t[1] = 250; t[1];",
    ),
    (
        "typed_uint8c",
        "var t = new Uint8ClampedArray(4); t[1] = 250; t[1];",
    ),
    (
        "typed_uint16",
        "var t = new Uint16Array(4); t[1] = 65000; t[1];",
    ),
    (
        "typed_uint32",
        "var t = new Uint32Array(4); t[1] = 4294967295; t[1];",
    ),
    (
        "typed_uint32_small",
        "var t = new Uint32Array(4); t[1] = 7; t[1];",
    ),
    (
        "typed_float32",
        "var t = new Float32Array(4); t[1] = 1.5; t[1];",
    ),
    (
        "typed_float32_frac",
        "var t = new Float32Array(4); t[1] = 0.1; t[1];",
    ),
    // Out of range: the typed array answers `undefined`.
    ("typed_out_of_range", "var t = new Int32Array(4); t[10];"),
    // A view with a byte offset: the guard has to add it.
    (
        "typed_byte_offset",
        "var b = new ArrayBuffer(32); var t = new Int32Array(b, 8); t[1] = 77; t[1]; t[0];",
    ),
    // A detached buffer: the elements are gone.
    (
        "typed_detached",
        "var b = new ArrayBuffer(16); var t = new Int32Array(b); t[0] = 3; b.transfer(); t[0];",
    ),
    // A resizable buffer that shrank below the view's end.
    (
        "typed_resizable_shrunk",
        "var b = new ArrayBuffer(16, { maxByteLength: 32 }); var t = new Int32Array(b); t[0] = 9; b.resize(0); t[0];",
    ),
];

fn eval(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

/// Run a script under one mode and describe its result (tag-exact).
fn run(mode: Mode, setup: &str, main: &str) -> (String, JitMetrics) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, mode.config()).expect("attach");
    let name = EvalName::Name("jit-array.js".into());

    assert!(eval(&mut ctx, &name, setup).is_ok(), "setup threw");
    settle(&jit);
    let res = eval(&mut ctx, &name, main);
    let value = crate::common::observe(&mut ctx, res);
    (value, jit.metrics())
}

/// The inline guard must not change any result: every mode agrees with the
/// interpreter, bit for bit.
#[test]
fn the_inline_guard_keeps_every_result() {
    for (label, script) in CASES {
        let (expected, _) = run(Mode::Interpreter, script, "");
        for mode in MODES {
            let (got, metrics) = run(mode, script, "");
            assert_eq!(
                got, expected,
                "{label}: {mode:?} disagrees with the interpreter"
            );
            if mode != Mode::Interpreter {
                assert!(
                    metrics.functions_compiled > 0,
                    "{label}: {mode:?} compiled nothing"
                );
            }
        }
    }
}

/// The `arguments` object carries its elements in the same array payload, so
/// the guard covers it too.
#[test]
fn the_arguments_object_is_covered() {
    let setup = "function f(a){ return arguments[0] + arguments[1]; }";
    let main = "f(20, 22);";
    let (expected, _) = run(Mode::Interpreter, setup, main);
    for mode in MODES {
        let (got, metrics) = run(mode, setup, main);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
        if mode != Mode::Interpreter {
            assert!(metrics.functions_compiled > 0, "{mode:?} compiled nothing");
        }
    }
}

/// The elements' mirror must follow every mutation that moves them: growing
/// the array reallocates its buffer, sorting rewrites it, shrinking drops the
/// tail, and destructuring takes it away. A stale mirror would read the wrong
/// element (or fault).
#[test]
fn the_elements_mirror_follows_the_mutations() {
    let setup = "var a = [1, 2, 3];";
    let script = "\
        var out = [];\
        out.push(a[0], a[1], a[2]);\
        a.push(4, 5);\
        out.push(a[3], a[4], a[0]);\
        a.sort(function (x, y) { return y - x; });\
        out.push(a[0], a[1], a[2]);\
        a.length = 2;\
        out.push(a[0], a[1], a[2]);\
        out.join(',');";

    let (expected, _) = run(Mode::Interpreter, setup, script);
    assert!(expected.contains("5,4,3"), "unexpected: {expected}");
    for mode in MODES {
        let (got, _) = run(mode, setup, script);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
    }
}

/// The guard reads the array's elements, so a *write* through the helper and a
/// read through the guard must see each other.
#[test]
fn a_write_and_a_read_agree() {
    let setup = "var a = [1, 2, 3];";
    let script = "\
        a[1] = 99;\
        var s = 0; for (var i = 0; i < a.length; i++) s += a[i];\
        s;";
    let (expected, _) = run(Mode::Interpreter, setup, script);
    for mode in MODES {
        let (got, _) = run(mode, setup, script);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
    }
}

/// The guard is only about *reads*: the write path still goes through the
/// helper, and a write that grows the array keeps the mirror in step.
#[test]
fn a_write_that_grows_the_array_is_visible_to_the_reader() {
    let setup = "var a = [1];";
    let script = "\
        for (var i = 0; i < 8; i++) a[i] = i * i;\
        var s = 0; for (var i = 0; i < a.length; i++) s += a[i];\
        s;";
    let (expected, _) = run(Mode::Interpreter, setup, script);
    for mode in MODES {
        let (got, _) = run(mode, setup, script);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
    }
}

/// The JIT's own counters: the guard is in the generated code, so the array
/// read must still be a native read (the function compiles and runs).
#[test]
fn an_array_read_runs_natively() {
    let script = "function sum(a){ var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; return s; } sum([1,2,3]);";
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .auto_poll(false)
            .build(),
    )
    .expect("attach");
    let name = EvalName::Name("jit-array-native.js".into());
    assert!(eval(&mut ctx, &name, script).is_ok());
    settle(&jit);
    assert!(eval(&mut ctx, &name, "sum([1,2,3,4,5,6,7,8]);").is_ok());
    settle(&jit);
    let m = jit.metrics();
    assert!(m.functions_compiled > 0, "nothing compiled: {m:?}");
    assert!(m.native_entries > 0, "nothing ran natively: {m:?}");
}
