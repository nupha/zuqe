// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! P1-1 (the property half): the inline guard for property reads.
//!
//! The generated code reads an own **data** property straight out of the
//! object's slot (the target's tag, the inline-cache site's shape, that
//! shape's id, the slot's range and kind). Everything else — a property on
//! the prototype chain, an accessor, a variable reference, a tombstone, a
//! non-object target — must fall back to the helper and produce
//! **bit-identical** results.
//!
//! The engine keeps a mirror of the object's property slots for the JIT to
//! read (`ObjectImpl`'s view), so the mutations that move the slots (adding
//! a property, deleting one, compacting) are exercised here too: a stale
//! mirror would read the wrong slot (or fault).

#![cfg(feature = "jit")]

use crate::common::{MODES, Mode, settle};

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitMetrics, Runtime,
    ScopedContext,
};

/// The scripts whose results must not change when the guard is inlined:
/// `(label, setup, main)`. The setup defines what the main reads; the main is
/// what runs (and what the JIT compiles).
const CASES: &[(&str, &str, &str)] = &[
    // A plain data property: the guard hits.
    (
        "own_data",
        "var o = { x: 10, y: 20 };",
        "var s = 0; for (var i = 0; i < 100; i++) s += o.x + o.y; s;",
    ),
    // The property is the only one: the slot is the first.
    (
        "single_property",
        "var o = { x: 7 };",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.x; s;",
    ),
    // A property whose value is an object (the slot holds a reference).
    (
        "object_value",
        "var o = { p: { q: 5 } };",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.p.q; s;",
    ),
    // A property on the prototype chain: not an own property, so the helper
    // walks the chain.
    (
        "prototype_chain",
        "var p = { x: 3 }; var o = Object.create(p);",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.x; s;",
    ),
    // An own accessor: the guard must not bypass the getter.
    (
        "own_accessor",
        "var o = {}; Object.defineProperty(o, 'x', { get: function(){ return 42; } });",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.x; s;",
    ),
    // An accessor on the prototype.
    (
        "prototype_accessor",
        "var p = {}; Object.defineProperty(p, 'x', { get: function(){ return 42; } }); var o = Object.create(p);",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.x; s;",
    ),
    // A deleted property: the slot is a tombstone.
    (
        "deleted",
        "var o = { x: 1 }; delete o.x;",
        "var s = 0; for (var i = 0; i < 10; i++) s += (o.x === undefined ? 0 : o.x); s;",
    ),
    // A property that becomes an accessor after it was read as data: the
    // shape changes, so the guard misses.
    (
        "becomes_an_accessor",
        "var o = { x: 1 };",
        "var s = 0; for (var i = 0; i < 10; i++) { s += o.x; if (i === 5) Object.defineProperty(o, 'x', { get: function(){ return 2; } }); } s;",
    ),
    // A property added in the middle: the shape changes, the guard misses.
    (
        "grows",
        "var o = { x: 1 };",
        "var s = 0; for (var i = 0; i < 10; i++) { s += o.x; if (i === 5) o.y = 2; } s;",
    ),
    // Two shapes at one site: the guard hits the shape it saw last.
    (
        "polymorphic",
        "var a = { x: 1 }; var b = { x: 2, z: 3 };",
        "var s = 0; for (var i = 0; i < 10; i++) s += (i % 2 ? a.x : b.x); s;",
    ),
    // An array's `length`: the property the fast arrays keep in their first
    // slot.
    (
        "array_length",
        "var a = [1, 2, 3];",
        "var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; s;",
    ),
    // A typed array's `length`.
    (
        "typed_array_length",
        "var t = new Int32Array(4);",
        "var s = 0; for (var i = 0; i < t.length; i++) s += t[i]; s;",
    ),
    // A typed array with an **own** `length`: the ordinary slot cache serves
    // it (it is an own property), so the value is the own one.
    (
        "typed_array_own_length",
        "var t = new Int32Array(4); Object.defineProperty(t, 'length', { value: 7, configurable: true });",
        "var s = 0; for (var i = 0; i < 10; i++) s += t.length; s;",
    ),
    // A **patched** `%TypedArray%.prototype`: the getter is not the intrinsic
    // one any more, so the guard must not answer with the element count.
    (
        "typed_array_patched_prototype",
        "var t = new Int32Array(4); Object.defineProperty(Object.getPrototypeOf(Object.getPrototypeOf(t)), 'length', { get: function(){ return 99; }, configurable: true });",
        "var s = 0; for (var i = 0; i < 10; i++) s += t.length; s;",
    ),
    // A **detached** buffer: the length is 0.
    (
        "typed_array_detached",
        "var t = new Int32Array(4); t.buffer.transfer();",
        "var s = 0; for (var i = 0; i < 10; i++) s += t.length; s;",
    ),
    // A view that went **out of bounds** (its resizable buffer was shrunk
    // past its end): the length is 0.
    (
        "typed_array_out_of_bounds",
        "var buf = new ArrayBuffer(16, { maxByteLength: 32 }); var t = new Int32Array(buf); buf.resize(0);",
        "var s = 0; for (var i = 0; i < 10; i++) s += t.length; s;",
    ),
    // A **plain object** with the typed array's prototype: it is not a view,
    // so the getter throws (a TypeError) — the guard must not answer with an
    // element count.
    (
        "typed_array_prototype_on_a_plain_object",
        "var o = Object.setPrototypeOf({}, Object.getPrototypeOf(new Int32Array(1)));",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.length; s;",
    ),
    // A string target: not an object at all.
    (
        "string_target",
        "var s = 'abc';",
        "var n = 0; for (var i = 0; i < 10; i++) n += s.length; n;",
    ),
    // A number target: boxed by the helper.
    (
        "number_target",
        "",
        "var n = 0; for (var i = 0; i < 10; i++) n += (5).toString().length; n;",
    ),
    // A null target: the helper throws.
    ("null_target", "var o = null;", "o.x;"),
    // An undefined target: the helper throws.
    ("undefined_target", "var o = undefined;", "o.x;"),
    // A frozen object: the property is still a data property.
    (
        "frozen",
        "var o = Object.freeze({ x: 5 });",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.x; s;",
    ),
    // A sealed object.
    (
        "sealed",
        "var o = Object.seal({ x: 5 });",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.x; s;",
    ),
    // A property whose value changes under the guard: the slot is read
    // fresh every time.
    (
        "value_changes",
        "var o = { x: 0 };",
        "var s = 0; for (var i = 0; i < 10; i++) { o.x = i; s += o.x; } s;",
    ),
    // A property written through the helper and read through the guard.
    (
        "write_then_read",
        "var o = { x: 1 }; o.x = 99;",
        "var s = 0; for (var i = 0; i < 10; i++) s += o.x; s;",
    ),
    // A global variable read (the global object's own property).
    (
        "global_object",
        "globalThis.gv = 3;",
        "var s = 0; for (var i = 0; i < 10; i++) s += globalThis.gv; s;",
    ),
    // A property that is a `NaN` (the tag must survive the round trip).
    (
        "nan_value",
        "var o = { x: NaN };",
        "var s = 0; for (var i = 0; i < 10; i++) s += (o.x === o.x ? 0 : 1); s;",
    ),
    // A property that is `-0`.
    ("negative_zero", "var o = { x: -0 };", "1 / o.x;"),
    // A property that is `undefined` (the slot holds the undefined tag).
    (
        "undefined_value",
        "var o = { x: undefined };",
        "var s = 0; for (var i = 0; i < 10; i++) s += (o.x === undefined ? 1 : 0); s;",
    ),
    // A **variable reference** property: a global `var` lives in the global
    // object as a reference, not as a value — the guard must decline it.
    (
        "global_var",
        "var gv = 5;",
        "var s = 0; for (var i = 0; i < 10; i++) s += globalThis.gv; s;",
    ),
    // A **method call**: the lookup is a `get_field2` (the target stays on
    // the stack) — the same guard, another shape. The work lives in a named
    // function (a top-level `eval` is never compiled, so the guard would not
    // run at all).
    (
        "method_call",
        "var o = { m: function(){ return 7; } }; function work(){ var s = 0; for (var i = 0; i < 10; i++) s += o.m(); return s; } work();",
        "work();",
    ),
    // A method on the prototype: the guard declines it (the helper walks the
    // chain), and the call still works.
    (
        "method_on_the_prototype",
        "var p = { m: function(){ return 7; } }; var o = Object.create(p); function work(){ var s = 0; for (var i = 0; i < 10; i++) s += o.m(); return s; } work();",
        "work();",
    ),
    // A **lazy** property: a class field's initializer is instantiated on the
    // first read (the slot is a lazy initializer), and the slot becomes a
    // plain value afterwards.
    (
        "lazy_class_field",
        "class C { x = 1; } var c = new C();",
        "var s = 0; for (var i = 0; i < 10; i++) s += c.x; s;",
    ),
];

