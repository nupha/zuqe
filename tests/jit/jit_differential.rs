// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! M2b-1: multi-mode differential suite. The same script runs under every tier
//! policy and must produce the same value (tag-exact), the same exception text
//! and the same global side effects. `tier2` exercises the feedback-driven
//! specialization of epic F1/F2.

#![cfg(feature = "jit")]

use crate::common::{Case, MODES, Mode, run_case};

/// The five dimensions of plan §8.2.
const DIMENSIONS: [&str; 5] = [
    "Normal",
    "NumericTagEdge",
    "ExceptionOrNonthrow",
    "OwnershipGc",
    "CoercionReentrancy",
];

/// One dimension's worth of cases.
macro_rules! dim_cases {
    ($dim:expr, [ $($name:ident: $setup:expr, $main:expr, $probe:expr;)* ]) => {
        &[$(Case {
            dim: $dim,
            name: stringify!($name),
            setup: $setup,
            warm: "",
            main: $main,
            probe: $probe,
        },)*]
    };
}

const NORMAL: &[Case] = dim_cases!("Normal", [
    sum_loop:
        "function sum(n){ var s = 0; for (var i = 0; i < n; i++) s += i * 2 - 1; return s; } sum(1);",
        "sum(2000)",
        "'sum:' + sum(3)";
    fib_iter:
        "function fib(n){ var a = 0, b = 1; for (var i = 0; i < n; i++) { var t = a + b; a = b; b = t; } return b; } fib(1);",
        "fib(90)",
        "'fib:' + fib(10)";
    closure_counter:
        "function mk(){ var n = 0; return function(){ return ++n; }; } var C = mk(); C();",
        "C() + C()",
        "'cl:' + C()";
    array_sum:
        "function arr(n){ var a = [1, 2, 3, 4, 5]; var s = 0; for (var i = 0; i < a.length; i++) s += a[i] * i; return s; } arr(1);",
        "arr(500)",
        "'arr:' + arr(3)";
    props_ic:
        "var O = { x: 1, y: 2 }; function pk(n){ var s = 0; for (var i = 0; i < n; i++) { s += O.x; O.x = (O.x * 3) & 1023; } return s; } pk(1);",
        "pk(300)",
        "'prop:' + O.x + ',' + pk(2)";
    call_reentry:
        "function a(n){ return n <= 0 ? 0 : b(n - 1) + 1; } function b(n){ return n <= 0 ? 0 : a(n - 1) + 2; } a(1);",
        "a(60)",
        "'re:' + a(5)";
    // Epic F4: pushes stay in registers until something observes the frame —
    // here the string-concat helper, which reads the operand stack.
    stack_cache_flushes_for_helpers:
        "function mix(n){ var a = n + 1, b = n * 2, c = n - 3; var s = '' + a + ':' + b; return s + '|' + (a * b + c); } mix(1);",
        "mix(50)",
        "'cc:' + mix(3)";
    // ... and the other flush point: a branch terminator, whose successor
    // block starts with an empty cache and reloads from the frame.
    stack_cache_flushes_at_branches:
        "function branchy(n){ var a = n + 1, b = n * 2; var t = 0; if (a > b) { t = a - b; } else { t = b - a; } for (var i = 0; i < 3; i++) { t = t + i * a; } return t; } branchy(1);",
        "branchy(300)",
        "'br:' + branchy(4)";
    // Epic F4 step 2: values cross block edges as parameters — short-circuit
    // operators leave a merged slot on the stack, which the join block reads
    // in a register instead of reloading from the frame.
    stack_cache_carries_slots_across_blocks:
        "function carry(n){ var a = n + 1, b = n * 2, c = n - 3; var t = 0; for (var i = 0; i < n; i++) { var x = (i || a) + (i && b) + (i ? a : b) * (c || i); t = (t + x) | 0; } return t + (a || b) + (c && n); } carry(1);",
        "carry(64)",
        "'cc2:' + carry(7)";
    // ... through several nested joins, with the falsy path taken too (the
    // probe's `pick(0)` enters every join from its other edge). Every value
    // crossing a label is a local; the literal-arm shape (`: 7`) is covered by
    // `coalesce_short_circuit_literals` below.
    stack_cache_carries_through_nested_joins:
        "function pick(n){ var v = n > 3 ? n * 2 : n + 7; var w = n || v; var z = n && w; return (z || v) - (n ? w : v); } pick(2);",
        "pick(64)",
        "'pk:' + pick(0) + ',' + pick(5)";
    // A short-circuit / `??` join whose arms differ in operand kind (a
    // `Tagged` local against an `Int32` literal): the verifier widens it, so
    // these functions now compile instead of being rejected fail-closed.
    coalesce_short_circuit_literals:
        "function cx(n){ var a = n ?? 7; var b = (n ?? 8) ?? 9; return a + b + (n || 5) * 1000 + (n ? 11 : 13); } cx(1);",
        "cx(0) + cx(2) * 3",
        "'cx:' + cx(4) + ',' + cx(null) + ',' + cx(0)";
    nested_while:
        "function w(n){ var t = 0, i = 0; while (i < n) { var j = 0; while (j < 3) { t += i ^ j; j++; } i++; } return t; } w(1);",
        "w(400)",
        "'w:' + w(2)";
    string_build:
        "function cat(n){ var s = ''; for (var i = 0; i < n; i++) s = s + 'ab'; return s.length; } cat(1);",
        "cat(300)",
        "'cat:' + cat(5)";
]);

