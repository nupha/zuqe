// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! T10: the inline guard for global-variable reads and writes.
//!
//! A `var`/`function` binding is an ordinary property of `global_obj`; a
//! `let`/`const` binding lives on `global_var_obj`, which shadows it. The
//! generated code reads or writes an own data property straight out of the
//! slot, behind the shape + shadowing-shape + slot guard the helpers fill in.
//! Everything else — an unbound name, a TDZ slot, a read-only binding, an
//! accessor, a shape change — must fall back to the helper and produce
//! **bit-identical** results.
//!
//! The guard's own counters (`global_guard_hits`/`global_guard_misses`) say
//! which path an access took: an inline answer or a helper call.

#![cfg(feature = "jit")]

use crate::common::{Case, MODES, Mode, run_case, settle};

use zuqe::{Context, EvalFlags, EvalName, EvalType, JSResult, Jit, Runtime, ScopedContext};

fn eval(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

/// The guard's own counters over the second native run of `main`: the setup
/// defines the function, `main` calls it twice (the first call queues it, the
/// second runs it natively), and the counters are the delta between those two
/// calls. The bytecode's site cache survives between runs, so a data property
/// that was served once is served from the first access on.
fn guard_counters(setup: &str, main: &str) -> (u64, u64) {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, Mode::Tier1.config()).expect("attach");
    let name = EvalName::Name("jit-global-guard.js".into());

    assert!(eval(&mut ctx, &name, setup).is_ok(), "setup threw");
    settle(&jit);
    assert!(eval(&mut ctx, &name, main).is_ok(), "main threw");
    settle(&jit);
    // The first native run fills the sites (the helper records the slot), so
    // take the delta around the third run: every access is a hit.
    assert!(eval(&mut ctx, &name, main).is_ok(), "main threw");
    settle(&jit);
    let before = jit.metrics();
    assert!(eval(&mut ctx, &name, main).is_ok(), "main threw");
    settle(&jit);
    let m = jit.metrics();
    assert!(m.native_entries > 0, "nothing ran natively: {m:?}");
    (
        m.global_guard_hits - before.global_guard_hits,
        m.global_guard_misses - before.global_guard_misses,
    )
}

/// A `var` global is a `global_obj` property: every read and write in the
/// native run is answered inline.
#[test]
fn the_guard_serves_var_reads_and_writes() {
    let (hits, misses) = guard_counters(
        "var x = 1; function work(){ var s = 0; for (var i = 0; i < 10; i++){ s += x; x = i; } return s + x; }",
        "work();",
    );
    assert_eq!(hits, 21, "the guard did not serve every access: {hits}");
    assert_eq!(misses, 0, "a data property reached the helper: {misses}");
}

/// A `let` global lives on `global_var_obj`: the same guard serves it.
#[test]
fn the_guard_serves_lexical_reads_and_writes() {
    let (hits, misses) = guard_counters(
        "let x = 1; function work(){ var s = 0; for (var i = 0; i < 10; i++){ s += x; x = i; } return s + x; }",
        "work();",
    );
    assert_eq!(hits, 21, "the guard did not serve every access: {hits}");
    assert_eq!(misses, 0, "a data property reached the helper: {misses}");
}

/// Everything outside the guard's subset reaches the helper: an unbound name,
/// a read-only property, and an accessor.
#[test]
fn the_guard_declines_unbound_and_read_only_and_accessors() {
    // An unbound name: `get_var_undef` (`typeof`) must keep returning
    // `undefined` without a ReferenceError.
    let (_, misses) = guard_counters(
        "function work(){ var s = 0; for (var i = 0; i < 10; i++) s += typeof missing; return 0; }",
        "work();",
    );
    assert!(
        misses >= 1,
        "the guard served an unbound name: {misses} misses"
    );

    // A read-only property: the write is rejected, so it must not be inlined.
    let (_, misses) = guard_counters(
        "Object.defineProperty(globalThis, 'ro', { value: 1, writable: false }); \
         function work(){ for (var i = 0; i < 10; i++) ro = i; return ro; }",
        "work();",
    );
    assert!(
        misses >= 1,
        "the guard wrote a read-only property: {misses} misses"
    );

    // An accessor: the getter and setter must run.
    let (_, misses) = guard_counters(
        "var got = 0; Object.defineProperty(globalThis, 'acc', { \
           get: function(){ return 7; }, set: function(v){ got = v; }, configurable: true }); \
         function work(){ var s = 0; for (var i = 0; i < 10; i++){ s += acc; acc = i; } return s + got; }",
        "work();",
    );
    assert!(misses >= 1, "the guard served an accessor: {misses} misses");
}

