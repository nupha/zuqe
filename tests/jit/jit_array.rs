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
    // T9: the float read folds integral elements to `Int`, exactly like the
    // interpreter's `JSValue::from(f64)` — `-0.0` and out-of-`i32` stay `f64`.
    (
        "typed_float64_integral",
        "var t = new Float64Array(4); t[1] = 3; t[1];",
    ),
    (
        "typed_float32_integral",
        "var t = new Float32Array(4); t[1] = 7; t[1];",
    ),
    (
        "typed_float64_negative_zero",
        "var t = new Float64Array(4); t[1] = -0; t[1];",
    ),
    (
        "typed_float64_out_of_int_range",
        "var t = new Float64Array(4); t[1] = 2147483648; t[1];",
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
    // The method-call shape (`get_array_el2`) on a typed array element: the
    // array stays on the stack and the value takes the key's slot.
    (
        "typed_element_method_call",
        "var t = new Int32Array([7]); t[0].toString();",
    ),
    (
        "typed_element_method_call_float",
        "var t = new Float64Array([1.5]); t[0].toString();",
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

/// The scripts whose results must not change when the **write** is inlined.
///
/// Each case defines a function and calls it once (the warm-up: the JIT
/// compiles it then), and the *observed* call is the second one — that one
/// runs natively, so the store really goes through the inline guard.
///
/// The inline store answers a packed array with an integer index and a typed
/// array whose element is numeric (`Int`/`Float64`); everything else — a
/// dictionary array, a float or string key, a negative index, a store that
/// grows the array, a frozen or sealed array, a non-numeric value, a BigInt or
/// f16 element, a plain object — goes through the helper, which owns the full
/// semantics.
const PUT_CASES: &[(&str, &str)] = &[
    // The guard hits: an in-bounds store into a packed array.
    (
        "packed",
        "function f(){ var a = [10, 20, 30]; a[1] = 99; return a.join(','); } f();",
    ),
    // The store grows the array (the index is the length): the helper owns it.
    (
        "append",
        "function f(){ var a = [1, 2]; a[2] = 3; return a.join(','); } f();",
    ),
    // A store past the end makes a hole: the helper owns it.
    (
        "sparse",
        "function f(){ var a = [1, 2]; a[5] = 9; return a.join(','); } f();",
    ),
    // A negative index is not an index at all.
    (
        "negative",
        "function f(){ var a = [1, 2, 3]; a[-1] = 7; return a.join(','); } f();",
    ),
    // A float key that is integral.
    (
        "float_key",
        "function f(){ var a = [1, 2, 3]; a[1.0] = 7; return a.join(','); } f();",
    ),
    // A string key.
    (
        "string_key",
        "function f(){ var a = [1, 2, 3]; a['1'] = 7; return a.join(','); } f();",
    ),
    // A plain object with numeric keys.
    (
        "plain_object",
        "function f(){ var o = { 0: 7, 1: 8 }; o[1] = 42; return o[1]; } f();",
    ),
    // A typed array: a different class, and a different element width.
    (
        "typed_array",
        "function f(){ var t = new Int32Array(4); t[1] = 5; return t[1]; } f();",
    ),
    (
        "typed_clamped",
        "function f(){ var t = new Uint8ClampedArray(4); t[1] = 250; return t[1]; } f();",
    ),
    // A frozen array: the store is rejected (silently in sloppy mode).
    (
        "frozen",
        "function f(){ var a = Object.freeze([4, 5, 6]); a[2] = 7; return a.join(','); } f();",
    ),
    // A frozen array in strict mode: the store throws.
    (
        "frozen_strict",
        "function f(){ 'use strict'; var a = Object.freeze([4, 5, 6]); a[2] = 7; return a.join(','); } try { f(); } catch (e) {}",
    ),
    // A sealed array: the elements stay writable, but the guard must not
    // answer it (the engine's fast write only runs on an extensible array).
    (
        "sealed",
        "function f(){ var a = Object.seal([4, 5, 6]); a[2] = 7; return a.join(','); } f();",
    ),
    // A hole: the array is not fast any more, the helper takes over.
    (
        "holey",
        "function f(){ var a = [1, 2, 3]; delete a[1]; a[1] = 8; return a.join(','); } f();",
    ),
    // A store of a reference: the value needs the collector's write barrier.
    (
        "object_value",
        "function f(){ var a = [1, 2]; a[0] = { tag: 'kept' }; return a[0].tag; } f();",
    ),
    // A store of a string: a reference as well.
    (
        "string_value",
        "function f(){ var a = [1, 2]; a[0] = 'hello'; return a[0]; } f();",
    ),
    // A store of `undefined` / `null`.
    (
        "other_values",
        "function f(){ var a = [1, 2]; a[0] = undefined; a[1] = null; return a.join(','); } f();",
    ),
    // The array's length was rewritten: the elements are the ones that are
    // left.
    (
        "length_rewritten",
        "function f(){ var a = [1, 2, 3]; a.length = 2; a[0] = 9; return a.join(','); } f();",
    ),
    // A prototype that owns the index: the store must not reach it.
    (
        "prototype_index",
        "function f(){ var a = [1, 2]; var p = { 5: 99 }; Object.setPrototypeOf(a, p); a[5] = 7; return a[5]; } f();",
    ),
    // A setter on the prototype: the guard must not bypass it.
    (
        "prototype_setter",
        "function f(){ var a = [1, 2]; Object.defineProperty(Object.getPrototypeOf(a), '9', { set: function(v){ this.got = v; } }); a[9] = 7; return a.got; } f();",
    ),
    // The store and the read of the same element, in a loop: the two inline
    // paths have to see each other.
    (
        "write_then_read",
        "function f(){ var a = [0, 0, 0]; for (var i = 0; i < 8; i++) { a[i % 3] = i; } return a.join(','); } f();",
    ),
    // A **method call on an element**: the lookup is a `get_array_el2` (the
    // array stays on the stack) — the same guard, another shape.
    (
        "element_method_call",
        "var a = [function(){ return 1; }, function(){ return 2; }]; function f(){ var s = 0; for (var i = 0; i < 10; i++) s += a[i % 2](); return s; } f();",
    ),
    // The **optional chain** (`a?.[i]`): the chain's opcodes are rewritten to
    // the plain ones (the resolver turns `get_array_el_opt_chain` into
    // `get_array_el` ✓), so the read goes through the inline guard ✓ — and a
    // nullish target gives `undefined` instead of throwing ✓.
    (
        "optional_chain",
        "var a = [1, 2, 3]; function f(){ return a?.[1]; } f();",
    ),
    (
        "optional_chain_nullish",
        "var a = null; function f(){ return a?.[1]; } f();",
    ),
    (
        "optional_chain_chained",
        "var a = [1, 2, 3]; function f(){ return a?.[0]?.toString?.(); } f();",
    ),
    // A **typed array**: its elements live in an `ArrayBuffer`, so the write
    // goes through the view (offset, count) and the buffer.
    (
        "typed_int32",
        "var t = new Int32Array(4); function f(){ for (var i = 0; i < 8; i++) t[i % 4] = i; return t.join(','); } f();",
    ),
    (
        "typed_float64",
        "var t = new Float64Array(4); function f(){ for (var i = 0; i < 8; i++) t[i % 4] = i * 0.25 + 0.1; return t.join(','); } f();",
    ),
    // The integer widths truncate (the engine's `val as u8` and friends).
    (
        "typed_int8_truncates",
        "var t = new Int8Array(2); function f(){ t[0] = 300; t[1] = -300; return t.join(','); } f();",
    ),
    (
        "typed_uint8_truncates",
        "var t = new Uint8Array(2); function f(){ t[0] = 300; t[1] = -1; return t.join(','); } f();",
    ),
    (
        "typed_int16_truncates",
        "var t = new Int16Array(1); function f(){ t[0] = 70000; return t.join(','); } f();",
    ),
    // The element's width is the store's width: a narrower write must not
    // reach the neighbour.
    (
        "typed_int8_no_spill",
        "var t = new Int8Array(2); function f(){ t[1] = 7; t[0] = 300; return t.join(','); } f();",
    ),
    (
        "typed_uint8_no_spill",
        "var t = new Uint8Array(2); function f(){ t[1] = 7; t[0] = 300; return t.join(','); } f();",
    ),
    (
        "typed_int16_no_spill",
        "var t = new Int16Array(2); function f(){ t[1] = 7; t[0] = 70000; return t.join(','); } f();",
    ),
    (
        "typed_uint16_no_spill",
        "var t = new Uint16Array(2); function f(){ t[1] = 7; t[0] = 70000; return t.join(','); } f();",
    ),
    // The lvalue read (`get_array_el3`) shares the store path.
    (
        "typed_lvalue_increment",
        "var t = new Int32Array(2); function f(){ t[0] = 1.5; t[0]++; t[1]++; return t.join(','); } f();",
    ),
    // A non-numeric value needs `ToNumber` (it may run user code), so the
    // helper owns it.
    (
        "typed_object_value_into_int",
        "var t = new Int32Array(2); var o = { valueOf: function(){ return 1.75; } }; function f(){ t[0] = o; t[1] = '2.5'; return t.join(','); } f();",
    ),
    (
        "typed_bigint_into_int_throws",
        "var t = new Int32Array(1); function f(){ try { t[0] = 5n; } catch (e) { return e instanceof TypeError; } return false; } f();",
    ),
    // The classes whose conversion stays in the interpreter.
    (
        "typed_bigint64",
        "var t = new BigInt64Array(2); function f(){ t[0] = 5n; t[1] = -7n; return t.join(','); } f();",
    ),
    (
        "typed_float16",
        "var t = new Float16Array(2); function f(){ t[0] = 0.1; t[1] = 70000; return t.join(','); } f();",
    ),
    // A `Float32Array` keeps the double's rounded value.
    (
        "typed_float32_rounds",
        "var t = new Float32Array(1); function f(){ t[0] = 0.1; return t[0]; } f();",
    ),
    // The clamped width: round to nearest even, then clamp.
    (
        "typed_uint8c_clamps",
        "var t = new Uint8ClampedArray(1); function f(){ t[0] = 300; return t[0]; } f();",
    ),
    (
        "typed_uint8c_rounds_and_clamps",
        "var t = new Uint8ClampedArray(8); function f(){ t[0] = 0.5; t[1] = 1.5; t[2] = 2.5; t[3] = 254.5; t[4] = 300.4; t[5] = -5; t[6] = NaN; t[7] = Infinity; return t.join(','); } f();",
    ),
    // A `Float64` into an integer width: `ToInt32` is truncation, then a wrap
    // modulo 2^32 (the same low bits for the narrower widths).
    (
        "typed_float_fraction_into_int",
        "var t = new Int32Array(4); function f(){ t[0] = 1.75; t[1] = -1.75; t[2] = 4294967297.5; t[3] = 0.5; return t.join(','); } f();",
    ),
    (
        "typed_float_wraps_into_int8",
        "var t = new Int8Array(4); function f(){ t[0] = 300.9; t[1] = -300.9; t[2] = 2147483648; t[3] = 1e300; return t.join(','); } f();",
    ),
    (
        "typed_float_specials_into_int",
        "var t = new Int16Array(4); function f(){ t[0] = NaN; t[1] = Infinity; t[2] = -Infinity; t[3] = -2.5; return t.join(','); } f();",
    ),
    (
        "typed_float_into_uint32",
        "var t = new Uint32Array(3); function f(){ t[0] = 4294967295.5; t[1] = -1.5; t[2] = 2147483648.5; return t.join(','); } f();",
    ),
    // The `ToInt32` boundaries: both sides of 2^31, 2^32 and 2^53, the
    // exponent range where the wrap applies, and the specials.
    (
        "typed_float_boundaries_into_int",
        "var v = [0.5, -0.5, 1.5, -1.5, 1.9, -1.9, 2147483647.9, 2147483648.1, -2147483648.1, -2147483649.1, 4294967295.9, 4294967296.1, 4294967297.1, -4294967297.1, 9007199254740991, 9007199254740992, 1e21, -1e21, 1e300, -1e300, 5e-324, NaN, Infinity, -Infinity]; \
         var t = new Int32Array(v.length); function f(){ for (var i = 0; i < v.length; i++) t[i] = v[i]; return t.join(','); } f();",
    ),
    (
        "typed_float_boundaries_clamped",
        "var v = [0.5, 1.5, 2.5, 127.5, 128.5, 254.5, 254.49999999999997, 255.1, -0.5, -1.5, 1e300, -1e300, 5e-324, NaN, Infinity, -Infinity]; \
         var t = new Uint8ClampedArray(v.length); function f(){ for (var i = 0; i < v.length; i++) t[i] = v[i]; return t.join(','); } f();",
    ),
    // An `Int` into a float width (`ToNumber` is the value itself).
    (
        "typed_float_value_into_int",
        "var t = new Int32Array(1); function f(){ t[0] = 1.5; return t[0]; } f();",
    ),
    (
        "typed_int_value_into_float",
        "var t = new Float64Array(1); function f(){ t[0] = 3; return t[0] === 3; } f();",
    ),
    (
        "typed_int_value_into_float_loop",
        "var src = new Int32Array(4); var dst = new Float64Array(4); function f(){ for (var i = 0; i < 4; i++) dst[i] = src[i] + 1; return dst.join(','); } f();",
    ),
    (
        "typed_int_value_into_float32",
        "var t = new Float32Array(2); function f(){ t[0] = 3; t[1] = 16777217; return t.join(','); } f();",
    ),
    // Out of range: the write is dropped.
    (
        "typed_out_of_range",
        "var t = new Int32Array(4); function f(){ t[10] = 1; return t.join(','); } f();",
    ),
    // A **shared** buffer: its bytes live behind a lock, so the guard must
    // miss and the helper owns the write.
    (
        "typed_shared",
        "var b = new SharedArrayBuffer(16); var t = new Int32Array(b); function f(){ t[0] = 7; return t[0]; } f();",
    ),
    // A **detached** buffer: the elements are gone.
    (
        "typed_detached",
        "var b = new ArrayBuffer(16); var t = new Int32Array(b); function f(){ t[0] = 3; b.transfer(); return t[0]; } f();",
    ),
    // A view with a byte offset: the write has to add it.
    (
        "typed_byte_offset",
        "var b = new ArrayBuffer(32); var t = new Int32Array(b, 8); function f(){ t[1] = 77; return t.join(','); } f();",
    ),
    // A resizable buffer that grew: the view's count is read fresh.
    (
        "typed_resizable_grown",
        "var b = new ArrayBuffer(16, { maxByteLength: 32 }); var t = new Int32Array(b); function f(){ b.resize(32); t[7] = 5; return t[7]; } f();",
    ),
    // A resizable buffer that shrank below the view's end: the write is
    // dropped.
    (
        "typed_resizable_shrunk",
        "var b = new ArrayBuffer(16, { maxByteLength: 32 }); var t = new Int32Array(b); function f(){ t[0] = 9; b.resize(0); return t[0]; } f();",
    ),
];

/// The inline store must not change any result: every mode agrees with the
/// interpreter, bit for bit.
#[test]
fn the_inline_store_keeps_every_result() {
    for (label, setup) in PUT_CASES {
        let main = "f();";
        let (expected, _) = run(Mode::Interpreter, setup, main);
        for mode in MODES {
            let (got, metrics) = run(mode, setup, main);
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

/// A store of a reference must be visible to the collector: the value lives
/// in the array's elements, and the array is what keeps it alive. The store
/// happens between two rounds of allocations, so it can land in a marking
/// phase — which is exactly when the write barrier matters.
#[test]
fn a_stored_reference_survives_the_collector() {
    let setup = "\
        function f(){ \
          var a = [1, 2, 3]; \
          for (var i = 0; i < 5000; i++) { var before = { i: i }; } \
          a[1] = { tag: 'kept' }; \
          for (var i = 0; i < 5000; i++) { var after = { i: i, s: 'x' + i }; } \
          return a[1].tag; \
        } f();";
    let main = "f();";
    let (expected, _) = run(Mode::Interpreter, setup, main);
    assert_eq!(expected, "str:kept", "unexpected: {expected}");
    for mode in MODES {
        let (got, _) = run(mode, setup, main);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
    }
}

/// The `arguments` object carries its elements in the same array payload, so
/// the store guard covers it too.
#[test]
fn the_arguments_object_is_written_inline() {
    let setup = "function f(a){ arguments[0] = 99; return a + arguments[0]; } f(1);";
    let main = "f(20);";
    let (expected, _) = run(Mode::Interpreter, setup, main);
    for mode in MODES {
        let (got, metrics) = run(mode, setup, main);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
        if mode != Mode::Interpreter {
            assert!(metrics.functions_compiled > 0, "{mode:?} compiled nothing");
        }
    }
}
