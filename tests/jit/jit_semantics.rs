// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! M2b-3: semantic stress suite. Recursion/TCO, TDZ, try/catch/finally shapes,
//! classes, closures, Proxy/`valueOf` re-entry and GC pressure, each run under
//! every tier policy and compared against the interpreter. Every feature the
//! JIT does not support must state that explicitly (`Expect::Interpreted`):
//! the function stays in the interpreter and the result must still match.

#![cfg(feature = "jit")]

use crate::common::{Case, Mode, run_case_polled};

/// Whether the case's own function is expected to run native code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// Every function the warm-up queues compiles and native code runs.
    Compiles,
    /// The frontend refuses at least one function the warm-up calls, because
    /// the feature is `Reject` in the policy table.
    Interpreted,
}

struct SemCase {
    name: &'static str,
    expect: Expect,
    case: Case,
}

macro_rules! sem_cases {
    ($($name:ident => $expect:ident = $setup:literal, $warm:literal, $main:literal, $probe:literal;)*) => {
        &[$(SemCase {
            name: stringify!($name),
            expect: Expect::$expect,
            case: Case {
                dim: "Semantics",
                name: stringify!($name),
                setup: $setup,
                warm: $warm,
                main: $main,
                probe: $probe,
            },
        },)*]
    };
}

