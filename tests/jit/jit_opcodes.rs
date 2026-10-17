// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! M2b-2: per-opcode end-to-end gate. Every opcode in the generated policy
//! table is claimed by at least one real JS case. A case whose claimed opcodes
//! are all `Native`/`Helper` must compile and run native code with results
//! identical to the interpreter; a case claiming any `Reject` opcode must leave
//! that function in the interpreter and still agree with the interpreter.

#![cfg(feature = "jit")]

use std::collections::HashMap;

use zuqe::jit::{JitPolicy, opcode_names, opcode_policies};

use crate::common::{Case, Mode, run_case_polled};

/// Opcodes no *final* bytecode can contain, so no JS case can ever claim them.
/// The JIT only ever sees the final stream (`JitPolicy` is indexed by
/// `OP_COUNT_FINAL`), so a temp opcode the resolver folds away is out of scope.
/// Verified against every emit site in `src/`: `invalid` is the dispatcher's
/// sentinel; `dup2` and `make_arg_ref` exist only as interpreter dispatch;
/// `nip1` survives only in commented-out emitter code; `add_loc`'s single emit
/// site (`optimize::optimize_get_loc`) matches `get_loc(n) … dup put_loc(n)
/// drop`, but `resolve_scoped_vars` rewrites every KEEP_TOP local write to
/// `set_loc` in the earlier pass, so the pattern can never appear. Lowering for
/// all of them is still covered by `jit::compiler::every_supported_opcode_lowers`,
/// which builds bytecode by hand. Keep this list justified line by line.
const UNREACHABLE: &[(&str, &str)] = &[
    ("invalid", "dispatcher sentinel, never emitted"),
    (
        "dup2",
        "interpreter dispatch only, no emit site anywhere in src/",
    ),
    (
        "make_arg_ref",
        "interpreter dispatch only, no emit site anywhere in src/",
    ),
    (
        "nip1",
        "the only emitter sites are commented out (parse/array.rs)",
    ),
    (
        "add_loc",
        "reachable only if `optimize_get_loc`'s `get_loc(n) … dup put_loc(n) drop` \
         pattern survived to pass 3, but `resolve_scoped_vars` rewrites the KEEP_TOP \
         local write to `set_loc` in pass 2",
    ),
];

/// Opcodes `Native`/`Helper` (lowering covered by
/// `jit::compiler::every_supported_opcode_lowers`) that no case in this suite
/// produces. **Empty since 2026-10-17**: the last eight entries all found
/// shapes (the depth-3 `super.x` lvalues → `dup3`/`perm5`/`rot4l`/`rot5l`, a
/// TDZ-checked `for` update → `put_loc_check`, a closure variable's `for`
/// update → `put_var_ref`, `super()` inside an arrow →
/// `put_var_ref_check_init`, and `add_loc` → moved to [`UNREACHABLE`]). The
/// list stays so a future shape-less opcode has somewhere honest to go.
const GAPS: &[(&str, &str)] = &[];

/// Cases whose function the frontend refuses today although every opcode they
/// claim is policy-accepted. **Empty since 2026-10-17** (`this_arguments_newtarget`
/// was the last one: `special_object` now lowers and the verifier no longer
/// rejects mapped `arguments`). The machinery stays so a future frontend gap can
/// be recorded here instead of silently shrinking the accepted set.
const UNCOMPILED_TODAY: &[(&str, &str)] = &[];

struct OpCase {
    name: &'static str,
    /// Opcodes this case is claimed to put in its bytecode.
    opcodes: &'static [&'static str],
    case: Case,
}

macro_rules! op_cases {
    ($($name:ident => [$($opcode:ident),*] = $setup:literal, $warm:literal, $main:literal, $probe:literal;)*) => {
        &[$(OpCase {
            name: stringify!($name),
            opcodes: &[$(stringify!($opcode)),*],
            case: Case { dim: "Opcode", name: stringify!($name), setup: $setup, warm: $warm, main: $main, probe: $probe },
        },)*]
    };
}

