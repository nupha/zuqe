// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! M2b-2: per-opcode end-to-end gate. Every opcode in the generated policy
//! table is claimed by at least one real JS case. A case whose claimed opcodes
//! are all `Native`/`Helper` must compile and run native code with results
//! identical to the interpreter; a case claiming any `Reject` opcode must leave
//! that function in the interpreter and still agree with the interpreter.

#![cfg(feature = "jit")]

use std::collections::HashMap;

use crate::common::{Case, Mode, run_case_polled};
use zuqe::jit::{JitPolicy, opcode_names, opcode_policies};

/// Opcodes no JS program (and no compile pass) can emit today, so no JS case
/// can ever claim them. Verified against every emit site in `src/`:
/// `invalid` is the dispatcher's sentinel; `dup2` and `make_arg_ref` exist only
/// as interpreter dispatch; `nip1` survives only in commented-out emitter code.
/// Lowering for the first three is covered by
/// `jit::compiler::every_supported_opcode_lowers`, which builds bytecode by
/// hand. Keep this list justified line by line.
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
];

/// Opcodes that are reachable in principle but that no case in this suite
/// produces: eight are `Native`/`Helper` (their lowering is covered by
/// `jit::compiler::every_supported_opcode_lowers`) and two are `Reject` ops
/// inside `with` (`with_get_var`/`with_make_ref`/`with_delete_var` do have a JS
/// case). This is the honest e2e gap of M2b-2 — it must only shrink.
const GAPS: &[(&str, &str)] = &[
    (
        "add_loc",
        "the optimizer's `set_loc` rewrite consumes the pattern add_loc matches",
    ),
    (
        "dup3",
        "no JS shape found for the depth-3 keep-top lvalue path",
    ),
    (
        "perm5",
        "no JS shape found for the depth-3 keep-second lvalue path",
    ),
    (
        "put_loc_check",
        "no JS shape found that the optimizer rewrites into it",
    ),
    (
        "put_var_ref",
        "every closure write is folded into `set_var_ref` first",
    ),
    (
        "put_var_ref_check_init",
        "`this` as a captured var_ref initializer: no JS shape found",
    ),
    ("rot4l", "no JS shape found for the depth-3 lvalue swap"),
    (
        "rot5l",
        "no JS shape found for the depth-3 computed lvalue swap",
    ),
    ("with_get_ref", "`with` + reference-read: no JS shape found"),
    (
        "with_put_var",
        "`with` + reference-write: no JS shape found",
    ),
];

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
    for_of_and_for_in => [push_i32, fclosure, drop, array_from, return, put_var, put_ref_value, get_field, get_loc, put_loc, set_loc, get_arg, if_false, goto, for_in_start, for_of_start, for_in_next, for_of_next, iterator_close, add, nop]
        = "var __seed = 0; function f(o){ var __z = 0; var s = 0; for (var v of [1, 2, 3]) s += v; for (var k in o) s += k.length; return s; }",
                  "f({a: 1, bb: 2})",
          "f({ccc: 3})",
          "'it:' + f({a: 1})";
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