/// The scripts whose results must not change when the **write** is inlined:
/// `(label, setup, main)`. The setup defines the function and calls it once
/// (the warm-up: the JIT compiles it then), and the main calls it again — that
/// call runs natively, so the write really goes through the inline guard.
const PUT_CASES: &[(&str, &str, &str)] = &[
    // A writable own data property: the guard hits.
    (
        "own_data",
        "var o = { x: 1 }; function work(){ for (var i = 0; i < 10; i++) o.x = i; return o.x; } work();",
        "work();",
    ),
    // A **non-writable** own data property: the write is rejected (silently
    // in sloppy mode), so the guard must decline it.
    (
        "not_writable",
        "var o = {}; Object.defineProperty(o, 'x', { value: 1, writable: false }); function work(){ for (var i = 0; i < 10; i++) o.x = i; return o.x; } work();",
        "work();",
    ),
    // The same in strict mode: the write throws.
    (
        "not_writable_strict",
        "var o = {}; Object.defineProperty(o, 'x', { value: 1, writable: false }); function work(){ 'use strict'; o.x = 2; return o.x; } try { work(); } catch (e) {}",
        "work();",
    ),
    // An own accessor: the setter has to run.
    (
        "own_accessor",
        "var o = { set x(v){ this.got = v; } }; function work(){ for (var i = 0; i < 10; i++) o.x = i; return o.got; } work();",
        "work();",
    ),
    // A property on the prototype chain: not an own property, so the helper
    // walks the chain (and the write creates an own property).
    (
        "prototype_chain",
        "var p = { x: 1 }; var o = Object.create(p); function work(){ for (var i = 0; i < 10; i++) o.x = i; return o.x; } work();",
        "work();",
    ),
    // A deleted property: the slot is a tombstone, and the write recreates the
    // property.
    (
        "deleted",
        "var o = { x: 1 }; delete o.x; function work(){ for (var i = 0; i < 10; i++) o.x = i; return o.x; } work();",
        "work();",
    ),
    // An array's `length`: writing it resizes the array, so the helper owns
    // it.
    (
        "array_length",
        "var a = [1, 2, 3]; function work(){ for (var i = 0; i < 10; i++) a.length = i; return a.length; } work();",
        "work();",
    ),
    // A frozen object: the write is rejected.
    (
        "frozen",
        "var o = Object.freeze({ x: 1 }); function work(){ for (var i = 0; i < 10; i++) o.x = i; return o.x; } work();",
        "work();",
    ),
    // A store of a reference: the value needs the collector's write barrier.
    (
        "object_value",
        "var o = { x: 1 }; function work(){ for (var i = 0; i < 10; i++) o.x = { i: i }; return o.x.i; } work();",
        "work();",
    ),
    // A store of a string: a reference as well.
    (
        "string_value",
        "var o = { x: 1 }; function work(){ for (var i = 0; i < 10; i++) o.x = 'v' + i; return o.x; } work();",
        "work();",
    ),
    // A property added in the middle: the shape changes, the guard misses.
    (
        "grows",
        "var o = { x: 1 }; function work(){ for (var i = 0; i < 10; i++) { o.x = i; if (i === 5) o.y = 2; } return o.x; } work();",
        "work();",
    ),
    // Two shapes at one site: the guard hits the shape it saw last.
    (
        "polymorphic",
        "var a = { x: 1 }; var b = { x: 2, z: 3 }; function work(){ for (var i = 0; i < 10; i++) { (i % 2 ? a : b).x = i; } return a.x + ',' + b.x; } work();",
        "work();",
    ),
    // A null target: the helper throws.
    (
        "null_target",
        "var o = null; function work(){ o.x = 1; return 0; } try { work(); } catch (e) {}",
        "work();",
    ),
    // A write to a property that does not exist yet: the helper creates it.
    (
        "creates",
        "var o = {}; function work(){ for (var i = 0; i < 10; i++) o.x = i; return o.x; } work();",
        "work();",
    ),
    // The write and the read of the same property must see each other.
    (
        "write_then_read",
        "var o = { x: 1 }; function work(){ for (var i = 0; i < 10; i++) { o.x = o.x + 1; } return o.x; } work();",
        "work();",
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
    let name = EvalName::Name("jit-field.js".into());

    assert!(eval(&mut ctx, &name, setup).is_ok(), "setup threw");
    settle(&jit);
    let res = eval(&mut ctx, &name, main);
    let value = crate::common::observe(&mut ctx, res);
    (value, jit.metrics())
}

/// The property guard's counters of the **native** run: `(hits, misses)`.
///
/// `main` runs three times: the first run is interpreted (or partly
/// interpreted — the inline compiler may take over a loop mid-run), so only
/// the later ones exercise the guard. The counters of the last run are
/// reported, so the first run's leftovers (how much of it ran natively
/// depends on the compiler mode) do not leak into the observation, and the
/// inline cache is warm (the second run is the one that fills it).
///
/// The counters are the guard's own instrumentation, so they say *which* path
/// a read took — the results alone cannot (the helper would answer the same).
///
/// The work lives in a **named function** (defined by the setup): a top-level
/// `eval` builds a fresh function every time, and under production thresholds
/// such a function is never compiled (todo §2's "每次 eval 新建的顶层脚本永远
/// 编译不上"). A named function is compiled at its first call ✓, so the
/// later runs are native from their first instruction ✓.
fn guard_counters(setup: &str, main: &str) -> (u64, u64) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, Mode::Tier1.config()).expect("attach");
    let name = EvalName::Name("jit-field-guard.js".into());

    assert!(eval(&mut ctx, &name, setup).is_ok(), "setup threw");
    settle(&jit);
    assert!(eval(&mut ctx, &name, main).is_ok(), "main threw");
    settle(&jit);
    let before = jit.metrics();
    assert!(eval(&mut ctx, &name, main).is_ok(), "main threw");
    settle(&jit);
    let before = jit.metrics();
    // The third run: the function is compiled and its inline cache is warm.
    assert!(eval(&mut ctx, &name, main).is_ok(), "main threw");
    settle(&jit);

    let m = jit.metrics();
    assert!(m.native_entries > 0, "nothing ran natively: {m:?}");
    (
        m.property_guard_hits - before.property_guard_hits,
        m.property_guard_misses - before.property_guard_misses,
    )
}