const CASES: &[OpCase] = op_cases! {
    literals => [push_i32, push_const, fclosure, push_atom_value, null, push_this, push_false, push_true, return, get_var, put_var, put_ref_value, get_loc, put_loc, if_false, goto, add, nop]
        = "var __seed = 0; function f(){ var __z = 0; var a = 1; var b = 2.5; var c = 'abc'; var d = undefined; var e = null; var g = false; var h = true; var t = this; return a + b + (t ? 0 : 0); }",
                  "f(0)",
          "f(0)",
          "'lit:' + f(0)";
    locals_and_args => [push_i32, fclosure, drop, return, put_var, put_ref_value, get_loc, put_loc, set_loc, get_arg, set_arg, add, sub, nop]
        = "var __seed = 0; function f(a, b){ var __z = 0; var x = a; x = x + b; a = x; return x - a + b; }",
                  "f(1, 2)",
          "f(3, 4)",
          "'la:' + f(3, 4)";
    let_locals => [push_i32, fclosure, drop, return, put_var, put_ref_value, get_loc, put_loc, set_loc, get_arg, set_loc_uninitialized, get_loc_check, mul, add, nop]
        = "var __seed = 0; function f(n){ var __z = 0; var r = 0; { let x = n + 1; let y = x * 2; r = x + y; } return r; }",
                  "f(1)",
          "f(4)",
          "'ll:' + f(4)";
    parameter_defaults => [push_i32, fclosure, undefined, drop, dup, return, put_var, put_ref_value, get_loc, put_loc, get_arg, set_arg, set_loc_uninitialized, get_loc_check, if_false, add, strict_eq, nop]
        = "var __seed = 0; function f(a = 7, b = a + 1){ var __z = 0; return a + b; }",
                  "f(1)",
          "f()",
          "'pd:' + f()";
    implicit_return => [push_i32, fclosure, return, return_undef, put_var, put_ref_value, get_loc, put_loc, get_arg, add, nop]
        = "var __seed = 0; function f(n){ var __z = 0; var x = n + 1; }",
                  "f(1)",
          "f(4)",
          "'ir:' + typeof f(4)";
    temporal_dead_zone => [push_i32, fclosure, undefined, drop, return, throw, put_var, put_ref_value, get_loc, put_loc, set_loc, get_arg, set_loc_uninitialized, get_loc_check, set_loc_check, goto, catch, add, nop]
        = "var __seed = 0; function f(n){ var __z = 0; var r = 0; { let a; try { r = r + a; } catch (e) { r = 100; } a = n + 1; let b = n; b = b + 1; r = r + a + b; } return r; }",
                  "f(1)",
          "f(5)",
          "'tdz:' + f(5)";
    // `set_name` is not claimed: the anonymous inner function expression only
    // carries it in the pre-optimization (pass 1) stream, where the audit still
    // sees it, but the optimizer drops it — the final bytecode has none.
    closure_var_refs => [push_i32, fclosure, drop, insert3, call, return, get_var, put_var, put_ref_value, get_loc, put_loc, set_loc, get_arg, get_var_ref, set_var_ref, add, nop]
        = "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var m = n; var r = apply(function(){ var __u = 0; m = m + 1; return m; }); return r + n; }",
                  "f(1)",
          "f(5)",
          "'cv:' + f(5)";
    closure_let_refs => [push_i32, fclosure, drop, dup, insert3, call, return, get_var, put_var, put_ref_value, set_name, get_loc, put_loc, set_loc, get_arg, set_loc_uninitialized, get_var_ref_check, put_var_ref_check, close_loc, add, nop]
        = "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var out = 0; { let m = n; out = apply(function(){ var __u = 0; m = m + 1; return m; }); } return out + n; }",
                  "f(2)",
          "f(3)",
          "'cr:' + f(3)";
    captured_loop_ref => [push_i32, fclosure, drop, insert3, perm4, call, return, get_var, put_var, get_ref_value, put_ref_value, get_loc, put_loc, set_loc, get_arg, get_var_ref, if_false, goto, post_inc, inc_loc, add, lt, nop]
        = "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var out = 0; for (var i = 0; i < 3; i++) { out = apply(function(){ var __u = 0; return i; }); } return out + n; }",
                  "f(1)",
          "f(2)",
          "'clr:' + f(2)";
    nested_closure_ref => [push_i32, fclosure, object, drop, insert3, call, return, get_var, put_var, put_ref_value, set_name, get_loc, put_loc, set_loc, get_arg, get_var_ref, set_loc_uninitialized, to_object, with_get_var, with_make_ref, make_var_ref_ref, add, nop]
        = "var __seed = 0; function apply(g){ var __z = 0; return g(); } function f(n){ var __z = 0; var m = n; var r = apply(function(){ var __u = 0; return apply(function(){ var __t = 0; with ({}) { m = m + 1; } return m; }); }); return r + n; }",
                  "f(1)",
          "f(4)",
          "'ncr:' + f(4)";
    globals => [push_i32, fclosure, push_atom_value, drop, dup, return, get_var_undef, get_var, put_var, put_ref_value, get_loc, put_loc, get_arg, if_false, goto, typeof, delete_var, add, strict_eq, nop]
        = "var G = 0; var W = 0; var GG = 1; function f(n){ var __z = 0; G = n + 1; W = W + 1; delete GG; return G + W + (typeof NOPE === 'undefined' ? 0 : 1); }",
                  "f(0)",
          "f(5)",
          "'gl:' + f(5)";
    with_statement => [push_i32, fclosure, object, drop, return, get_var, put_var, put_ref_value, define_field, get_loc, put_loc, set_loc, get_arg, set_loc_uninitialized, to_object, with_get_var, with_delete_var, with_make_ref, make_loc_ref, make_var_ref, delete_var, add, nop]
        = "var __seed = 0; var WO = {x: 1}; function f(o){ var __z = 0; var r = 0; with (o) { r = x; x = r + 1; delete x; } return r; }",
                  "f(WO)",
          "f(WO)",
          "'w:' + f(WO)";
    unary_operators => [push_i32, fclosure, push_atom_value, drop, return, put_var, put_ref_value, get_loc, put_loc, set_loc, get_arg, if_false, goto, neg, plus, dec, inc, post_dec, post_inc, dec_loc, inc_loc, not, lnot, typeof, add, strict_eq, and, nop]
        = "var __seed = 0; function f(n){ var __z = 0; var x = n; x += 2; x++; x--; var q = (x++) + (x--) + (++x) + (--x); var a = -x; var b = +x; var c = !a; var d = ~b; var e = typeof x; return x + a + b + (c ? 1 : 0) + (d & 0) + (e === 'number' ? 1 : 0); }",
                  "f(5)",
          "f(9)",
          "'u:' + f(9)";
    binary_operators => [push_i32, fclosure, drop, return, put_var, put_ref_value, get_loc, put_loc, set_loc, get_arg, if_false, inc_loc, mul, div, mod, add, sub, pow, shl, sar, shr, lt, lte, gt, gte, eq, neq, strict_eq, strict_neq, and, xor, or, nop]
        = "var __seed = 0; function f(n){ var __z = 0; var x = n; var s = 0; s += x + 1; s += x - 1; s += x * 2; s += x / 2; s += x % 3; s += x ** 2; if (x < 3) s++; if (x <= 3) s++; if (x > 3) s++; if (x >= 3) s++; if (x == 3) s++; if (x != 3) s++; if (x === 3) s++; if (x !== 3) s++; s += x & 3; s += x | 3; s += x ^ 3; s += x << 1; s += x >> 1; s += x >>> 1; return s; }",
                  "f(4)",
          "f(7)",
          "'b:' + f(7)";
    relational_operators => [push_i32, fclosure, push_atom_value, object, return, put_var, put_ref_value, define_field, get_loc, put_loc, get_arg, if_false, goto, add, instanceof, in, nop]
        = "var __seed = 0; var KO = {k: 1}; function f(o, c){ var __z = 0; return (('k' in o) ? 1 : 0) + ((o instanceof c) ? 2 : 0); }",
                  "f(KO, Object)",
          "f(KO, Object)",
          "'ro:' + f(KO, Object)";
    // `??` lowers inline (its `is_undefined_or_null` guard) and its join with
    // the literal arm verifies, so this is a fully compiling case.
    nullish_coalescing => [push_i32, fclosure, drop, dup, return, put_var, put_ref_value, get_loc, put_loc, get_arg, if_false, is_undefined_or_null, nop]
        = "var __seed = 0; function f(x){ var __z = 0; return x ?? 7; }",
                  "f(null)",
          "f(3)",
          "'nc:' + f(3) + ',' + f(null)";
    properties => [push_i32, fclosure, push_atom_value, object, drop, array_from, return, put_var, put_ref_value, get_field, get_field2, put_field, get_array_el, get_array_el3, put_array_el, define_field, get_loc, put_loc, get_arg, delete, add, nop]
        = "var __seed = 0; var PO = {p: 1, q: 2, tmp: 9}; var PA = [0, 5, 0]; function f(o, a, i){ var __z = 0; var x = o.p; o.p = x + 1; o.q += 1; var y = o.q; var z = a[i]; a[i] += 1; delete o.tmp; return x + y + z + a[i]; }",
                  "f(PO, PA, 1)",
          "f(PO, PA, 1)",
          "'p:' + f(PO, PA, 1)";
    array_literal => [push_i32, fclosure, array_from, return, put_var, put_ref_value, get_field, get_array_el, get_loc, put_loc, set_loc, get_arg, add, nop]
        = "var __seed = 0; function f(n){ var __z = 0; var a = [n, n + 1, n + 2]; return a[0] + a.length; }",
                  "f(1)",
          "f(4)",
          "'al:' + f(4)";
    spread_call => [push_i32, fclosure, drop, perm3, array_from, apply, return, get_var, put_var, put_ref_value, get_field2, append, get_loc, put_loc, get_arg, nop]
        = "var __seed = 0; function f(a){ var __z = 0; return Math.max(1, ...a); }",
                  "f([5, 9])",
          "f([3, 8, 2])",
          "'sc:' + f([3, 8, 2])";
    object_spread => [push_i32, fclosure, null, object, drop, return, put_var, put_ref_value, copy_data_properties, get_loc, put_loc, get_arg, nop]
        = "var __seed = 0; function f(o){ var __z = 0; return { ...o }; }",
                  "f({a: 1})",
          "f({b: 2})",
          "'os:' + f({c: 3}).c";
    object_literal => [push_i32, push_const, fclosure, push_atom_value, undefined, null, push_this, object, drop, dup, swap, call, call_method, return, return_undef, check_ctor, get_var, put_var, put_ref_value, define_field, set_name, set_name_computed, set_proto, define_array_el, define_method, define_method_computed, define_class_computed, get_loc, put_loc, get_arg, get_var_ref, set_loc_uninitialized, get_var_ref_check, close_loc, if_false, to_propkey, add, nop]
        = "var __seed = 0; function f(n){ var __z = 0; return { a: n, ['b']: n + 1, m(){ return n; }, [String(n)]: function(){ return n; }, [String(n) + 'M'](){ return n; }, [String(n) + 'C']: class { z(){ return 1; } }, __proto__: null }; }",
                  "f(1)",
          "f(2)",
          "'ol:' + f(3).a";
    object_rest_pattern => [push_i32, fclosure, undefined, null, object, drop, dup, swap, rot3l, return, put_var, put_ref_value, get_field, define_field, copy_data_properties, get_loc, put_loc, get_arg, if_true, goto, to_object, make_loc_ref, for_of_start, for_of_next, iterator_close, add, strict_eq, nop]
        = "var __seed = 0; function f(p){ var __z = 0; var a, b, c; [a, b] = p; ({x: c} = p); var {y, ...rest} = p; return a + b + c + y + rest.z; }",
                  "f([1, 2, {x: 3, y: 4, z: 5}])",
          "f([3, 4, {x: 5, y: 6, z: 7}])",
          "'orp:' + f([1, 2, {x: 3, y: 4, z: 5}])";
    destructuring_shuffle => [push_i32, fclosure, object, drop, dup, dup1, swap2, rot3l, rot3r, array_from, return, get_var, put_var, put_ref_value, get_field, put_field, get_array_el, get_array_el2, define_field, get_loc, put_loc, get_arg, goto, to_object, to_propkey, make_loc_ref, for_of_start, for_of_next, iterator_close, add, nop]
        = "var __seed = 0; var o1 = {p: 1, q: [7], r: 8}; function f(p, k, o){ var __z = 0; var a = 1, b = 2, c = 3, d = 4, e = 5; var v = 0; [a, b, c, d, e] = p; [a, b] = [b, a]; ({[k]: o.p} = o1); ({[k]: [v]} = o1); ({[k]: v} = o1); ({a: v} = o1); return a + b + c + d + e + v + o.p; }",
                  "f([1, 2, 3, 4, 5], 'q', o1)",
          "f([10, 20, 30, 40, 50], 'r', o1)",
          "'ds:' + f([1, 2, 3, 4, 5], 'q', o1)";
    call_shapes => [push_i32, fclosure, push_this, object, drop, dup, insert2, call_constructor, call, call_method, return, return_undef, get_var, put_var, put_ref_value, get_field, get_field2, put_field, define_field, set_name, get_loc, put_loc, get_arg, mul, add, nop]
        = "var __seed = 0; var GADD = function(a, b){ var __t = 0; return a + b; }; var MO1 = {m: function(x){ return x; }}; var MO2 = {m: function(x){ return x * 2; }}; var CT1 = function(n){ this.v = n; }; var CT2 = function(n){ this.v = n * 10; }; function f(o, Ctor){ var __z = 0; return GADD(1, 2) + o.m(3) + (new Ctor(4)).v; }",
                  "f(MO1, CT1)",
          "f(MO2, CT2)",
          "'cs:' + f(MO1, CT1)";
    // Proper tail calls are only rewritten in strict mode (ES IsInTailPosition).
    tail_calls => [push_i32, fclosure, push_atom_value, drop, call, tail_call, call_method, tail_call_method, return, get_var, put_var, put_ref_value, get_field2, get_loc, put_loc, get_arg, add, nop]
        = "var __seed = 0; function g(n){ var __z = 0; return n + 1; } function f(n){ \"use strict\"; var __z = 0; return g(n); } function h(o){ \"use strict\"; var __z = 0; return o.m(); }",
                  "f(1)",
          "f(3) + h({m: function(){ return 1; }})",
          "'tc:' + f(4)";
    eval_calls => [push_i32, fclosure, push_atom_value, push_this, special_object, drop, insert3, array_from, return, eval, tail_eval, apply_eval, get_var, put_var, put_ref_value, append, get_loc, put_loc, get_arg, make_var_ref, add, nop]
        = "var __seed = 0; var G = 1; function f(n){ \"use strict\"; var __z = 0; return eval('n + 1'); } function h(n){ \"use strict\"; var __z = 0; G = G + eval('n'); return G; } function e2(a){ \"use strict\"; var __z = 0; return eval(...a); }",
                  "f(1)",
          "f(3)",
          "'ev:' + f(4)";
    dynamic_import => [push_i32, fclosure, undefined, null, return, throw, import, put_var, put_ref_value, get_loc, put_loc, get_arg, catch, nip_catch, nop]
        = "var __seed = 0; function f(p){ var __z = 0; try { return import(p); } catch (e) { return null; } }",
                  "f('./x.js')",
          "f('./y.js')",
          "'im:' + typeof f('./z.js')";
    regexp_literal => [push_i32, push_const, fclosure, call_method, return, regexp, put_var, put_ref_value, get_field2, get_loc, put_loc, get_arg, nop]
        = "var __seed = 0; function f(s){ var __z = 0; return /ab+c/.test(s); }",
                  "f('abbbbc')",
          "f('ac')",
          "'re:' + f('abbbc')";
    bigint_literal => [push_i32, fclosure, call, return, get_var, put_var, put_ref_value, get_loc, put_loc, get_arg, add, push_bigint_i32, nop]
        = "var __seed = 0; function f(n){ var __z = 0; return 5n + BigInt(n); }",
                  "f(1)",
          "f(2)",
          "'bi:' + f(4)";
    try_catch_finally => [push_i32, fclosure, push_atom_value, undefined, drop, dup, call_constructor, return, throw, get_var, put_var, put_ref_value, get_loc, put_loc, get_arg, set_arg, if_false, goto, catch, gosub, ret, add, strict_eq, nop]
        = "var __seed = 0; function f(n){ var __z = 0; try { if (n === 0) throw new Error('x'); } catch (e) { } finally { n = n + 1; } return n; }",
                  "f(0)",
          "f(3)",
          "'tc:' + f(3)";
    control_flow => [push_i32, fclosure, return, put_var, put_ref_value, get_loc, put_loc, get_arg, if_false, neg, gt, nop]
        = "var __seed = 0; function f(n){ var __z = 0; ; if (n > 0) { return n; } else { return -n; } }",
                  "f(1)",
          "f(-4)",
          "'cf:' + f(6)";
    // The iterable and the object live in `setup` so the *warm-up script*
    // contains no rejected opcode (`define_field` for an inline object literal
    // would otherwise be the thing the gate sees rejected, not `f`).
    for_of_and_for_in => [push_i32, fclosure, drop, array_from, return, put_var, put_ref_value, get_field, get_loc, put_loc, set_loc, get_arg, if_false, goto, for_in_start, for_of_start, for_in_next, for_of_next, iterator_close, add, nop]
        = "var __seed = 0; var OFO = {a: 1, bb: 2}; function f(o){ var __z = 0; var s = 0; for (var v of [1, 2, 3]) s += v; for (var k in o) s += k.length; return s; }",
                  "f(OFO)",
          "f(OFO)",
          "'it:' + f(OFO)";
    // `super.x` as an lvalue is the only producer of the depth-3 shuffles:
    // `super.p++` (dup3 + perm5), `({v: super.p} = o)` (rot4l) and the
    // computed-key form `({[k]: super.p} = o)` (rot5l).
    super_lvalue_shuffles => [dup3, perm5, rot4l, rot5l]
        = "var __seed = 0; class A { constructor(){ this.q = 0; } get p(){ return this.q; } set p(v){ this.q = v; } } class B extends A { bump(){ return super.p++; } setNamed(o){ ({ v: super.p } = o); return super.p; } setComputed(o, k){ ({ [k]: super.p } = o); return super.p; } } function f(n){ var b = new B(); b.bump(); b.setNamed({v: n}); b.setComputed({z: n + 1}, 'z'); return b.p; }",
                  "f(1)",
          "f(5)",
          "'sv:' + f(3)";
    // A `let` written by a `for` update outside its declaration is TDZ-checked
    // on every write (`get_loc_check` … `put_loc_check`).
    tdz_for_update => [put_loc_check]
        = "var __seed = 0; function f(n){ let a; for (a = 0; a < n; a++) {} return a; }",
                  "f(1)",
          "f(4)",
          "'tfu:' + f(3)";
    // A `for` update on a variable captured from an enclosing function writes
    // through the closure cell (`put_var_ref`).
    closure_var_for_update => [put_var_ref]
        = "var __seed = 0; function f(n){ var x = 0; function g(){ for (x = 0; x < n; x++) {} } g(); return x; }",
                  "f(1)",
          "f(4)",
          "'cvfu:' + f(3)";
    // `a[i]++` is the depth-2 keep-second lvalue path (`perm4`): the old value
    // is kept for the expression while the new one is stored.
    array_element_postfix => [perm4]
        = "var __seed = 0; function f(a, i){ var r = 0; for (var k = 0; k < 3; k++) { r = r + a[i]++; } return r + a[i]; }",
                  "f([1], 0)",
          "f([5], 0)",
          "'ap:' + f([9], 0)";
    // ── one order-observable shape per stack shuffle (see the oracle test
    //    `lvalue_shuffle_semantics_match_the_language`) ──
    // `nip` drops the operand below the top. It sits on the *short-circuit*
    // arm of a logical assignment (`||=` keeps the existing value, which is
    // the expression's result), so the observed value must come from that arm:
    // the `p` argument is truthy here (the falsy path assigns instead and never
    // runs the `nip`).
    logical_assign_nip => [nip]
        = "var __seed = 0; function f(o, v){ var r = (o.p ||= v); return r * 10 + o.p; }",
                  "f({p: 1}, 1)",
          "f({p: 3}, 5)",
          "'nip:' + f({p: 4}, 9)";
    // `dup`: the `??` guard keeps the operand for both the test and the result.
    dup_nullish => [dup]
        = "var __seed = 0; function f(a, b){ return a ?? b; }",
                  "f(null, 1)",
          "f(null, 4)",
          "'dn:' + f(0, 4) + ',' + f(null, 9)";
    // `dup1` + `swap2`: a computed-key target that is a *local* (the reference
    // object for the local is built, then the object/prop pair is rotated).
    swap2_computed_target => [dup1, swap2]
        = "var __seed = 0; function f(o, k){ var v = 0; ({ [k]: v } = o); return v; }",
                  "f({q: 1}, 'q')",
          "f({q: 7}, 'q')",
          "'s2:' + f({q: 9}, 'q')";
    // `insert2`: an assignment expression's value survives above its target.
    insert2_field_assign_value => [insert2]
        = "var __seed = 0; function f(o, v){ return (o.p = v) * 2; }",
                  "f({}, 1)",
          "f({}, 4)",
          "'i2:' + f({}, 6)";
    // `insert3`: the same for an element store.
    insert3_element_assign_value => [insert3]
        = "var __seed = 0; function f(a, i, v){ return (a[i] = v) + 1; }",
                  "f([0], 0, 1)",
          "f([0], 0, 5)",
          "'i3:' + f([0], 0, 7)";
    // `insert4` (+ `swap` from the class machinery): `super.p = v` as an
    // expression keeps `this` under the value.
    insert4_super_assign_value => [insert4, swap]
        = "var __seed = 0; class A { set p(v){ this.q = v; } get p(){ return this.q; } } class B extends A { m(){ return (super.p = 5) + this.q; } } function f(){ return new B().m(); }",
                  "f()",
          "f()",
          "'i4:' + f()";
    // `swap`: a `for-in` whose target is a property needs the store's
    // object/value order swapped.
    swap_for_in_target => [swap]
        = "var __seed = 0; function f(src, o){ var n = 0; for (o.p in src) { n = n + 1; } return n * 10 + o.p.length; }",
                  "f({a: 1}, {})",
          "f({a: 1, b: 2}, {})",
          "'sw:' + f({a: 1, b: 2, c: 3}, {})";
    // `perm3`: the arguments of a spread call must keep their order (a
    // commutative callee like `Math.max` would hide a swap).
    perm3_spread_call_order => [perm3]
        = "var __seed = 0; var MO = { m: function(a, b, c){ return a * 100 + b * 10 + c; } }; function f(xs){ return MO.m(1, ...xs); }",
                  "f([1, 2])",
          "f([4, 5])",
          "'p3:' + f([7, 8])";
    // `rot3l`: object rest; the pattern source/property pair is rotated before
    // the copy, so `rest` sees the wrong properties if the order is off.
    rot3l_object_rest => [rot3l]
        = "var __seed = 0; function f(p){ var y = 0, z = {}; ({ y, ...z } = p); return y * 100 + z.w; }",
                  "f({y: 1, w: 2})",
          "f({y: 3, w: 4})",
          "'r3l:' + f({y: 5, w: 6})";
    // `rot3r` (+ `dup1`): a computed-key target that is a *field*.
    rot3r_computed_field_target => [dup1, rot3r]
        = "var __seed = 0; function f(o, k, v){ ({ [k]: o.p } = { [k]: v }); return o.p; }",
                  "f({p: 0}, 'q', 1)",
          "f({p: 0}, 'q', 5)",
          "'r3r:' + f({p: 0}, 'q', 7)";
    // `super()` inside an arrow captures the constructor's `this` as a lexical
    // closure variable, whose initialization is `put_var_ref_check_init`.
    captured_this_super_init => [put_var_ref_check_init]
        = "var __seed = 0; class A { constructor(x){ this.x = x; } } class B extends A { constructor(){ var init = (x) => super(x); init(7); } } function f(n){ var b = new B(); return b.x + n; }",
                  "f(1)",
          "f(2)",
          "'ctsi:' + f(3)";
    generators_and_async => [push_i32, fclosure, undefined, drop, nip, dup, insert3, call, call_method, array_from, return, return_async, throw_error, get_var, put_var, get_ref_value, put_ref_value, get_field, get_field2, get_loc, put_loc, set_loc, get_arg, if_false, if_true, goto, for_of_start, for_await_of_start, for_await_of_next, iterator_check_object, iterator_get_value_done, iterator_close, iterator_next, iterator_call, initial_yield, yield, yield_star, async_yield_star, await, add, strict_eq, nop]
        = "function* g(){ yield 1; yield* [2, 3]; } async function af(xs){ var __z = 0; var s = 0; for await (var v of xs) s += v; return s; } async function* ag(){ yield* [1, 2]; } function f(){ var __z = 0; var it = g(); return it.next().value; }",
                  "f()",
          "f()",
          "'gy:' + f()";
    classes => [push_i32, push_const, fclosure, push_atom_value, private_symbol, undefined, null, push_this, push_true, object, special_object, drop, dup, insert2, insert3, insert4, swap, call_constructor, call_method, return, return_undef, check_ctor_return, check_ctor, init_ctor, check_brand, add_brand, get_super, get_var, put_var, put_var_init, put_ref_value, get_field, get_field2, put_field, get_private_field, put_private_field, define_private_field, get_array_el, put_array_el, get_super_value, put_super_value, set_name, set_home_object, define_method, define_class, get_loc, put_loc, set_loc, get_arg, get_var_ref, set_loc_uninitialized, get_loc_check, put_loc_check_init, get_loc_checkthis, get_var_ref_check, close_loc, if_false, goto, add, private_in, nop]
        = "var __seed = 0; class A { constructor(x){ this.p = x; } m(){ return this.p; } } class B extends A { #priv = 3; #pm(){ return 1; } constructor(){ super(1); this.q = 2; } m(){ this.#priv = 9; var r = (super.p = 5); return super.m() + super.p + this.#priv + this.#pm() + ((#priv in this) ? 1 : 0) + r; } } class E extends A { } class D extends A { constructor(){ super(1); return this; } } function f(k){ var __z = 0; var o = {}; o[k] = class { n(){ return 1; } }; var b = new B(); var e = new E(2); var d = new D(); return b.m() + e.p + d.p; }",
                  "f('C')",
          "f('D')",
          "'cl:' + f('E')";
    scoped_block_close => [push_i32, fclosure, object, drop, insert2, call_method, return, put_var, put_ref_value, get_field2, put_field, set_name, get_loc, put_loc, get_arg, set_loc_uninitialized, get_var_ref_check, close_loc, add, nop]
        = "var __seed = 0; function f(n){ var __z = 0; var o = {}; { let x = n + 1; o.g = function(){ return x; }; } return o.g(); }",
                  "f(1)",
          "f(5)",
          "'sb:' + f(5)";
    this_arguments_newtarget => [push_i32, fclosure, push_atom_value, push_this, special_object, return, put_var, put_ref_value, get_field, get_loc, put_loc, get_arg, if_false, goto, typeof, add, strict_eq, nop]
        = "var __seed = 0; function f(a){ var __z = 0; var t = this; var args = arguments.length; var nt = typeof new.target; return a + args + (nt === 'undefined' ? 0 : 1) + (t ? 0 : 0); }",
                  "f(1)",
          "f(2)",
          "'ta:' + f(4)";
    // `with` + a *call* to a name: the parser emits `scope_get_ref` (the name
    // may be a method of the with object), which resolves to `with_get_ref`.
    // `with_get_ref` was the last `with_*` opcode to lower (S3 2.4).
    with_get_ref => [push_i32, fclosure, push_atom_value, undefined, null, object, drop, dup, call_method, return, return_undef, get_var, put_var, put_ref_value, define_field, set_name, get_loc, put_loc, set_loc, get_arg, set_loc_uninitialized, if_false, if_true, to_object, with_get_var, with_get_ref, typeof, add, strict_eq, strict_neq, nop]
        = "var __seed = 0; var WO = { ff: function(){ return 5; } }; function wf(o, n){ var __z = 0; with (o) { return ff() + n; } }",
                  "wf(WO, 1)",
          "wf(WO, 2)",
          "'wr:' + wf(WO, 3)";
    // `with` + a `var` written through the with object (`for (var k in o)`).
    with_put_var => [push_i32, fclosure, push_atom_value, null, object, drop, dup, return, put_var, put_ref_value, define_field, get_loc, put_loc, get_arg, set_loc_uninitialized, if_false, if_true, goto, to_object, with_get_var, with_put_var, with_make_ref, make_loc_ref, for_in_start, for_in_next, typeof, add, strict_eq, strict_neq, nop]
        = "var __seed = 0; var WO2 = { a: 1, b: 2 }; function ws(o, n){ var __z = 0; var r = 0; with (o) { for (var k in o) { r = r + 1; } } return r + n; }",
                  "ws(WO2, 1)",
          "ws(WO2, 2)",
          "'wv:' + ws(WO2, 3)";
    rest_parameters => [push_i32, fclosure, rest, drop, dup, return, put_var, put_ref_value, get_field, get_array_el, get_loc, put_loc, get_arg, put_arg, if_true, add, nop]
        = "var __seed = 0; function f(a, ...rest){ var __z = 0; return a + rest.length + (rest[0] || 0); }",
                  "f(1, 2, 3)",
          "f(5)",
          "'rp:' + f(5, 7)";
};