/// The guard re-reads the shape on every access, so a global whose writability
/// changes under a hot loop stops being served inline — and, crucially, the
/// writes after the change are rejected instead of overwriting the value.
///
/// The global is **configurable** (created by a sloppy assignment), so each run
/// can make it writable again first: the writes before the flip are inline
/// hits, and the ones after it must not be (a stale cached flag would write
/// `x = 9` where the interpreter keeps `x = 5`).
#[test]
fn the_guard_re_checks_writability() {
    let (_, misses) = guard_counters(
        "function work(){ \
           Object.defineProperty(globalThis, 'x', { value: 1, writable: true, configurable: true }); \
           for (var i = 0; i < 10; i++){ \
             x = i; \
             if (i === 5) Object.defineProperty(globalThis, 'x', { writable: false }); \
           } \
           return x; \
         }",
        "work();",
    );
    assert!(misses >= 1, "the guard ignored the flag change: {misses}");
}

/// The differential half: every mode must produce the same value for the same
/// script. The cases cover the guard's hits and each of its declines, plus a
/// global added and deleted while the loop is hot (the shape moves under the
/// cached slot).
const CASES: &[Case] = &[
    Case {
        dim: "global",
        name: "var_read_write",
        setup: "var x = 1; function work(){ var s = 0; for (var i = 0; i < 100; i++){ s += x; x = i; } return s + x; } work();",
        warm: "",
        main: "work();",
        probe: "x;",
    },
    Case {
        dim: "global",
        name: "lexical_read_write",
        setup: "let x = 1; const c = 3; function work(){ var s = c; for (var i = 0; i < 100; i++){ s += x; x = i; } return s + x; } work();",
        warm: "",
        main: "work();",
        probe: "x;",
    },
    Case {
        dim: "global",
        name: "unbound_write",
        setup: "function work(){ for (var i = 0; i < 100; i++){ sloppy_global = i; } return sloppy_global; } work();",
        warm: "",
        main: "work();",
        probe: "sloppy_global;",
    },
    Case {
        dim: "global",
        name: "read_only_write",
        setup: "Object.defineProperty(globalThis, 'ro', { value: 1, writable: false }); \
                function work(){ for (var i = 0; i < 100; i++){ ro = i; } return ro; } work();",
        warm: "",
        main: "work();",
        probe: "ro;",
    },
    Case {
        dim: "global",
        name: "accessor",
        setup: "var got = 0; Object.defineProperty(globalThis, 'acc', { \
                  get: function(){ return 7; }, set: function(v){ got = v; }, configurable: true }); \
                function work(){ var s = 0; for (var i = 0; i < 100; i++){ s += acc; acc = i; } return s + got; } work();",
        warm: "",
        main: "work();",
        probe: "got;",
    },
    Case {
        dim: "global",
        name: "writable_flips",
        setup: "var x = 1; function work(){ \
                  for (var i = 0; i < 100; i++){ \
                    x = i; \
                    if (i === 50) Object.defineProperty(globalThis, 'x', { writable: false }); \
                  } \
                  return x; \
                } work();",
        warm: "",
        main: "work();",
        probe: "x;",
    },
    Case {
        // The configurable global makes every run writable again, so the flip
        // happens *inside* the observed native run.
        dim: "global",
        name: "writable_flips_native",
        setup: "function work(){ \
                  Object.defineProperty(globalThis, 'x', { value: 1, writable: true, configurable: true }); \
                  for (var i = 0; i < 100; i++){ \
                    x = i; \
                    if (i === 50) Object.defineProperty(globalThis, 'x', { writable: false }); \
                  } \
                  return x; \
                } work();",
        warm: "",
        main: "work();",
        probe: "x;",
    },
    Case {
        dim: "global",
        name: "shape_moves",
        setup: "var x = 1; function work(){ var s = 0; \
                  for (var i = 0; i < 100; i++){ \
                    s += x; \
                    if (i === 50){ globalThis.added = 1; delete globalThis.added; } \
                  } \
                  return s + x; \
                } work();",
        warm: "",
        main: "work();",
        probe: "x;",
    },
    Case {
        // A `const` write throws even in sloppy mode; the guard's cached flags
        // say "not writable", so every write must take the helper.
        dim: "global",
        name: "const_write_throws",
        setup: "const c = 1; function work(){ var n = 0; \
                  for (var i = 0; i < 100; i++){ \
                    try { c = i; } catch (e) { if (e instanceof TypeError) n++; } \
                  } \
                  return c * 1000 + n; \
                } work();",
        warm: "",
        main: "work();",
        probe: "c;",
    },
    Case {
        // A strict write to an unbound name must not create the property.
        dim: "global",
        name: "strict_unbound_write",
        setup: "function work(){ 'use strict'; var n = 0; \
                  for (var i = 0; i < 100; i++){ \
                    try { sloppy_name = i; } catch (e) { n++; } \
                  } \
                  return n + ':' + (typeof sloppy_name); \
                } work();",
        warm: "",
        main: "work();",
        probe: "typeof sloppy_name;",
    },
    Case {
        // Deleting the cached binding leaves a tombstone (debug: a compacted
        // shape); the following writes recreate it through the helper.
        dim: "global",
        name: "delete_and_readd",
        setup: "d = 1; function work(){ var s = 0; \
                  for (var i = 0; i < 100; i++){ \
                    d = i; \
                    if (i === 50) delete globalThis.d; \
                    s += (typeof d === 'undefined') ? 1000 : d; \
                  } \
                  return s; \
                } work();",
        warm: "",
        main: "work();",
        probe: "d;",
    },
    Case {
        // A cached data property turned into an accessor inside the observed
        // native run: the guard must re-check the shape and the slot tag.
        dim: "global",
        name: "becomes_accessor_native",
        setup: "function work(){ \
                  Object.defineProperty(globalThis, 'x', { value: 1, writable: true, configurable: true }); \
                  var s = 0; \
                  for (var i = 0; i < 100; i++){ \
                    s += x; \
                    if (i === 50) Object.defineProperty(globalThis, 'x', { get: function(){ return 42; } }); \
                  } \
                  return s; \
                } work();",
        warm: "",
        main: "work();",
        probe: "x;",
    },
    Case {
        // Two global sites in one function: each has its own cache entry, and
        // the read/write of the same name share one.
        dim: "global",
        name: "two_sites",
        setup: "var a = 1, b = 2; function work(){ var t = 0; \
                  for (var i = 0; i < 100; i++){ t = a; a = b; b = t; } \
                  return a * 1000 + b; \
                } work();",
        warm: "",
        main: "work();",
        probe: "a * 10 + b;",
    },
    Case {
        // An object stored into a global in a native loop, between two rounds
        // of allocations (the guard's write barrier).
        dim: "global",
        name: "object_value_barrier",
        setup: "x = 1; function work(){ \
                  for (var i = 0; i < 5000; i++){ var before = { i: i }; } \
                  x = { tag: 'kept' }; \
                  for (var i = 0; i < 5000; i++){ var after = { i: i, s: 'x' + i }; } \
                  return x.tag; \
                } work();",
        warm: "",
        main: "work();",
        probe: "x.tag;",
    },
    Case {
        dim: "global",
        name: "toplevel_osr",
        setup: "function osrWarm(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } osrWarm(1);",
        warm: "",
        main: "var s = 1; for (var i = 1; i < 20000; i++) s = (s + i) | 0; s;",
        probe: "s;",
    },
    Case {
        // The lexical half of the OSR shape: `s` lives on `global_var_obj`.
        dim: "global",
        name: "toplevel_osr_lexical",
        setup: "function osrWarm(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } osrWarm(1);",
        warm: "",
        main: "let s = 1; for (let i = 1; i < 20000; i++) s = (s + i) | 0; s;",
        probe: "s;",
    },
];