const NUMERIC: &[Case] = dim_cases!("NumericTagEdge", [
    int_promotes_to_double:
        "function grow(n){ var s = 1; for (var i = 0; i < n; i++) s = s * 3 + 1; return s; } grow(1);",
        "grow(30)",
        "'grow:' + grow(3)";
    int_wrap_stays_int:
        "function wrap(n){ var s = 1; for (var i = 0; i < n; i++) s = (s * 3 + 1) | 0; return s; } wrap(1);",
        "wrap(30)",
        "'w:' + wrap(4)";
    neg_zero:
        "function nz(x){ return x * 0; } nz(1);",
        "Object.is(nz(-1), -0)",
        "'nz:' + Object.is(nz(1), 0) + ',' + String(1 / nz(-1))";
    nan_inf:
        "function nanf(x){ return (x - x) / (x - x); } function inf(){ return 1 / 0; } nanf(1);",
        "String(nanf(2)) + ',' + String(inf())",
        "'ni:' + String(nanf(9)) + ',' + String(-inf())";
    bitwise_edges:
        "function sh(n){ var r = 0; for (var i = 0; i < n; i++) { r ^= (i << 27) >> 3; r |= (i >>> 28); r &= 0xffff; } return r; } function sar(x){ return x >> 1; } sh(1); sar(-7);",
        "sh(200)",
        "'sh:' + sh(5) + ',sar:' + sar(-7) + ',' + ((-1) >>> 28)";
    mod_div_sign:
        "function md(a, b){ return a % b; } function dv(a, b){ return a / b; } md(1, 2); dv(1, 3);",
        "md(-7, 3)",
        "'md:' + md(7, -3) + ',dv:' + dv(1, 3) + ',' + dv(1e308, 10) + ',' + dv(0, 0)";
    bigint_ops:
        "function badd(a, b){ return a + b; } function bpow(x){ return x ** 30n; } badd(1n, 2n); bpow(2n);",
        "badd(9007199254740993n, 1n)",
        "'bi:' + badd(1n, 2n) + ',' + bpow(3n) + ',' + (1n << 64n)";
    string_number_coercion:
        "function sadd(a, b){ return a + b; } function ssub(a, b){ return a - b; } function smul(a, b){ return a * b; } sadd('1', '2');",
        "ssub('10', 4)",
        "'co:' + sadd('1', '2') + ',' + smul('3', '4') + ',' + sadd(1, '')";
    compare_edges:
        "function lt(a, b){ return a < b; } function seq(a, b){ return a === b; } lt(1, 2);",
        "lt('2', 10)",
        "'cmp:' + seq('5', 5) + ',' + lt(NaN, 1) + ',' + seq(undefined, undefined)";
]);