fn must_claim_reject(op_case: &OpCase, policies: &HashMap<&'static str, JitPolicy>) -> bool {
    op_case
        .opcodes
        .iter()
        .any(|name| policies.get(*name).copied() == Some(JitPolicy::Reject))
}

fn policy_by_name() -> HashMap<&'static str, JitPolicy> {
    opcode_names()
        .iter()
        .zip(opcode_policies())
        .map(|(name, policy)| (*name, *policy))
        .collect()
}

/// The claim list is the manifest for the gate: every opcode in the generated
/// table must be claimed by some case, or be one of the justified exceptions
/// above. Claims themselves are checked against the real bytecode by
/// `bytecode_dump_audit` (run with `dump_bytecode`, then `fixclaims`-style
/// grep), so a claim that no JS produces cannot survive.
#[test]
fn every_policy_opcode_is_covered_by_a_case() {
    let policies = policy_by_name();
    let mut covered: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut unknown: Vec<String> = Vec::new();
    for op_case in CASES {
        assert!(
            !op_case.opcodes.is_empty(),
            "{} claims nothing",
            op_case.name
        );
        for opcode in op_case.opcodes {
            if !policies.contains_key(opcode) {
                unknown.push(format!("{} -> {opcode}", op_case.name));
            }
            covered.entry(opcode).or_default().push(op_case.name);
        }
    }
    assert!(
        unknown.is_empty(),
        "claims naming no table opcode: {unknown:?}"
    );

    let exceptions: Vec<&str> = UNREACHABLE
        .iter()
        .chain(GAPS.iter())
        .map(|(name, _)| *name)
        .collect();
    let missing: Vec<&str> = opcode_names()
        .iter()
        .copied()
        .filter(|name| !covered.contains_key(name) && !exceptions.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "opcodes with no JS case: {missing:?} (extend CASES or justify above)"
    );

    for (name, _) in UNREACHABLE.iter().chain(GAPS.iter()) {
        assert!(
            policies.contains_key(name),
            "{name} is no longer in the policy table"
        );
        assert!(
            !covered.contains_key(name),
            "{name} moved out of its exception list but is now claimed"
        );
    }
    // The gap list is a known debt, not a licence to grow.
    assert!(GAPS.len() <= 10, "M2b-2's gap list grew: {GAPS:?}");
    for (name, _) in UNCOMPILED_TODAY {
        assert!(
            CASES.iter().any(|op_case| op_case.name == *name),
            "{name} is in UNCOMPILED_TODAY but is not a case"
        );
    }
    println!(
        "opcode coverage: {} opcodes, {} cases, {} claimed, {} unreachable, {} gap",
        opcode_names().len(),
        CASES.len(),
        covered.len(),
        UNREACHABLE.len(),
        GAPS.len()
    );
    for (name, why) in GAPS {
        println!("  gap {name}: {why}");
    }
}