/// All four modes agree, value and side effect included. The observed run's
/// metrics are returned by [`run_case`], but the assertion is the value: a
/// wrong guard is a wrong read or write, not a counter.
#[test]
fn the_global_guard_is_bit_exact_across_modes() {
    for case in CASES {
        let (expected_main, expected_probe, _) = run_case(Mode::Interpreter, case);
        for mode in MODES {
            let (main, probe, m) = run_case(mode, case);
            assert_eq!(
                main,
                expected_main,
                "{} {}: main differs in {} ({m:?})",
                case.dim,
                case.name,
                mode.label()
            );
            assert_eq!(
                probe,
                expected_probe,
                "{} {}: probe differs in {}",
                case.dim,
                case.name,
                mode.label()
            );
        }
    }
}

/// The top-level loop reaches native code by back-edge OSR; its `var`s are
/// globals, so the guard is what makes the OSR loop pay off. The lexical shape
/// (`let s`) goes through `global_var_obj` instead.
#[test]
fn the_top_level_osr_loop_uses_the_guard() {
    // The loop must outlive the compile: install only lands on a back edge, so
    // a loop shorter than the worker's compile latency (a few ms) ends still
    // interpreted and `native_entries` never moves.
    let sources = [
        "var s = 1; for (var i = 1; i < 2000000; i++) s = (s + i) | 0; s;",
        "let s = 1; for (let i = 1; i < 2000000; i++) s = (s + i) | 0; s;",
    ];
    for src in sources {
        let mut rt = Runtime::create().expect("runtime");
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .expect("context");
        let jit = Jit::attach_to(&rt, Mode::Tier1.config()).expect("attach");
        let name = EvalName::Name("jit-global-osr.js".into());
        // The first eval already reaches native code by back-edge OSR (the
        // loose threshold asks for the compile at the first back edge), so one
        // eval is enough to observe the guard.
        let before = jit.metrics();
        assert!(eval(&mut ctx, &name, src).is_ok());
        settle(&jit);
        let m = jit.metrics();
        let hits = m.global_guard_hits - before.global_guard_hits;
        let misses = m.global_guard_misses - before.global_guard_misses;
        assert!(
            m.native_entries > before.native_entries,
            "OSR did not run the loop natively: {src}"
        );
        assert!(
            hits > 10_000,
            "the top-level loop barely used the guard ({hits}): {src}"
        );
        // One miss per site (the first access fills the cache); the read and
        // write of `s` share the function's one global-var-ref index.
        assert!(
            misses <= 2,
            "the top-level guard kept missing ({misses}): {src}"
        );
    }
}