const EXCEPTION: &[Case] = dim_cases!("ExceptionOrNonthrow", [
    throw_caught:
        "function f(n){ try { if (n === 0) throw new Error('boom'); return n; } catch (e) { return 'c:' + e.message; } } f(1);",
        "f(0)",
        "'ok:' + f(7)";
    throw_uncaught:
        "function f(n){ if (n === 0) throw new TypeError('bad'); return n; } f(1);",
        "f(0)",
        "'ok:' + f(7)";
    finally_return:
        "function f(n){ try { return 't' + n; } finally { n = n + 1; } } f(1);",
        "f(7)",
        "'ok:' + f(1)";
    finally_overrides_throw:
        "function f(){ try { throw new Error('x'); } finally { return 'fin'; } } f();",
        "f()",
        "'ok:' + f()";
    throw_through_finally:
        "function f(){ try { throw 'a'; } finally { N = N + 1; } } var N = 0; try { f(); } catch (e) { }",
        "f()",
        "'N:' + N";
    nested_rethrow:
        "function f(n){ try { try { if (n === 0) throw new Error('inner'); return 'no'; } catch (e) { throw new Error('re:' + e.message); } } catch (e) { return e.message; } } f(1);",
        "f(0)",
        "'ok:' + f(1)";
    finally_on_normal_path:
        "function f(n){ var v = 0; try { v = n * 2; } catch (e) { v = -1; } finally { v += 100; } return v; } f(1);",
        "f(21)",
        "'ok:' + f(5)";
    caught_typeerror:
        "function f(n){ try { var o = null; return o.x; } catch (e) { return e.name + ':' + (e instanceof TypeError); } } f(1);",
        "f(0)",
        "'ok:' + f(0)";
]);

const OWNERSHIP: &[Case] = dim_cases!("OwnershipGc", [
    alloc_drop:
        "function mk(n){ var last = null; for (var i = 0; i < n; i++) last = { a: i, b: 'x' + i }; return last.a + '|' + last.b.length; } mk(1);",
        "mk(6000)",
        "'mk:' + mk(10)";
    alloc_string_pressure:
        "function build(n){ var last = ''; for (var i = 0; i < n; i++) { var s = 'v' + i; last = s; } return last; } build(1);",
        "build(20000)",
        "'bs:' + build(5)";
    retained_array:
        "function build(n){ var a = [0]; for (var i = 1; i < n; i++) a[i] = i; return a; } function total(a){ var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; return s; } var KEEP = build(10);",
        "total(build(1500))",
        "'keep:' + KEEP.length + ',' + total(KEEP)";
    retained_closures:
        "function mk(n){ var fns = []; for (var i = 0; i < n; i++) { (function(k){ fns.push(function(){ return k; }); })(i); } return fns; } function callall(f){ var s = 0; for (var i = 0; i < f.length; i++) s += f[i](); return s; } var F = mk(8);",
        "callall(F)",
        "'F:' + F.length + ',' + callall(F)";
    shape_churn:
        "function churn(n){ var t = 0; for (var i = 0; i < n; i++) { var o = {}; o.a = i; o.b = i * 2; t += o.a + o.b; } return t; } churn(1);",
        "churn(4000)",
        "'ch:' + churn(10)";
    interned_keys:
        "function sk(n){ var s = ''; for (var i = 0; i < n; i++) s = 'k' + i; return s; } sk(1);",
        "sk(5000)",
        "'sk:' + sk(3).length";
]);

const COERCION: &[Case] = dim_cases!("CoercionReentrancy", [
    valueof_reentry:
        "var LOG = ''; var V = { valueOf: function(){ LOG = LOG + 'v'; return 7; }, toString: function(){ LOG = LOG + 't'; return 'S'; } }; function co(x){ return x + 1; } co(1);",
        "co(V)",
        "'LOG:' + LOG + ',r:' + co(V)";
    valueof_throws:
        "var E = { valueOf: function(){ throw new RangeError('nope'); }, toString: function(){ return 'X'; } }; function f(x){ return x + 1; } f(1);",
        "f(E)",
        "'ok:' + f(1)";
    tostring_coercion:
        "var S = { toString: function(){ return 'K'; } }; function f(x){ return x + '!'; } f(S);",
        "f(S)",
        "'n:' + f(S) + ',' + f(S)";
    compare_reentry:
        "var ORDER = []; var C = { valueOf: function(){ ORDER.push('v'); return 3; } }; function lt(a, b){ return a < b; } lt(1, 2);",
        "lt(C, 5)",
        "'order:' + ORDER.join('') + ',' + lt(C, 9)";
    array_join_coercion:
        "var JLOG = ''; var JV = { toString: function(){ JLOG = JLOG + 'j'; return 'X'; } }; function j(a){ return a.join('-'); } j([1, 2]);",
        "j([1, JV, 3])",
        "'JLOG:' + JLOG + ',' + j([JV])";
    concat_reentry_loop:
        "var RLOG = ''; var R = { toString: function(){ RLOG = RLOG + 'r'; return 'P'; } }; function rep(n, o){ var s = ''; for (var i = 0; i < n; i++) s = s + o; return s; } rep(1, R);",
        "rep(4, R)",
        "'RLOG:' + RLOG.length + ',' + rep(2, R)";
]);

