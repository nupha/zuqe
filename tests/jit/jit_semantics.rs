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

    // ── classes (base/derived/private/brand/super + object literals) ──
    class_constructor_field => Compiles =
        "var __seed = 0; class C { constructor(x){ this.v = x; } get(){ return this.v; } } function f(){ var __z = 0; var c = new C(4); return c.get(); }",
        "f()",
        "f()",
        "'cc:' + f()";
    class_inherit_super => Compiles =
        "var __seed = 0; class A { constructor(){ this.p = 1; } m(){ return this.p; } } class B extends A { m(){ return super.m() + 1; } } function f(){ var __z = 0; return new B().m(); }",
        "f()",
        "f()",
        "'ci:' + f()";
    class_private_fields => Compiles =
        "var __seed = 0; class C { #s = 5; get(){ return this.#s; } bump(){ this.#s = this.#s + 1; return this.#s; } } function f(){ var __z = 0; var c = new C(); c.bump(); return c.get(); }",
        "f()",
        "f()",
        "'cp:' + f()";
    // Computed member names, accessors, a spread/`__proto__` object literal and
    // a class expression all live in the same function: the whole
    // `define_method*`/`define_class*`/`copy_data_properties`/`set_proto` set.
    class_computed_members_and_literals => Compiles =
        "var __seed = 0; var KEY = 'k'; var G = 'g'; class C { [KEY](){ return 1; } get v(){ return 2; } set v(x){ this.w = x; } static s(){ return 3; } static get [G](){ return 4; } } function f(n){ var __z = 0; var o = { a: n, [KEY]: n + 1, ...{ b: 2 }, get [G](){ return 5; }, __proto__: null }; var c = new C(); c.v = n; return o.a + o.k + o.b + (o.__proto__ === null ? 1 : 0) + o.g + c.k() + c.v + c.w + C.s() + C[G]; }",
        "f(1)",
        "f(2)",
        "'cml:' + f(3)";

    // ── closures ──
    closure_counter => Compiles =
        "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var m = n; var r = apply(function(){ var __u = 0; m = m + 1; return m; }); return r + n; }",
        "f(1)",
        "f(5)",
        "'cl:' + f(5)";
    closure_captured_block_let => Compiles =
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

    // `set_name` also appears *inside* a function body (`var f = function(){}`),
    // so a factory that builds a named closure stays in native code instead of
    // resuming at the naming instruction.
    closure_function_expression_stays_native => Compiles =
        "var __seed = 0; function mk(n){ var __z = 0; var f = function(x){ return x * n + 1; }; return f(3); }",
        "mk(2)",
        "mk(7)",
        "'fe:' + mk(4)";

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

    // ── element access on a non-fast (dictionary) array (epic N2): `delete
    //    a[1]` converts the literal into a sparse array whose elements are own
    //    shape properties, so the element site's `(shape, prop)` cache serves
    //    the repeated `a[8]` read and write. The tombstone (`a[1]`),
    //    out-of-range (`a[99]`), negative (`a[-1]`) and `length` shapes must
    //    stay on the slow path and stay tag-exact ──
    ic_dictionary_array_elements => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var a = [1,2,3,4,5]; delete a[1]; a[8] = 40; var s = 0; for (var i = 0; i < n; i++) { s = (s + a[8]) | 0; a[8] = (a[8] + 1) & 1023; s = (s + (a[1] === undefined ? 7 : 0) + (a[99] === undefined ? 3 : 0) + (a[-1] === undefined ? 5 : 0)) | 0; a['length'] = 9; } return s + a.length; }",
        "f(1)",
        "f(400)",
        "'dic:' + f(3) + ',' + f(0)";

    // ── the inline array store (P1-1's write half): a packed array with an
    //    integer index is written in the generated code. The shapes that must
    //    fall back to the helper (a store that grows the array, a frozen
    //    array, a dictionary array, a float key) must stay tag-exact, and the
    //    store and the read have to see each other ──
    inline_array_store => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var a = [0,0,0,0]; for (var i = 0; i < n; i++) { a[i & 3] = (a[i & 3] + i) & 1023; } var b = [1,2]; b[2] = 3; var c = Object.freeze([1,2]); c[0] = 9; var d = [1,2,3]; delete d[1]; d[1] = 8; var e = [1,2]; e[1.0] = 4; return a.join(',') + '|' + b.join(',') + '|' + c.join(',') + '|' + d.join(',') + '|' + e.join(','); }",
        "f(4)",
        "f(400)",
        "'as:' + f(3) + ',' + f(0)";

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

    // ── cross-block local residency (epic S2 / 1.1): the loop header reads the
    //    induction variable, the bound and the accumulators before writing any
    //    of them, so they cross the back edge as block parameters. The safepoint
    //    on that edge must not let a register copy drift from the frame.
    carry_locals_across_the_back_edge => Compiles =
        "var __seed = 0; function f(n, seed){ var acc = seed; var step = 2; var limit = n * 3; for (var i = 0; i < limit; i = i + step){ acc = ((acc * 3 + i) ^ step) | 0; } return acc + limit + i; }",
        "f(5, 1)",
        "f(4000, 7)",
        "'xl:' + f(9, 2) + ',' + f(0, -1)";

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

    // ── sync iteration (S3 2.4.1) ──
    // Both abrupt exits are observable: `break` runs `iterator_close`, and a
    // body `throw` unwinds through the `catch_offset(0)` sentinel (which closes
    // the iterator) before reaching the JS `catch` below it. The iterator is
    // built in `setup` and its methods contain no literals, so every function
    // the warm-up calls must compile.
    iter_for_of_close_on_break_and_throw => Compiles =
        "var __seed = 0; var IT_LOG = ''; var IT_I = 0; var IT_RESULT = { value: undefined, done: false }; var IT_OBJ = {}; IT_OBJ[Symbol.iterator] = function () { IT_I = 0; return IT_OBJ; }; IT_OBJ.next = function () { IT_I = IT_I + 1; IT_RESULT.value = IT_I; IT_RESULT.done = IT_I > 3; return IT_RESULT; }; IT_OBJ.return = function () { IT_LOG = IT_LOG + 'r'; return IT_RESULT; }; function f(n) { var __z = 0; var s = 0; for (var v of IT_OBJ) { if (v === n) break; s = s + v; } try { for (var w of IT_OBJ) { if (w === n) throw 42; s = s + w; } } catch (e) { s = s + 100; } for (var u of IT_OBJ) { if (u === n) return s + u; } return s; }",
        "f(2)",
        "f(2)",
        "'iter:' + IT_LOG + ',' + f(1)";
    // Inherited keys: the enum record walks the prototype chain (and skips the
    // already-visited own key).
    iter_for_in_walks_the_prototype_chain => Compiles =
        "var __seed = 0; var FI_BASE = { a: 1 }; var FI_OBJ = Object.create(FI_BASE); FI_OBJ.b = 2; function f(n) { var __z = 0; var s = 0; var i = 0; for (var k in FI_OBJ) { s = s + k.length + i; i = i + 1; } return s + n; }",
        "f(1)",
        "f(2)",
        "'fi:' + f(3)";
    // Built-in iterators: a string (per-code-point), a Map (entry arrays) and a
    // fresh array literal. Only `for_of_next` is involved; there is no sentinel
    // to test beyond the ordinary loop.
    iter_builtin_iterables => Compiles =
        "var __seed = 0; var MI = new Map(); MI.set('a', 1); MI.set('bb', 2); function f(n) { var __z = 0; var s = 0; for (var ch of 'abc') s = s + ch.length; for (var e of MI) s = s + e[1]; for (var v of [n, n + 1]) s = s + v; return s; }",
        "f(1)",
        "f(2)",
        "'bi:' + f(3)";

    // ── ref cells (S3 2.5) ──
    // Destructuring assignment and a `for-of` target both go through a ref
    // object: `make_loc_ref` builds it, `put_ref_value` writes the element back
    // and `get_ref_value` reads a reference. `RO` lives in `setup` so the
    // warm-up's own script contains no object literal.
    ref_cells => Compiles =
        "var __seed = 0; var RO = [1, 2]; function f(n){ var __z = 0; var a = 0, b = 0; var s = 0; for (var i = 0; i < n; i++) { [a, b] = RO; s = s + a + b; } for (var v of RO) s = s + v; return s + n; }",
        "f(1)",
        "f(5)",
        "'ref:' + f(3)";

    // ── with (S3 2.4) ──
    // Every `with_*` opcode has two edges: the with object had the name (the
    // arm runs and the bytecode skips the ordinary access) or it did not (the
    // arm drops the object and the access runs). `WF_HIT` has both names used
    // below, `WF_MISS` neither, and `WF_HIDDEN` hides `x` behind
    // `@@unscopables`, which is the third way to fall through. The `for-in`
    // writes its loop variable through `with_put_var`.
    with_scope_edges => Compiles =
        "var __seed = 0; var x = 5; var WF_HIT = { x: 11 }; var WF_MISS = { y: 1 }; var WF_HIDDEN = { x: 22 }; WF_HIDDEN[Symbol.unscopables] = { x: true }; function wf(o, n){ var __z = 0; var r = 0; with (o) { r = x; for (var k in o) { r = r + k.length; } } return r + n; }",
        "wf(WF_HIT, 1)",
        "wf(WF_MISS, 2)",
        "'with:' + wf(WF_HIT, 1) + ',' + wf(WF_MISS, 2) + ',' + wf(WF_HIDDEN, 3)";

    // ── cross-block facts (S2 1.6) ──
    // The loop header re-reads a bound every iteration and the body's two arms
    // merge back into it, so the `Int32` proof has to survive a join *and* a
    // backedge. The bound is produced before the loop (tag 0 in the pool = Int);
    // `XF_BIG` is the same code with a bound the engine cannot type as Int, so
    // the shapes differ only in whether the header keeps its checks. Both must
    // agree with the interpreter, and a Float64 bound must not be read as Int.
    fact_loop_invariant_bound => Compiles =
        "var __seed = 0; var XF_BIG = 4000000000; function f(n, lim, big){ var __z = 0; var s = 0; for (var i = 0; i < lim; i++){ if (i & 1) { s = s + i; } else { s = s + (i * 2); } } for (var j = 0; j < n; j++){ if (j > big) { break; } s = s + 1; } return s; }",
        "f(3, 6, XF_BIG)",
        "f(4, 9, XF_BIG)",
        "'fact:' + f(5, 11, XF_BIG)";
    // The merge case on its own: both arms assign an Int to the same local, and
    // the join must keep the proof for the read after it (with a Float64 arm the
    // join has to widen, or the consumer would skip a needed check).
    fact_join_of_two_int_arms => Compiles =
        "var __seed = 0; var XF_I = 3; var XF_F = 2.5; function g(c, i, f){ var __z = 0; var x; if (c) { x = i + 1; } else { x = i + 2; } var s = x * 2; var y; if (c) { y = i * 3; } else { y = f; } return s + (y === f ? 1 : 0); }",
        "g(true, XF_I, XF_F)",
        "g(false, XF_I, XF_F)",
        "'fj:' + g(true, XF_I, XF_F) + ',' + g(false, XF_I, XF_F)";

    // ── inline Float64 arithmetic (epic N1) ──
    // `add`/`sub`/`mul`/`div` over two numbers take an inline `f64` arm; the
    // `DataView` exposes the exact IEEE bits, so a lost sign, a wrong NaN
    // payload (the box must canonicalize) or an int/float mix-up cannot hide
    // behind `String()`.
    float_mixed_arithmetic => Compiles =
        "var __seed = 0; var __ab = new ArrayBuffer(8); var __dv = new DataView(__ab); function bits(x){ __dv.setFloat64(0, x); return __dv.getUint32(0).toString(16) + ':' + __dv.getUint32(4).toString(16); } function fadd(a, b){ var __z = 0; return a + b; } function fsub(a, b){ var __z = 0; return a - b; } function fmul(a, b){ var __z = 0; return a * b; } function fdiv(a, b){ var __z = 0; return a / b; }",
        "fadd(1.5, 2.25); fsub(1.5, 2.25); fmul(1.5, 2); fdiv(1, 3)",
        "bits(fadd(1.5, 2.25)) + '|' + bits(fadd(1, 0.5)) + '|' + bits(fadd(0.1, 0.2)) + '|' + bits(fsub(1.5, 2.25)) + '|' + bits(fmul(1.5, -2)) + '|' + bits(fmul(3, 0.5)) + '|' + bits(fdiv(1, 3)) + '|' + bits(fdiv(1, 0)) + '|' + bits(fdiv(-1, 0)) + '|' + bits(fdiv(1, -0)) + '|' + bits(fdiv(0, 0)) + '|' + bits(fdiv(1e308, 1e-308)) + '|' + bits(fadd(2147483647, 1))",
        "'fb:' + bits(fadd(2, 3)) + ',' + bits(fdiv(6, 3))";

    // ── cross-block value reuse (S2 1.5) ──
    // The same numeric expression twice in one block is computed once; an
    // operand that is *not* provably numeric must still be evaluated every time
    // (`valueOf` counts calls), and a Float64 pair reuses the helper's result.
    // `VNC` is the observable: the interpreter's call count must survive.
    cse_repeated_expressions => Compiles =
        "var __seed = 0; var VNC = 0; var VNO = { valueOf: function(){ VNC = VNC + 1; return 1; } }; function f(n, o){ var __z = 0; var s = 0; for (var i = 0; i < n; i++){ var x = i & 7; s = s + (((x + 1) * (x + 3)) + ((x + 1) * (x + 3))); s = s + ((x * 1.5) + (x * 1.5) | 0); s = s + ((o + 1) + (o + 1) | 0); } return s + VNC; }",
        "f(1, VNO)",
        "f(5, VNO)",
        "'cse:' + f(3, VNO)";

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

    // ── the engine-generated opcodes (S3 2.6) ──
    // `throw` raises from native code: the caught value, the error class and the
    // message must be the interpreter's, in a loop that throws on every third
    // iteration and rethrows from the handler.
    raise_throw_in_a_loop => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var log = ''; for (var i = 0; i < n; i++){ try { if (i % 3 === 0) throw i; if (i % 3 === 1) throw new Error('e' + i); log = log + 'o'; } catch (e) { log = log + '|' + e.name + e.message; } } return log; }",
        "f(2)",
        "f(9)",
        "'rse:' + f(6)";
    // `throw_error`: the engine's own errors (assignment to a constant binding,
    // a TDZ read). Their text comes from the shared `engine_ops::throw_error`,
    // so a JIT run must not name a different message.
    raise_engine_generated_errors => Compiles =
        "var __seed = 0; function f(n){ var __z = 0; var log = ''; { const c = n; try { c = 1; } catch (e) { log = e.name + ':' + e.message; } } try { { let a; log = log + '|' + a; } } catch (e) { log = log + '|' + e.name; } return log; }",
        "f(1)",
        "f(2)",
        "'rse:' + f(3)";
    // `regexp` and `push_bigint_i32` build their value in a helper, so a loop
    // that creates one per iteration runs natively (no method call, no
    // conversion).
    raise_regexp_and_bigint_literals => Compiles =
        "var __seed = 0; function re(n){ var __z = 0; var s = 0; for (var i = 0; i < n; i++){ var r = /a+b/; s = s + r.source.length; } return s; } function big(n){ var __z = 0; var s = 0n; for (var i = 0; i < n; i++){ s = s + 123456789n; } return s === 123456789000n; }",
        "re(2); big(1)",
        "re(20); big(1000)",
        "'reb:' + re(5) + ',' + big(1000)";

    // ── calls: `call` / `call_method` / `call_constructor` / `apply` (D-6) ──
    // A spread call (`...`) and `Function.prototype.apply` both compile to the
    // `apply` opcode; the method form (`O.add`) is `call_method` with a
    // non-undefined `this`; `inc.call` is a normal method call on `Function`.
    call_apply_methods_and_constructor => Compiles =
        "var __seed = 0; var O = { base: 10, add(n){ return this.base + n; } }; \
         function inc(x){ return x + 1; } function Ctor(v){ this.v = v; } \
         function f(n){ var __z = 0; var acc = 0; \
           for (var i = 0; i < n; i++){ acc = inc(acc); acc = O.add(acc); acc = inc.apply(null, [acc]); \
             acc = inc.call(null, acc); acc = inc(...[acc]); acc = new Ctor(acc).v; } return acc; }",
        "f(2)",
        "f(20)",
        "'calls:' + f(5) + ',' + O.add(1)";
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
    "iter_",
    "ref_",
    "with_",
    "cse_",
    "raise_",
    "fact_",
    "float_",
    "call_",
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