/// A global `let` added **after** the property was cached is legal (the
/// property is configurable) and shadows it. Adding the binding mutates
/// `global_var_obj`'s shape in place, so the shape id must move — otherwise the
/// guard keeps reading the shadowed property.
#[test]
fn a_later_lexical_binding_shadows_the_cached_property() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, Mode::Tier1.config()).expect("attach");
    let name = EvalName::Name("jit-global-shadow.js".into());

    assert!(
        eval(
            &mut ctx,
            &name,
            "let a = 0; x = 1; function get(){ return x; } get();"
        )
        .is_ok()
    );
    settle(&jit);
    // The first native call fills the guard's cache for the global property.
    assert!(eval(&mut ctx, &name, "get();").is_ok());
    settle(&jit);
    let before = jit.metrics();
    assert!(eval(&mut ctx, &name, "get();").is_ok());
    settle(&jit);
    let m = jit.metrics();
    assert!(
        m.global_guard_hits > before.global_guard_hits,
        "the global property was not cached"
    );
    // A new global lexical binding shadows it.
    assert!(eval(&mut ctx, &name, "let x = 2;").is_ok());
    let before = jit.metrics();
    let got = eval(&mut ctx, &name, "get();").expect("get");
    settle(&jit);
    let m = jit.metrics();
    assert!(
        m.global_guard_misses > before.global_guard_misses,
        "the guard served a shadowed property: {} hits",
        m.global_guard_hits - before.global_guard_hits
    );
    assert_eq!(
        got.as_int(),
        Some(2),
        "the guard read the shadowed property instead of the lexical binding"
    );
}

/// A global lexical binding on `global_var_obj` before its initializer runs is
/// in its TDZ: both accesses must call the helper (which raises the
/// ReferenceError), never read the `Uninitialized` sentinel out of the slot.
///
/// The first TDZ access fills the site's cache with the uninitialized slot; the
/// second one is served by the guard's own TDZ check.
#[test]
fn a_tdz_read_throws_instead_of_reading_the_slot() {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, Mode::Tier1.config()).expect("attach");
    let name = EvalName::Name("jit-global-tdz.js".into());

    // Compile `get` while `t` is unbound.
    assert!(
        eval(
            &mut ctx,
            &name,
            "function get(){ return t; } try { get(); } catch (e) {}"
        )
        .is_ok()
    );
    settle(&jit);
    let before = jit.metrics();
    // `t` is in its TDZ while the initializer calls `get`; both calls throw.
    let src = "let t = (function(){ var n = 0; \
               try { get(); } catch (e) { if (e instanceof ReferenceError) n++; } \
               try { get(); } catch (e) { if (e instanceof ReferenceError) n++; } \
               return n; })(); t;";
    let got = eval(&mut ctx, &name, src).expect("initializer");
    assert_eq!(got.as_int(), Some(2), "the TDZ reads did not throw");
    let m = jit.metrics();
    assert!(
        m.global_guard_misses > before.global_guard_misses,
        "the TDZ reads did not go through the helper"
    );
    // Once initialized, the same site reads the value.
    assert_eq!(eval(&mut ctx, &name, "get();").unwrap().as_int(), Some(2));
}

/// An unbound global: `get_var` raises a ReferenceError, `get_var_undef`
/// (`typeof`) returns `undefined` — both from the helper, after the guard
/// declines the empty cache.
#[test]
fn unbound_global_read_errors_agree_across_modes() {
    let case = Case {
        dim: "global",
        name: "unbound_errors",
        setup: "function work(){ \
                  try { return missing_name; } \
                  catch (e) { return (e instanceof ReferenceError) + ':' + (typeof missing_name); } \
                } work();",
        warm: "",
        main: "work();",
        probe: "typeof missing_name;",
    };
    let (expected_main, expected_probe, _) = run_case(Mode::Interpreter, &case);
    for mode in MODES {
        let (main, probe, _) = run_case(mode, &case);
        assert_eq!(main, expected_main, "{} differs", mode.label());
        assert_eq!(probe, expected_probe, "{} probe differs", mode.label());
    }
}