/// Behavioural half of the gate: a case with only `Native`/`Helper` opcodes
/// must compile everything it queues in `setup` and run native code with results
/// identical to the interpreter; a case claiming a `Reject` opcode must leave
/// that function in the interpreter and still agree with it.
#[test]
fn every_case_matches_policy_and_interpreter() {
    let policies = policy_by_name();
    let mut accept = 0usize;
    let mut reject = 0usize;

    for op_case in CASES {
        let uncompiled_today = UNCOMPILED_TODAY
            .iter()
            .any(|(name, _)| *name == op_case.name);
        if uncompiled_today {
            assert!(
                !must_claim_reject(op_case, &policies),
                "{} is in UNCOMPILED_TODAY but claims a Reject opcode",
                op_case.name
            );
        }
        let must_reject = must_claim_reject(op_case, &policies) || uncompiled_today;

        let base = run_case_polled(Mode::Interpreter, &op_case.case);
        assert_eq!(
            base.after_warm.functions_compiled, 0,
            "{}: interpreter policy compiled something",
            op_case.name
        );

        for mode in [Mode::Tier1, Mode::Tier2, Mode::Automatic] {
            let run = run_case_polled(mode, &op_case.case);
            assert_eq!(
                run.main,
                base.main,
                "{}: main differs from the interpreter under {}",
                op_case.name,
                mode.label()
            );
            assert_eq!(
                run.probe,
                base.probe,
                "{}: side effects differ from the interpreter under {}",
                op_case.name,
                mode.label()
            );

            // Metrics are cumulative, so the warm-up phase is the delta
            // against the definition script's snapshot. That phase queues
            // exactly the warm-up script plus the functions it calls, which is
            // the precise statement about the case's own function.
            let m = run.after_warm;
            let queued = m.functions_queued - run.after_definition.functions_queued;
            let compiled = m.functions_compiled - run.after_definition.functions_compiled;
            let rejected = m.functions_rejected - run.after_definition.functions_rejected;
            // The definition script's own verdict (it is polled before the
            // warm-up snapshot): a Reject opcode that only the script contains
            // lands here.
            let rejected_by_definition = run.after_definition.functions_rejected;
            assert!(
                queued >= 2,
                "{}: nothing queued under {} (metrics: {m:?})",
                op_case.name,
                mode.label()
            );
            assert_eq!(
                compiled + rejected,
                queued,
                "{}: a queued function was neither compiled nor rejected under {}",
                op_case.name,
                mode.label()
            );

            if must_reject {
                assert!(
                    rejected_by_definition + rejected >= 1,
                    "{}: {} compiled the whole Reject-opcode case (metrics: {m:?})",
                    op_case.name,
                    mode.label()
                );
                reject += 1;
            } else {
                assert_eq!(
                    rejected,
                    0,
                    "{}: {} rejected a Native/Helper-only function (metrics: {m:?})",
                    op_case.name,
                    mode.label()
                );
                assert_eq!(
                    compiled,
                    queued,
                    "{}: not every warm-up function compiled under {}",
                    op_case.name,
                    mode.label()
                );
                assert!(
                    run.after_main.native_entries > 0,
                    "{}: no native code ran under {}",
                    op_case.name,
                    mode.label()
                );
                accept += 1;
            }
        }
    }
    println!("opcode cases: {accept} accepted-mode runs, {reject} rejected-mode runs");
}