/// The guard's decisions, observed through its counters: an **own data
/// property** is answered inline (a hit), while everything else goes back to
/// the helper (a miss) — a prototype-chain property, an accessor, a variable
/// reference, a tombstone.
///
/// The counters are the second (native) run's, so the numbers are exact: ten
/// reads, ten hits or ten misses.
#[test]
fn the_guard_serves_own_data_properties_and_declines_the_rest() {
    // An own data property: every read is answered inline.
    let (hits, misses) = guard_counters(
        "var o = { x: 1 }; function work(){ var s = 0; for (var i = 0; i < 10; i++) s += o.x; return s; }",
        "work();",
    );
    assert_eq!(hits, 10, "the guard did not serve the data property");
    assert_eq!(
        misses, 0,
        "the guard missed a data property: {misses} misses"
    );

    // An own accessor: the getter has to run, so the guard must decline.
    let (hits, misses) = guard_counters(
        "var o = {}; Object.defineProperty(o, 'x', { get: function(){ return 1; } }); function work(){ var s = 0; for (var i = 0; i < 10; i++) s += o.x; return s; }",
        "work();",
    );
    assert_eq!(hits, 0, "the guard served an accessor: {hits} hits");
    assert_eq!(misses, 10, "the accessor never reached the helper");

    // A prototype-chain property: not an own property, so the helper owns it.
    let (hits, misses) = guard_counters(
        "var p = { x: 1 }; var o = Object.create(p); function work(){ var s = 0; for (var i = 0; i < 10; i++) s += o.x; return s; }",
        "work();",
    );
    assert_eq!(
        hits, 0,
        "the guard served a prototype property: {hits} hits"
    );
    assert_eq!(
        misses, 10,
        "the prototype property never reached the helper"
    );

    // A deleted property: the slot is a tombstone, so the helper owns it.
    let (hits, misses) = guard_counters(
        "var o = { x: 1 }; delete o.x; function work(){ var s = 0; for (var i = 0; i < 10; i++) s += (o.x === undefined ? 0 : o.x); return s; }",
        "work();",
    );
    assert_eq!(hits, 0, "the guard served a tombstone: {hits} hits");
    assert_eq!(misses, 10, "the tombstone never reached the helper");
}