const CASES: &[SemCase] = sem_cases! {
    // ── recursion / tail calls ──
    recur_fib => Compiles =
        "var __seed = 0; function fib(n){ var __z = 0; if (n < 2) return n; return fib(n - 1) + fib(n - 2); }",
        "fib(12)",
        "fib(18)",
        "'fib:' + fib(10)";
    recur_mutual => Compiles =
        "var __seed = 0; function ev(n){ var __z = 0; if (n === 0) return true; return od(n - 1); } function od(n){ var __z = 0; if (n === 0) return false; return ev(n - 1); }",
        "ev(20)",
        "ev(101)",
        "'ev:' + ev(10)";
    // Proper tail calls are rewritten only in strict mode, and `tail_call` is
    // `Reject`: the function must stay interpreted and still agree.
    recur_tail_rejected => Interpreted =
        "var __seed = 0; function f(n){ \"use strict\"; var __z = 0; if (n === 0) return 0; return f(n - 1); }",
        "f(50)",
        "f(500)",
        "'tc:' + f(10)";
    recur_stack_overflow => Compiles =
        "var __seed = 0; function deep(n){ var __z = 0; return deep(n + 1); } function guard(){ var __z = 0; try { deep(0); return 'no-overflow'; } catch (e) { return 'caught:' + e.name; } }",
        "guard()",
        "guard()",
        "'ovf:' + guard()";

    // ── temporal dead zone ──
    tdz_read_uninitialised => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var r = 0; { let a; try { r = r + a; } catch (e) { r = 100 + (e.name === 'ReferenceError' ? 0 : 1); } a = n + 1; r = r + a; } return r; }",
        "f(1)",
        "f(7)",
        "'tdz:' + f(7)";
    tdz_const_assign => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var r = 0; { const c = n; try { c = c + 1; } catch (e) { r = 1; } r = r + c; } return r; }",
        "f(3)",
        "f(9)",
        "'tc:' + f(9)";
    // The TDZ error itself is the interpreter's (epic F: `get_loc_check` lowers
    // to a guard whose slow path resumes at the same pc, so the message that
    // names the binding is raised by the interpreter verbatim). The pending
    // `5` is cache-resident when the guard flushes and exits, so this also
    // exercises materializing the operand stack before that exit.
    tdz_message_and_pending_operand => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var r = ''; { let a; try { r = (5 + a) + ('x' + a); } catch (e) { r = e.name + ':' + e.message + ':' + n; } } return r; }",
        "f(1)",
        "f(7)",
        "'tdzm:' + f(5)";
    tdz_block_scope => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var out = 0; for (var i = 0; i < 3; i++) { let v = n + i; out = out + v; } return out; }",
        "f(1)",
        "f(5)",
        "'blk:' + f(5)";

    // ── try / catch / finally ──
    finally_return_overrides => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; try { return 'try'; } finally { n = n + 1; } }",
        "f(1)",
        "f(7)",
        "'fr:' + f(7)";
    finally_throw_after_catch => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; try { try { throw 'inner'; } catch (e) { throw 're:' + e; } finally { N = N + 1; } } catch (e) { return e + '|' + N; } } var N = 0;",
        "f(0)",
        "f(0)",
        "'ft:' + f(0)";
    exception_through_finally => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var log = ''; try { try { if (n === 0) throw new Error('x'); log = 'ok'; } finally { log = log + '|fin'; } } catch (e) { log = log + '|caught:' + e.message; } return log; }",
        "f(1)",
        "f(0)",
        "'etf:' + f(0) + ',' + f(1)";
    nested_finally_return => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; try { try { if (n === 0) throw 1; return 'no'; } finally { n = n + 10; } } finally { n = n + 100; } }",
        "f(1)",
        "f(0)",
        "'nf:' + f(0) + ',' + f(1)";
    rethrow_from_catch => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; try { throw new TypeError('a'); } catch (e) { if (n === 0) throw e; return e.name + ':' + e.message; } }",
        "f(1)",
        "f(0)",
        "'re:' + f(1) + ',' + f(0)";
    finally_in_loop_return => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var s = 0; for (var i = 0; i < n; i++) { try { if (i === 3) throw 'skip'; s = s + i; } finally { s = s + 100; } } return s; }",
        "f(2)",
        "f(9)",
        "'fil:' + f(9)";

    // ── classes (all class machinery is `Reject`) ──
    class_constructor_field => Interpreted =
        "var __seed = 0; class C { constructor(x){ this.v = x; } get(){ return this.v; } } function f(){ var __z = 0; var c = new C(4); return c.get(); }",
        "f()",
        "f()",
        "'cc:' + f()";
    class_inherit_super => Interpreted =
        "var __seed = 0; class A { constructor(){ this.p = 1; } m(){ return this.p; } } class B extends A { m(){ return super.m() + 1; } } function f(){ var __z = 0; return new B().m(); }",
        "f()",
        "f()",
        "'ci:' + f()";
    class_private_fields => Interpreted =
        "var __seed = 0; class C { #s = 5; get(){ return this.#s; } bump(){ this.#s = this.#s + 1; return this.#s; } } function f(){ var __z = 0; var c = new C(); c.bump(); return c.get(); }",
        "f()",
        "f()",
        "'cp:' + f()";

    // ── closures ──
    closure_counter => Compiles =
        "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var m = n; var r = apply(function(){ var __u = 0; m = m + 1; return m; }); return r + n; }",
        "f(1)",
        "f(5)",
        "'cl:' + f(5)";
    closure_captured_block_let => Interpreted =
        "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var out = 0; { let x = n + 1; out = apply(function(){ var __u = 0; return x; }); } return out; }",
        "f(2)",
        "f(8)",
        "'cb:' + f(8)";
    closure_loop_binding => Compiles =
        "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var out = 0; for (var i = 0; i < 4; i++) { out = apply(function(){ var __u = 0; return i; }); } return out + n; }",
        "f(0)",
        "f(3)",
        "'clb:' + f(3)";

    // A local captured by a closure: reads after the closure ran must come from
    // the frame (the `LocalCache` is off for a function that creates closures),
    // including after the native→interpreter→native re-entry the call forces.
    closure_mutation_is_visible => Compiles =
        "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var m = n; var r = apply(function(){ var __u = 0; m = m + 2; return m; }); var a = m; var r2 = apply(function(){ var __u = 0; m = m + 3; return m; }); var b = m; return r + ':' + a + ':' + b + ':' + r2; }",
        "f(1)",
        "f(10)",
        "'cm:' + f(4)";

    // ── Proxy / coercion re-entry ──
    proxy_get_trap => Compiles =
        "var __seed = 0; var H = { get: function(t, k){ return 'trap:' + k; } }; function f(o){ var __z = 0; var p = new Proxy(o, H); return p.missing; }",
        "f({})",
        "f({})",
        "'px:' + f({})";
    valueof_reentry_stress => Compiles =
        "var __seed = 0; var LOG = ''; var V = { valueOf: function(){ LOG = LOG + 'v'; return 3; }, toString: function(){ LOG = LOG + 't'; return 'S'; } }; function co(n){ var __z = 0; var s = 0; for (var i = 0; i < n; i++) s = s + V; return s; }",
        "co(3)",
        "co(50)",
        "'vo:' + co(2) + ',' + LOG";
    compare_reentry_throw => Compiles =
        "var __seed = 0; var E = { valueOf: function(){ throw new RangeError('nope'); } }; function cmp(x, y){ var __z = 0; try { return x < y; } catch (e) { return e.name; } }",
        "cmp(1, 2)",
        "cmp(E, 2)",
        "'cr:' + cmp(E, 5)";

    // ── GC pressure (the JIT pins queued bytecode and scans live slots) ──
    gc_string_pressure => Compiles =
        "var __seed = 0; function build(n){ var __z = 0; var last = ''; for (var i = 0; i < n; i++) { var s = 'v' + i; last = s; } return last.length; }",
        "build(10)",
        "build(40000)",
        "'gs:' + build(100)";
    gc_object_churn => Compiles =
        "var __seed = 0; function churn(n){ var __z = 0; var t = 0; for (var i = 0; i < n; i++) { var o = [i, i * 2]; t = t + o[0] + o[1]; } return t; }",
        "churn(10)",
        "churn(30000)",
        "'go:' + churn(100)";
    gc_retained_graph => Compiles =
        "var __seed = 0; function apply(g){ var __z = 0; return g(); } function keep(n){ var __z = 0; var a = []; for (var i = 0; i < n; i++) { var b = [i, i * 2]; a.push(b); } return a.length; } function sum(a){ var __z = 0; var s = 0; for (var i = 0; i < a.length; i++) s = s + a[i][0] + a[i][1]; return s; } var KEEP = keep(4);",
        "keep(8) + sum(KEEP)",
        "keep(20) + sum(KEEP)",
        "'gr:' + sum(KEEP) + ',' + KEEP.length";
    gc_shape_churn => Compiles =
        "var __seed = 0; var BASE = { k0: 0 }; function shapes(n){ var __z = 0; var t = 0; for (var i = 0; i < n; i++) { BASE['k' + (i % 7)] = i; t = t + i; } return t; }",
        "shapes(10)",
        "shapes(20000)",
        "'gsc:' + shapes(50)";

    // ── polymorphic inline cache (epic F3): one site, five shapes, so the
    //    helper-side IC must hold several ways and evict past `IC_WAYS` ──
    ic_polymorphic_shapes => Compiles =
        "var __seed = 0; var PO = [ {x: 1, y: 2}, {x: 3, y: 4, a: 1}, {x: 5, y: 6, b: 2}, {x: 7, y: 8, c: 3}, {x: 9, y: 10, d: 4} ]; function f(n){ var __z = 0; var s = 0; for (var i = 0; i < n; i++) { var o = PO[i % 5]; s = ((s + o.x) * 2 + o.y) & 0xffff; o.x = (o.x + 1) & 1023; } return s; }",
        "f(50)",
        "f(500)",
        "'ic:' + f(7) + ',' + PO[0].x + ',' + PO[4].y";

    // ── cross-block operand stack residency (epic F4 step 2): short-circuit
    //    and ternary joins leave a merged slot on the operand stack, which the
    //    join block must read exactly like the interpreter does. The values
    //    crossing each label are locals; the literal-arm shape (`x || 5`) is
    //    the `coalesce_` case below.
    carry_join_slots => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var t = 0, m = 1; for (var i = 0; i < n; i++) { var x = (i || m) + (i && m) + (i ? m : t); t = (t + x) | 0; } return t + (n || m) + (n && m); }",
        "f(4)",
        "f(64)",
        "'cj:' + f(0) + ',' + f(9)";

    // ── short-circuit / `??` joins with literal arms ──
    // Until the verifier widened operand kinds at a join, every one of these
    // rejected the whole function (`Tagged` local vs `Int32` literal). The
    // values cover both falsy-but-not-nullish (`0`) and nullish (`null`,
    // `undefined`) operands, plus a float and a string, so the inline
    // `is_undefined_or_null` guard and the merged slot are tag-exact.
    coalesce_literal_arms => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var a = n ?? 7; var b = (n ?? 8) ?? 9; var c = (n || 5) + (n && 9) + (n ? 11 : 13); return a + b + c; }",
        "f(2)",
        "String(f(0)) + '/' + String(f(2)) + '/' + String(f(null)) + '/' + String(f(undefined)) + '/' + String(f(NaN)) + '/' + String(f(2.5)) + '/[' + f('') + ']'",
        "'coal:' + f(3) + ',' + f(0)";

    // ── frame-entry special objects (epic F §6.12) ──
    // Sloppy and strict `arguments`: the mapped object aliases the formals in
    // both directions, the strict one is a plain copy. Its `VarRef`s point at
    // the frame's argument slots (the verifier's `MappedArguments` rejection is
    // gone because the JIT helper calls the interpreter's own builder).
    arguments_mapped_and_strict => Compiles =
        "var __seed = 0; function m(a){ var __z = 0; arguments[0] = 9; var r = a; a = 7; return r + ':' + arguments[0]; } function s(a){ \"use strict\"; var __z = 0; arguments[0] = 9; var r = a; a = 7; return r + ':' + arguments[0]; }",
        "m(1)",
        "m(1) + ',' + s(1)",
        "'args:' + m(2) + ',' + s(2)";
    // `push_this` inline for strict, the substitution helper for sloppy.
    this_is_raw_when_strict => Compiles =
        "var __seed = 0; function s(){ \"use strict\"; var __z = 0; return typeof this; } function l(){ var __z = 0; return typeof this; }",
        "l()",
        "s() + ',' + l()",
        "'this:' + s() + ',' + l()";
    // `new.target` inside a constructor — undefined on a plain call.
    newtarget_in_constructor => Compiles =
        "var __seed = 0; function Ctor(){ var __z = 0; var nt = new.target; return nt === undefined ? 'plain' : 'new'; } function f(k){ var __z = 0; return k ? new Ctor() : Ctor(); }",
        "f(1)",
        "f(1) + ',' + f(0)",
        "'nt:' + f(1) + ',' + f(0)";

    // ── control flow shapes the verifier must survive ──
    switch_and_labels => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var r = 0; switch (n % 4) { case 0: r = 10; break; case 1: r = 20; break; default: r = 30; } outer: for (var i = 0; i < 5; i++) { if (i === 2) continue outer; if (i === 4) break outer; r = r + i; } return r; }",
        "f(0)",
        "f(5)",
        "'sw:' + f(5)";
    generators_rejected => Interpreted =
        "var __seed = 0; function* g(){ var __z = 0; yield 1; yield 2; } function f(){ var __z = 0; var it = g(); return it.next().value + it.next().value; }",
        "f()",
        "f()",
        "'gn:' + f()";
};