const ALL: [&[Case]; 5] = [NORMAL, NUMERIC, EXCEPTION, OWNERSHIP, COERCION];

fn all_cases() -> Vec<&'static Case> {
    ALL.iter().flat_map(|cases| cases.iter()).collect()
}

#[test]
fn every_dimension_is_covered() {
    let cases = all_cases();
    let mut seen: Vec<&str> = cases.iter().map(|c| c.dim).collect();
    seen.sort_unstable();
    seen.dedup();
    seen.retain(|d| DIMENSIONS.contains(d));
    assert_eq!(
        seen.len(),
        DIMENSIONS.len(),
        "not all §8.2 dimensions are covered"
    );
    // Names must be unique so a failure points at exactly one case.
    let mut names: Vec<&str> = cases.iter().map(|c| c.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), cases.len(), "case names must be unique");
    println!(
        "differential: {} cases x {} modes",
        cases.len(),
        MODES.len()
    );
}

#[test]
fn all_modes_agree_on_every_case() {
    let mut native_total = [0u64; MODES.len()];
    let mut compiled_total = [0u64; MODES.len()];

    for case in all_cases() {
        // The interpreter policy is the reference: it must never compile.
        let (reference, reference_probe, base) = run_case(Mode::Interpreter, case);
        assert_eq!(
            (base.functions_compiled, base.function_entries),
            (0, 0),
            "interpreter policy is not inert for {}",
            case.name
        );

        for (idx, mode) in MODES.into_iter().enumerate() {
            if mode == Mode::Interpreter {
                continue;
            }
            let (main, probe, metrics) = run_case(mode, case);
            assert_eq!(
                main,
                reference,
                "main differs: case {} mode {} (probe {probe} vs {reference_probe})",
                case.name,
                mode.label(),
            );
            assert_eq!(
                probe,
                reference_probe,
                "global side effects differ: case {} mode {}",
                case.name,
                mode.label(),
            );
            assert!(
                metrics.functions_queued > 0,
                "case {} queued nothing under {}",
                case.name,
                mode.label()
            );
            native_total[idx] += metrics.native_entries;
            compiled_total[idx] += metrics.functions_compiled;
        }
    }

    for (idx, mode) in MODES.into_iter().enumerate() {
        if mode == Mode::Interpreter {
            continue;
        }
        assert!(
            compiled_total[idx] > 0,
            "{} compiled nothing across the suite",
            mode.label()
        );
        assert!(
            native_total[idx] > 0,
            "{} never ran native code across the suite",
            mode.label()
        );
        println!(
            "{:<11} compiled={} native_entries={}",
            mode.label(),
            compiled_total[idx],
            native_total[idx]
        );
    }
}

/// Per-dimension roll-up for the gate report: each dimension must actually
/// drive the compiler, not just the interpreter.
#[test]
fn every_dimension_compiles_something() {
    let mut rows: Vec<(&str, u64)> = Vec::new();
    for case in all_cases() {
        let (_, _, metrics) = run_case(Mode::Automatic, case);
        rows.push((case.dim, metrics.functions_compiled));
    }
    for dim in DIMENSIONS {
        let compiled: u64 = rows.iter().filter(|(d, _)| *d == dim).map(|(_, c)| c).sum();
        assert!(compiled > 0, "nothing compiled in dimension {dim}");
        println!("dim {dim:<20} compiled={compiled}");
    }
}