/// Epic F §6.12: `fclosure` — the entry opcode of *every* script that declares a
/// function — now lowers, so a definition script is verified and compiled
/// instead of being refused at its frame entry. (The script body runs once per
/// eval, so this is coverage rather than speed; `fclosure` *inside* a function
/// body, a factory, is the speed case.)
#[test]
fn a_script_that_declares_a_function_compiles() {
    let case = Case {
        dim: "Opcode",
        name: "definition_script",
        setup: "function g(n){ return n + 1; } g(1);",
        warm: "g(2)",
        main: "g(3)",
        probe: "'ds:' + g(4)",
    };
    let run = run_case_polled(Mode::Automatic, &case);
    assert_eq!(
        run.after_definition.functions_rejected, 0,
        "the declaring script must verify ({:?})",
        run.after_definition
    );
    assert!(
        run.after_definition.functions_compiled > 0,
        "the declaring script must compile ({:?})",
        run.after_definition
    );
    assert_eq!(run.main, "int:4", "g(3) == 4");
}

/// One audited `CASES` snippet per stack shuffle, with the *language's* answer
/// hard-coded. The differential suites only compare native against the
/// interpreter, so a shuffle whose two slots happen to hold equal values (or
/// that both engines get wrong the same way) would pass; here every engine —
/// the interpreter and all three JIT modes — must produce the hand-derived
/// value, so an engine that reorders the operands wrongly fails.
///
/// `perm4`/`perm5` are the reason this exists: their JIT table was
/// `[1,0,2,3]`/`[1,0,2,3,4]` instead of QuickJS's `[2,0,1,3]`/`[3,0,1,2,4]`,
/// and no case reached them. `dup2`/`nip1` have no shape (see `UNREACHABLE`).
///
/// Measured by mutating each table entry and re-running this test (2026-10-17,
/// after S3 2.5 lowered the ref-cell family): **15 of the 16 reachable shuffles
/// fail when their entry is permuted**. The remaining one, `dup`, cannot be —
/// it duplicates one slot, so every valid same-height variant is identical.
/// `rot3l`/`swap2` became observable only once `make_loc_ref`/`put_ref_value`
/// lowered: before that their snippets exited to the interpreter before
/// reaching the shuffle.
const SHUFFLE_ORACLES: &[(&str, &str, &str)] = &[
    // (shuffle, audited case, expected `main`)
    ("nip", "logical_assign_nip", "int:33"),
    ("dup", "dup_nullish", "int:4"),
    ("dup1", "swap2_computed_target", "int:7"),
    ("insert2", "insert2_field_assign_value", "int:8"),
    ("insert3", "insert3_element_assign_value", "int:6"),
    ("insert4", "insert4_super_assign_value", "int:10"),
    ("perm3", "perm3_spread_call_order", "int:145"),
    ("perm4", "array_element_postfix", "int:26"),
    ("perm5", "super_lvalue_shuffles", "int:6"),
    ("swap", "swap_for_in_target", "int:21"),
    ("swap2", "swap2_computed_target", "int:7"),
    ("rot3l", "rot3l_object_rest", "int:304"),
    ("rot3r", "rot3r_computed_field_target", "int:5"),
    ("rot4l", "super_lvalue_shuffles", "int:6"),
    ("rot5l", "super_lvalue_shuffles", "int:6"),
    ("dup3", "super_lvalue_shuffles", "int:6"),
];