/// The inline guard must not change any result: every mode agrees with the
/// interpreter, bit for bit.
#[test]
fn the_inline_guard_keeps_every_result() {
    for (label, setup, main) in CASES {
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

/// The same for the **write**: the inline store must not change any result
/// either.
#[test]
fn the_inline_store_keeps_every_result() {
    for (label, setup, main) in PUT_CASES {
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

/// The write guard's decisions, observed through its counters: a **writable
/// own data property** is written inline (a hit), while everything else goes
/// back to the helper (a miss) — a non-writable property, an accessor, a
/// variable reference, a tombstone, `length`.
///
/// The scripts only *write*: the counters serve the reads too, so a read in
/// the same script would show up as a hit.
#[test]
fn the_write_guard_serves_writable_data_properties_and_declines_the_rest() {
    // A writable own data property: every write is answered inline.
    let (hits, misses) = guard_counters(
        "var o = { x: 1 }; function work(){ for (var i = 0; i < 10; i++) o.x = i; }",
        "work();",
    );
    assert_eq!(hits, 10, "the guard did not serve the write: {hits} hits");
    assert_eq!(misses, 0, "the guard missed a writable property: {misses}");

    // A non-writable own data property: the write is rejected, so the helper
    // has to own it.
    let (hits, misses) = guard_counters(
        "var o = {}; Object.defineProperty(o, 'x', { value: 1, writable: false }); function work(){ for (var i = 0; i < 10; i++) o.x = i; }",
        "work();",
    );
    assert_eq!(
        hits, 0,
        "the guard wrote a non-writable property: {hits} hits"
    );
    assert_eq!(misses, 10, "the write never reached the helper");

    // An own accessor: the setter has to run. The setter's body is empty on
    // purpose — a property write inside it would show up in the counters too.
    let (hits, misses) = guard_counters(
        "var o = { set x(v){} }; function work(){ for (var i = 0; i < 10; i++) o.x = i; }",
        "work();",
    );
    assert_eq!(hits, 0, "the guard bypassed a setter: {hits} hits");
    assert_eq!(misses, 10, "the setter never ran");

    // A property on the prototype chain that is **not writable**: the write
    // is rejected and no own property is created, so the shape never changes
    // and the guard never gets a slot to serve.
    let (hits, misses) = guard_counters(
        "var p = {}; Object.defineProperty(p, 'x', { value: 1, writable: false }); var o = Object.create(p); function work(){ for (var i = 0; i < 10; i++) o.x = i; }",
        "work();",
    );
    assert_eq!(hits, 0, "the guard wrote a prototype property: {hits} hits");
    assert_eq!(misses, 10, "the prototype write never reached the helper");

    // An array's `length`: writing it resizes the array.
    let (hits, misses) = guard_counters(
        "var a = [1, 2, 3]; function work(){ for (var i = 0; i < 10; i++) a.length = i; }",
        "work();",
    );
    assert_eq!(hits, 0, "the guard resized an array: {hits} hits");
    assert_eq!(misses, 10, "the length write never reached the helper");
}

/// The guard re-reads the shape on **every** access, so a property that turns
/// into an accessor stops being served: the reads before the change are hits,
/// the reads after it are misses (the helper runs the getter).
///
/// The change is made in the **third** run only (`phase > 2`): the setup's
/// object is shared by all the runs, so an accessor defined in an earlier
/// (the interpreted) run would still be there in the observed one.
#[test]
fn the_guard_re_checks_the_shape_on_every_access() {
    let (hits, misses) = guard_counters(
        "var o = { x: 1 }; var phase = 0; \
         function work(){ \
           phase++; \
           var s = 0; \
           for (var i = 0; i < 5; i++) s += o.x; \
           if (phase > 2) Object.defineProperty(o, 'x', { get: function(){ return 100; } }); \
           for (var i = 0; i < 5; i++) s += o.x; \
           return s; \
         }",
        "work();",
    );
    assert_eq!(hits, 5, "the guard served the accessor: {hits} hits");
    // Six misses: the five reads after the change, plus the
    // `Object.defineProperty` **method lookup** (a `get_field2` ✓ — the
    // method is on the prototype ✓, so the guard declines it ✓ and the
    // counters see it ✓).
    assert_eq!(
        misses, 6,
        "the accessor never reached the helper: {misses} misses"
    );
}

/// The typed array's `length` (P1-1's tail): the property lives on
/// `%TypedArray%.prototype`, so the ordinary slot cache cannot serve it — the
/// site records the shape that owns it in the chain instead, and the guard
/// answers the read inline.
#[test]
fn the_typed_array_length_is_served_inline() {
    let (hits, misses) = guard_counters(
        "var t = new Int32Array(4); \
         function work(){ var s = 0; for (var i = 0; i < 10; i++) s += t.length; return s; }",
        "work();",
    );
    assert_eq!(hits, 10, "the guard did not serve the typed array's length");
    assert_eq!(misses, 0, "the typed array's length reached the helper");
}

/// The typed array's `length` guard is only valid while the prototype chain
/// is untouched: a patched prototype must send the read back to the helper
/// (the getter's value is not the element count).
///
/// The **own** `length` case is the opposite: the ordinary slot cache serves
/// it (it *is* an own property) ✓ — the differential suite checks its value ✓.
#[test]
fn the_typed_array_length_guard_declines_a_changed_chain() {
    // A **patched** `%TypedArray%.prototype`: the shape that owns `length`
    // changes, so the recorded id no longer matches.
    let (hits, misses) = guard_counters(
        "var t = new Int32Array(4); \
         Object.defineProperty(Object.getPrototypeOf(Object.getPrototypeOf(t)), 'length', \
           { get: function(){ return 99; }, configurable: true }); \
         function work(){ var s = 0; for (var i = 0; i < 10; i++) s += t.length; return s; }",
        "work();",
    );
    assert_eq!(hits, 0, "the guard served a patched prototype: {hits} hits");
    assert_eq!(misses, 10, "the patched prototype never reached the helper");

    // A **patched** `%Int32Array%.prototype`: the owner of `length` moves one
    // level up, where the guard does not look.
    let (hits, misses) = guard_counters(
        "var t = new Int32Array(4); \
         Object.defineProperty(Object.getPrototypeOf(t), 'length', \
           { get: function(){ return 99; }, configurable: true }); \
         function work(){ var s = 0; for (var i = 0; i < 10; i++) s += t.length; return s; }",
        "work();",
    );
    assert_eq!(
        hits, 0,
        "the guard served a patched class prototype: {hits} hits"
    );
    assert_eq!(
        misses, 10,
        "the patched class prototype never reached the helper"
    );
}

/// The slots' mirror must follow every mutation that moves them: adding a
/// property reallocates the slots, deleting one leaves a tombstone, and the
/// compaction shifts the indices. A stale mirror would read the wrong slot.
///
/// The work lives in a named function, so it runs natively: the guard reads
/// the slots the mirror points at.
#[test]
fn the_slots_mirror_follows_the_mutations() {
    let setup = "\
        var o = { a: 1, b: 2 };\
        function work(){\
            var out = [];\
            out.push(o.a, o.b);\
            o.c = 3;\
            out.push(o.a, o.b, o.c);\
            delete o.b;\
            out.push(o.a, o.b);\
            return out.join(',');\
        }";
    let script = "work();";
    let (expected, _) = run(Mode::Interpreter, setup, script);
    assert!(expected.contains("1,2"), "unexpected: {expected}");
    for mode in MODES {
        let (got, _) = run(mode, setup, script);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
    }
}

/// The guard reads the slot, so a *write* through the helper and a read
/// through the guard must see each other.
#[test]
fn a_write_and_a_read_agree() {
    let setup = "\
        var o = { x: 1 };\
        function work(){\
            var s = 0;\
            for (var i = 0; i < 10; i++) { o.x = i; s += o.x; }\
            return s;\
        }";
    let script = "work();";
    let (expected, _) = run(Mode::Interpreter, setup, script);
    for mode in MODES {
        let (got, _) = run(mode, setup, script);
        assert_eq!(got, expected, "{mode:?} disagrees with the interpreter");
    }
}

/// The JIT's own counters: the guard is in the generated code, so the
/// property read must still be a native read (the function compiles and
/// runs).
#[test]
fn a_property_read_runs_natively() {
    let script = "function sum(o){ var s = 0; for (var i = 0; i < o.n; i++) s += o.x; return s; } sum({ n: 4, x: 2 });";
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
    let name = EvalName::Name("jit-field-native.js".into());
    assert!(eval(&mut ctx, &name, script).is_ok());
    settle(&jit);
    assert!(eval(&mut ctx, &name, "sum({ n: 8, x: 3 });").is_ok());
    settle(&jit);
    let m = jit.metrics();
    assert!(m.functions_compiled > 0, "nothing compiled: {m:?}");
    assert!(m.native_entries > 0, "nothing ran natively: {m:?}");
}