/// Feature areas the suite must cover, checked so the list cannot silently
/// shrink to cases that happen to pass.
const FEATURES: &[&str] = &[
    "recur_",
    "tdz_",
    "finally_",
    "exception_",
    "nested_",
    "rethrow_",
    "class_",
    "closure_",
    "proxy_",
    "valueof_",
    "compare_",
    "gc_",
    "switch_",
    "generators_",
    "ic_",
    "carry_",
    "coalesce_",
    "arguments_",
    "this_",
    "newtarget_",
];

#[test]
fn every_semantic_feature_has_a_case() {
    for feature in FEATURES {
        assert!(
            CASES.iter().any(|c| c.name.starts_with(feature)),
            "no case covers the {feature} feature area"
        );
    }
    let compiled = CASES
        .iter()
        .filter(|c| c.expect == Expect::Compiles)
        .count();
    let interpreted = CASES.len() - compiled;
    println!(
        "semantics: {} cases ({compiled} compile, {interpreted} stay interpreted)",
        CASES.len()
    );
    assert!(interpreted > 0, "the reject-side features are not asserted");
}

#[test]
fn every_case_agrees_with_the_interpreter() {
    let mut native = 0u64;
    for sem_case in CASES {
        let base = run_case_polled(Mode::Interpreter, &sem_case.case);
        assert_eq!(
            base.after_warm.functions_compiled, 0,
            "{}: interpreter policy compiled something",
            sem_case.name
        );

        for mode in [Mode::Tier1, Mode::Tier2, Mode::Automatic] {
            let run = run_case_polled(mode, &sem_case.case);
            assert_eq!(
                run.main,
                base.main,
                "{}: main differs from the interpreter under {}",
                sem_case.name,
                mode.label()
            );
            assert_eq!(
                run.probe,
                base.probe,
                "{}: side effects differ from the interpreter under {}",
                sem_case.name,
                mode.label()
            );

            let m = run.after_warm;
            let queued = m.functions_queued - run.after_definition.functions_queued;
            let compiled = m.functions_compiled - run.after_definition.functions_compiled;
            let rejected = m.functions_rejected - run.after_definition.functions_rejected;
            assert!(
                queued >= 2,
                "{}: nothing queued under {} (metrics: {m:?})",
                sem_case.name,
                mode.label()
            );
            assert_eq!(
                compiled + rejected,
                queued,
                "{}: a queued function was neither compiled nor rejected under {}",
                sem_case.name,
                mode.label()
            );

            match sem_case.expect {
                Expect::Compiles => {
                    assert_eq!(
                        rejected,
                        0,
                        "{}: {} rejected a supported feature (metrics: {m:?})",
                        sem_case.name,
                        mode.label()
                    );
                    assert_eq!(
                        compiled,
                        queued,
                        "{}: not every function compiled under {}",
                        sem_case.name,
                        mode.label()
                    );
                    assert!(
                        run.after_main.native_entries > 0,
                        "{}: no native code ran under {}",
                        sem_case.name,
                        mode.label()
                    );
                    native += run.after_main.native_entries;
                }
                Expect::Interpreted => {
                    assert!(
                        rejected >= 1,
                        "{}: {} compiled a feature that must stay interpreted (metrics: {m:?})",
                        sem_case.name,
                        mode.label()
                    );
                }
            }
        }
    }
    assert!(native > 0, "the suite never ran native code");
    println!("semantics: {native} native entries across the compiling cases");
}