/// The write shapes that came with the GAPS work, under the same absolute
/// oracle as the shuffles: a `for` update on a TDZ-protected local / on a
/// closure variable, and `super()` inside an arrow.
#[test]
fn gap_write_shapes_match_the_language() {
    for (name, expected) in [
        ("tdz_for_update", "int:4"),
        ("closure_var_for_update", "int:4"),
        ("captured_this_super_init", "int:9"),
    ] {
        let op_case = CASES
            .iter()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("{name} is not in CASES"));
        for mode in [Mode::Interpreter, Mode::Tier1, Mode::Tier2, Mode::Automatic] {
            let run = run_case_polled(mode, &op_case.case);
            assert_eq!(
                run.main,
                expected,
                "{name} under {}: wrong result",
                mode.label()
            );
            if mode != Mode::Interpreter {
                assert!(
                    run.after_main.native_entries > 0,
                    "{name} under {}: never ran native",
                    mode.label()
                );
            }
        }
    }
}

#[test]
fn every_shuffle_has_an_order_observable_working_case() {
    // `jit::shuffle_opcodes` is the real table; the oracle set plus the
    // (shape-less) `UNREACHABLE` entries must account for all of it, so adding
    // a shuffle without a case fails here.
    const SHAPE_LESS: &[&str] = &["dup2", "nip1"];
    let mut covered: Vec<&str> = SHUFFLE_ORACLES.iter().map(|(name, ..)| *name).collect();
    covered.extend_from_slice(SHAPE_LESS);
    for shuffle in zuqe::jit::shuffle_opcodes() {
        assert!(
            covered.contains(shuffle),
            "{shuffle} has no order-observable case (add one to SHUFFLE_ORACLES)"
        );
    }
    // Every oracle must name an audited case, and that case must actually
    // compile (so the shuffle's lowering runs natively, not just the opcode
    // being present in a rejected function's bytecode).
    let policies = policy_by_name();
    for (shuffle, case_name, expected) in SHUFFLE_ORACLES {
        let op_case = CASES
            .iter()
            .find(|case| case.name == *case_name)
            .unwrap_or_else(|| panic!("{shuffle} points at the unknown case {case_name}"));
        assert!(
            op_case.opcodes.contains(shuffle),
            "{case_name} does not claim {shuffle}"
        );
        assert!(
            !must_claim_reject(op_case, &policies),
            "{case_name} claims a Reject opcode, so {shuffle} never runs natively"
        );
        for mode in [Mode::Interpreter, Mode::Tier1, Mode::Tier2, Mode::Automatic] {
            let run = run_case_polled(mode, &op_case.case);
            assert_eq!(
                run.main,
                *expected,
                "{shuffle} ({case_name}) under {}: wrong result, the operands are reordered",
                mode.label()
            );
            if mode != Mode::Interpreter {
                assert!(
                    run.after_main.native_entries > 0,
                    "{shuffle} ({case_name}) under {}: never ran native",
                    mode.label()
                );
            }
        }
    }
}

/// `set_name` (the inferred name of a function *expression*) lowers too, so a
/// script that only *assigns* functions compiles as well — before this it was
/// rejected at policy time.
#[test]
fn a_script_with_a_function_expression_compiles() {
    let case = Case {
        dim: "Opcode",
        name: "function_expression_script",
        setup: "var g = function(n){ return n * 2; }; g(1);",
        warm: "g(2)",
        main: "g(3)",
        probe: "'fe:' + g(4)",
    };
    let run = run_case_polled(Mode::Automatic, &case);
    assert_eq!(
        run.after_definition.functions_rejected, 0,
        "the script must verify ({:?})",
        run.after_definition
    );
    assert!(
        run.after_definition.functions_compiled > 0,
        "the script must compile ({:?})",
        run.after_definition
    );
    assert_eq!(run.main, "int:6", "g(3) == 6");
}

/// Development audit for the claims: with `dump_bytecode` the engine prints the
/// bytecode of every function the cases define, so/// `cargo test --features jit,dump_bytecode --test jit bytecode_dump_audit -- --nocapture`
/// lists every opcode the suite really produces (grep the output for a claimed
/// name to check a claim).
#[cfg(feature = "dump_bytecode")]
#[test]
fn bytecode_dump_audit() {
    use zuqe::{EvalFlags, EvalName, EvalType, Runtime, ScopedContext};

    println!("OPLIST {}", zuqe::jit::opcode_names().join(" "));
    for op_case in CASES {
        println!("CLAIM {} {}", op_case.name, op_case.opcodes.join(" "));
        println!(";; case {}", op_case.name);
        let mut rt = Runtime::create().expect("runtime");
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .expect("context");
        let name = EvalName::Name(format!("audit-{}.js", op_case.name));
        ctx.eval(
            op_case.case.setup.as_bytes(),
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .expect("audit setup must run");
    }
}
