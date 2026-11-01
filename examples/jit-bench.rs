// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Multi-mode benchmark matrix (M2b-6): interpreter / tier1 / tier2 /
//! automatic over 34 kernels ported from quickjs-jit
//! `benchmarks/scripts/*.js` — scalars, control flow, Float64, bit ops,
//! monomorphic and polymorphic properties, packed and typed arrays, iteration,
//! closures/calls, exceptions, JSON, regexp, strings, Map/Set/BigInt, classes.
//!
//! Run with: `cargo run --release --features jit --example jit-bench -- \
//!   --modes interpreter,tier1,tier2,automatic` (or `tests/jit/jit-bench-matrix.sh`).
//!
//! Two protocols (epic G4):
//! * `--protocol inproc` (default): every mode runs in this process, `ROUNDS`
//!   timed rounds each, median reported. Fast, and what CI runs.
//! * `--protocol process`: the reference implementation's protocol — `rounds`
//!   *fresh processes* per mode, **interleaved** (round by round) so drift in
//!   machine state hits every mode alike, plus a bootstrap confidence interval
//!   on the speedup. Slower; use it when a number matters (the full 34-kernel
//!   matrix at the default 30 rounds takes ~30 min).
//!
//! Protocol: 2 warm-up rounds discarded, then 7 timed rounds; the per-round
//! median is reported. Every mode must produce the same result per kernel
//! (a number or a string), asserted bit-exactly before that row is printed.
//! `tier2` records feedback and applies the speculative int specialization of
//! epic F1/F2.

use std::time::{Duration, Instant};

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSValue, Jit, JitConfig, JitMetrics, JitTierPolicy,
    Runtime, ScopedContext,
};

const WARMUP: usize = 2;
/// Odd so the median is an actual sample.
const ROUNDS: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Policy `Interpreter`: JIT attached but every hook inert.
    Interpreter,
    /// Forced baseline tier.
    Tier1,
    /// Forced optimizing tier: feedback-driven specialization (epic F1/F2).
    Tier2,
    /// Production default: pick the tier from hotness.
    Automatic,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Mode::Interpreter => "interpreter",
            Mode::Tier1 => "tier1",
            Mode::Tier2 => "tier2",
            Mode::Automatic => "automatic",
        }
    }

    const fn policy(self) -> JitTierPolicy {
        match self {
            Mode::Interpreter => JitTierPolicy::Interpreter,
            Mode::Tier1 => JitTierPolicy::Tier1,
            Mode::Tier2 => JitTierPolicy::Tier2,
            Mode::Automatic => JitTierPolicy::Automatic,
        }
    }

    /// The thresholds this run uses. The default is eager, so the single
    /// warm-up call in `define` queues the function and [`settle`] installs it
    /// before any timing starts; `--call-threshold` / `--loop-threshold` (and
    /// their tier-2 twins) turn the run into a production-threshold sweep.
    fn config(self, th: &Thresholds) -> JitConfig {
        JitConfig::builder()
            .call_threshold(th.call_threshold)
            .loop_threshold(th.loop_threshold)
            .tier2_call_threshold(th.tier2_call_threshold)
            .tier2_loop_threshold(th.tier2_loop_threshold)
            .tier_policy(self.policy())
            .build()
    }
}

/// The compile thresholds of one bench run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Thresholds {
    call_threshold: u32,
    loop_threshold: u32,
    tier2_call_threshold: u32,
    tier2_loop_threshold: u32,
}

impl Default for Thresholds {
    /// Eager (1/1): the bench measures the *code*, not the tiering. A
    /// production-threshold run passes `--call-threshold` / `--loop-threshold`.
    fn default() -> Self {
        Self {
            call_threshold: 1,
            loop_threshold: 1,
            tier2_call_threshold: 1024,
            tier2_loop_threshold: 2048,
        }
    }
}

/// Poll until the JIT is quiet: `queue_len == 0 && functions_compiling == 0`.
/// `poll` is non-blocking, so a single call may return with an attempt still in
/// flight; the timed window must start from installed code.
fn settle(jit: &Jit) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        jit.poll();
        let m = jit.metrics();
        if m.queue_len == 0 && m.functions_compiling == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "jit never settled: {m:?}");
        std::thread::sleep(Duration::from_micros(50));
    }
}

struct Kernel {
    name: &'static str,
    /// Defines `workload(...)` and warms it up once; must not throw.
    define: &'static str,
    /// The timed computation; returns the kernel's result value (number or string).
    work: &'static str,
}

// The ported `../quickjs-jit/benchmarks/scripts/*.js` kernels. Two upstream
// files are deliberately absent:
// * `exceptions-promises-async.js` — `async`/`await` stays `Reject` (todo 2.4.2)
//   and the harness cannot consume a Promise result, so it can neither compile
//   nor be compared across modes.
// * `numeric.js` — byte-identical to `scalar-loop.js`; one of the two is enough.
const KERNELS: &[Kernel] = &[
    // Iterative fib (the original jit-bench kernel), loop moved into workload.
    Kernel {
        name: "fib-iter",
        define: "function fib(n){ var a = 0, b = 1; for (var i = 0; i < n; i++){ var t = a + b; a = b; b = t; } return b; } \
                 function workload(){ var s = 0; for (var k = 0; k < 4000; k++) s = fib(30); return s; } workload();",
        work: "workload();",
    },
    // scalar-loop.js: integer induction + addition, one back edge.
    Kernel {
        name: "scalar-loop",
        define: "function workload(n, seed){ var sum = seed; for (var i = seed; i < n; i++) sum = sum + i; return sum; } workload(10, 1);",
        work: "workload(2000000, 1);",
    },
    // The same loop as the whole *eval*, the shape N3 (inline back-edge OSR)
    // exists for: no function entry is ever reached again, so the frame can only
    // reach native code by handing itself to the compiled loop header at a back
    // edge. `work` re-evaluates the script every round (each eval is a fresh
    // function object), so the compile is part of the measured round; the loop
    // count keeps it in the noise. `define` only exists because the harness
    // asserts a compile happened before timing.
    Kernel {
        name: "toplevel-loop-osr",
        define: "function osrWarm(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } osrWarm(10);",
        work: "var s = 1; for (var i = 1; i < 2000000; i++) s = (s + i) | 0; s;",
    },
    // quickjs-bitops.js: shifts, xor, int32 wrap.
    Kernel {
        name: "bitops",
        define: "function workload(n, seed){ var v = seed | 0; for (var i = 0; i < n; i++) v = ((v << 5) ^ (v >>> 3) ^ i) | 0; return v; } workload(10, 1);",
        work: "workload(1000000, 12345);",
    },
    // property-heavy.js: monomorphic property read/write, receiver as argument.
    Kernel {
        name: "property",
        define: "var point = { x: 0, y: 1 }; \
                 function workload(n, seed, p){ p.x = seed; p.y = 1; var toggle = 0; \
                   for (var i = 0; i < n; i++){ p.x = p.x + p.y; p.y = p.y + toggle; toggle = 1 - toggle; } \
                   return p.x + p.y; } workload(10, 1, point);",
        work: "workload(500000, 1, point);",
    },
    // packed-array-traversal.js: dense array sum with int32 wrap.
    Kernel {
        name: "array-sum",
        define: "var values = new Array(2000); \
                 for (var i = 0; i < values.length; i++) values[i] = (i * 17) & 0xffff; \
                 function workload(seed, a){ var s = seed; \
                   for (var r = 0; r < 500; r++){ s = seed; for (var i = 0; i < a.length; i++) s = (s + a[i]) | 0; } \
                   return s; } workload(0, values);",
        work: "workload(7, values);",
    },
    // The write half of the array element access (P1-1): a packed array is
    // written in the loop — the inline store — and the values are read back
    // once, so the kernel measures the store and not the read.
    Kernel {
        name: "array-store",
        define: "var store = new Array(1024); \
                 function workload(iterations, seed){ var s = seed; \
                   for (var i = 0; i < iterations; i++){ store[i & 1023] = (i * 17) & 0xffff; } \
                   return s + store[0]; } workload(10, 1);",
        work: "workload(2000000, 1);",
    },
    // The read/write pair of `a[i]++`: `get_array_el3` keeps the key for the
    // write, so an Int key must not be turned into a fresh property-key string
    // on every iteration (T11).
    Kernel {
        name: "array-post-inc",
        define: "function workload(iterations){ \
                   var post = new Array(1024); \
                   for (var i = 0; i < post.length; i++) post[i] = i; \
                   var s = 0; \
                   for (var i = 0; i < iterations; i++){ s += post[i & 1023]++; } \
                   return s + post[0]; } workload(10);",
        work: "workload(2000000);",
    },
    // T12 probe: a monomorphic constructor per iteration (`call_constructor`
    // is still the general helper; the own-field reads use the property guard).
    Kernel {
        name: "ctor-heavy",
        define: "function Vec(x, y){ this.x = x; this.y = y; } \
                 function workload(iterations){ var s = 0; \
                   for (var i = 0; i < iterations; i++){ var v = new Vec(i, s); s = (s + v.x + v.y) | 0; } \
                   return s; } workload(10);",
        work: "workload(2000000);",
    },
    // T12 control: the same loop with an object literal instead of `new`.
    Kernel {
        name: "ctor-control",
        define: "function workload(iterations){ var s = 0; \
                   for (var i = 0; i < iterations; i++){ var v = { x: i, y: s }; s = (s + v.x + v.y) | 0; } \
                   return s; } workload(10);",
        work: "workload(2000000);",
    },
    // T12 probe: a monomorphic `apply` per iteration.
    Kernel {
        name: "apply-heavy",
        define: "function add(a, b){ return (a + b) | 0; } \
                 function workload(iterations){ var args = [0, 0]; var s = 0; \
                   for (var i = 0; i < iterations; i++){ args[0] = s; args[1] = i & 7; s = add.apply(null, args); } \
                   return s; } workload(10);",
        work: "workload(1000000);",
    },
    // T12 control: the same loop and argument reads, but a direct call (the
    // direct path T12 would extend to `apply`).
    Kernel {
        name: "apply-control",
        define: "function add(a, b){ return (a + b) | 0; } \
                 function workload(iterations){ var args = [0, 0]; var s = 0; \
                   for (var i = 0; i < iterations; i++){ args[0] = s; args[1] = i & 7; s = add(args[0], args[1]); } \
                   return s; } workload(10);",
        work: "workload(1000000);",
    },
    // T13 control: the same method call with an **own** method on a global
    // receiver (the `get_field2` guard hits; `call_method` is the direct path).
    Kernel {
        name: "method-own-global",
        define: "var v = { x: 3, y: 4 }; \
                 v.dot = function (o){ return (this.x * o.x + this.y * o.y) | 0; }; \
                 var o = { x: 1, y: 2 }; \
                 function workload(iterations){ var s = 0; \
                   for (var i = 0; i < iterations; i++){ s = (s + v.dot(o)) | 0; } \
                   return s; } workload(10);",
        work: "workload(2000000);",
    },
    // T13 probe: the same call with the method on the prototype.
    Kernel {
        name: "proto-method-global",
        define: "function Vec(x, y){ this.x = x; this.y = y; } \
                 Vec.prototype.dot = function (o){ return (this.x * o.x + this.y * o.y) | 0; }; \
                 var v = new Vec(3, 4); var o = { x: 1, y: 2 }; \
                 function workload(iterations){ var s = 0; \
                   for (var i = 0; i < iterations; i++){ s = (s + v.dot(o)) | 0; } \
                   return s; } workload(10);",
        work: "workload(2000000);",
    },
    // T13 probe: a data property that lives on the prototype (the guard only
    // serves own data properties today, so every read is a helper call).
    Kernel {
        name: "proto-data",
        define: "function P(){} P.prototype.x = 1; P.prototype.y = 2; \
                 function workload(iterations){ var p = new P(); var s = 0; \
                   for (var i = 0; i < iterations; i++){ s = (s + p.x + p.y) | 0; } \
                   return s; } workload(10);",
        work: "workload(2000000);",
    },
    // T13 probe: a method on the prototype (`get_field2` misses the own guard,
    // then `call_method` runs).
    Kernel {
        name: "proto-method",
        define: "function Vec(x, y){ this.x = x; this.y = y; } \
                 Vec.prototype.dot = function (o){ return (this.x * o.x + this.y * o.y) | 0; }; \
                 function workload(iterations){ var v = new Vec(3, 4); var o = { x: 1, y: 2 }; var s = 0; \
                   for (var i = 0; i < iterations; i++){ s = (s + v.dot(o)) | 0; } \
                   return s; } workload(10);",
        work: "workload(2000000);",
    },
    // scalar-control-flow.js: both branch exits establish numeric values whose
    // join feeds the same multiply.
    Kernel {
        name: "scalar-control-flow",
        define: "function workload(iterations, seed){ var sum = seed; for (var i = seed; i < iterations; i++){ var x, y; \
                 if (i & 1) { x = i + seed; y = i - seed; } else { x = i - seed; y = i + seed; } \
                 sum = (sum + ((x * y) & 1023)) | 0; } return sum; } workload(10, 1);",
        work: "workload(3000000, 1);",
    },
    // scalar-expressions.js: repeated nested numeric expressions.
    Kernel {
        name: "scalar-expressions",
        define: "function workload(iterations, seed){ var sum = seed; for (var i = seed; i < iterations; i++){ var x = i & 7; \
                 sum += ((x + 1) * (x + 3)) + ((x + 1) * (x + 3)); } return sum; } workload(10, 1);",
        work: "workload(4000000, 1);",
    },
    // quickjs-fibonacci.js: integer induction with int32 wrap.
    Kernel {
        name: "fib-iter-i32",
        define: "function workload(n, seed){ var a = seed | 0, b = 1; for (var i = 0; i < n; i++){ var next = (a + b) | 0; a = b; b = next; } return a; } workload(10, 1);",
        work: "workload(3000000, 12345);",
    },
    // float64-dense.js: dense Float64 arithmetic, no property lookups.
    Kernel {
        name: "float64-dense",
        define: "function float64Kernel(iterations, seed){ var two = 2; var p3 = two * two * two; var p6 = p3 * p3; \
                 var p10 = p6 * p3 * two; var p13 = p10 * p3; var p20 = p10 * p10; \
                 var x = seed / p10 + 1 / p3; var y = 1 + 1 / p20; var sum = 0; \
                 for (var i = 0; i < iterations; i++){ x = x * (1 + 1 / p13) + 1 / p10; y = (y + x / p20) / (1 + 1 / p20); sum = sum + x * y + x / (i + 1); } \
                 return sum; } function workload(iterations, seed){ return float64Kernel(iterations - 0.000001, seed + 0.000001); } workload(10, 1);",
        work: "workload(400000, 1);",
    },
    // property-polymorphic.js: the `x`/`y` sites each see three shapes.
    Kernel {
        name: "property-polymorphic",
        define: "var SHAPES = { a: { x: 1, y: 10 }, b: { y: 20, x: 2, z: 5 }, c: { tag: 7, x: 3, y: 30 } }; \
                 function workload(n, seed, shapes){ var sum = seed; var p = shapes.a; for (var i = 0; i < n; i++){ var k = i % 3; \
                 if (k === 0) p = shapes.a; else if (k === 1) p = shapes.b; else p = shapes.c; \
                 sum = sum + p.x + p.y; } return sum; } workload(10, 1, SHAPES);",
        work: "workload(500000, 1, SHAPES);",
    },
    // ── 4.1: the rest of ../quickjs-jit/benchmarks/scripts (see the note above
    // the list for the two upstream files that are not ported). ──
    // adversarial.js: a branch whose rare arm exits to a builtin call.
    Kernel {
        name: "adversarial",
        define: "function workload(iterations){ var value = 0; \
                 for (var i = 0; i < iterations; i++) value = (i % 7) ? (value + i) : String(value).length; \
                 return String(value); } workload(10);",
        work: "workload(450000);",
    },
    // arrays-typed.js: the two hot callees (packed array sum, typed convert),
    // batched because the reference's `workload` is allocation-dominated.
    Kernel {
        name: "arrays-typed",
        define: "var values = new Array(2000); for (var i = 0; i < values.length; i++) values[i] = (i * 17 + 1) & 0xffff; \
                 var ints = new Int32Array(values); var floats = new Float64Array(2000); \
                 function sumElements(values){ var sum = 0; for (var i = 0; i < values.length; i++) sum = (sum + values[i]) | 0; return sum; } \
                 function convertAndSum(ints, floats){ var mixed = 0.0; for (var i = 0; i < ints.length; i++){ floats[i] = ints[i] * 0.25 + 0.5; mixed += floats[i]; } return mixed; } \
                 function workload(rounds, vals, src, dst){ var s = 0; for (var r = 0; r < rounds; r++) s = s + sumElements(vals) + convertAndSum(src, dst); return s; } \
                 workload(2, values, ints, floats);",
        work: "workload(80, values, ints, floats);",
    },
    // call-heavy.js: monomorphic direct call per iteration; the `call` helper
    // (D-6) runs the callee on the caller's frame, so no per-iteration frame
    // exit/interpreter dispatch/re-entry is paid.
    Kernel {
        name: "call-heavy",
        define: "function increment(value, delta){ return value + delta; } \
                 function workload(iterations, seed, target){ var value = seed; var delta = 0; \
                 for (var i = 0; i < iterations; i++){ value = target(value, delta); if (delta >= 8) delta = 0; else delta = delta + 1; } return value; } \
                 workload(10, 1, increment);",
        work: "workload(300000, 1, increment);",
    },
    // calls-closures.js: a curried closure allocated and called twice per
    // iteration (closure captures live across the calls).
    Kernel {
        name: "calls-closures",
        define: "function workload(iterations){ var add = x => y => (x + y) | 0; var sum = 0; \
                 for (var i = 0; i < iterations; i++) sum = add(sum)(i & 31); return String(sum); } workload(10);",
        work: "workload(40000);",
    },
    // calls-recursion-closures.js: recursion plus a four-deep call chain onto
    // a closure that mutates a captured variable.
    Kernel {
        name: "calls-recursion-closures",
        define: "function recursiveSum(n){ return n <= 0 ? 0 : n + recursiveSum(n - 1); } \
                 function call4(fn, value){ return fn(value); } function call3(fn, value){ return call4(fn, value); } \
                 function call2(fn, value){ return call3(fn, value); } function call1(fn, value){ return call2(fn, value); } \
                 function workload(iterations, seed){ var captured = seed; var update = value => (captured = (captured + value) | 0); \
                 for (var i = 0; i < iterations; i++) call1(update, recursiveSum(i & 7)); return captured; } workload(10, 1);",
        work: "workload(40000, 1);",
    },
    // collections.js: grow a packed array with push, then drain it with for-of.
    Kernel {
        name: "collections",
        define: "function workload(iterations){ var values = []; for (var i = 0; i < iterations; i++) values.push((i * 17) & 1023); \
                 var sum = 0; for (const value of values) sum += value; return String(sum); } workload(10);",
        work: "workload(200000);",
    },
    // exceptions-sync.js: a primitive throw every 16th iteration, a finally on
    // every iteration and a helper-raised TypeError every 64th. The throw
    // sites must leave the native frame (the catch scan is the interpreter's),
    // so the exception-dense loop stays a negative kernel (2.6).
    Kernel {
        name: "exceptions-sync",
        define: "function workload(iterations, seed){ var table = [{ w: 1 }, { w: 2 }, null, { w: 3 }]; \
                 var caught = 0; var total = seed | 0; var finals = 0; var missing = 0; \
                 for (var i = 0; i < iterations; i++){ try { if ((i & 15) === 0) throw i; total = (total + i) | 0; } \
                 catch (value) { caught += value; } finally { finals++; } \
                 if ((i & 63) === 1){ try { total = (total + table[(i >> 6) & 3].w) | 0; } catch (error) { missing++; } } } \
                 return caught + ':' + total + ':' + finals + ':' + missing; } workload(10, 1);",
        work: "workload(300000, 1);",
    },
    // fibonacci-iterative.js: the fib batched in JS so the call boundary does
    // not dominate the compute evidence.
    Kernel {
        name: "fibonacci-iterative",
        define: "function workload(iterations, seed){ var result = seed; \
                 for (var batch = seed; batch < iterations; batch++){ var a = seed; var b = 1; \
                 for (var i = seed; i < 40; i++){ var next = a + b; a = b; b = next; } result = a; } return result; } \
                 workload(10, 1);",
        work: "workload(20000, 1);",
    },
    // fibonacci-recursive.js: the reference's single fib(20) is microseconds, so
    // it is batched; the measured unit stays one recursive fib.
    Kernel {
        name: "fibonacci-recursive",
        define: "function fibonacci(n){ if (n < 2) return n; return fibonacci(n - 1) + fibonacci(n - 2); } \
                 function workload(iterations, seed){ var result = seed; for (var b = seed; b < iterations; b++) result = fibonacci(20) + seed; return result; } \
                 workload(1, 1);",
        work: "workload(20, 1);",
    },
    // IMP-3 probe: a thin callee (the call overhead is route B's ceiling).
    Kernel {
        name: "probe-thin-call",
        define: "function thin(x){ return x; } function workload(iterations, seed){ var s = seed; for (var i = 0; i < iterations; i++) s = thin(s); return s; } workload(10, 1);",
        work: "workload(20000000, 1);",
    },
    // The same loop without the call: the difference is the per-call cost.
    Kernel {
        name: "probe-thin-nocall",
        define: "function workload(iterations, seed){ var s = seed; for (var i = 0; i < iterations; i++) s = s; return s; } workload(10, 1);",
        work: "workload(20000000, 1);",
    },
    // T7.1: a callee that loops. The inline path copies the loop and pays one
    // safepoint per back edge, but no frame/call protocol per invocation.
    Kernel {
        name: "probe-loop-call",
        define: "function sumTo(n){ var s = 0; for (var i = 0; i < n; i++) { s = s + i; } return s; } \
                 function workload(iterations, seed){ var s = seed; for (var i = 0; i < iterations; i++) s = (s + sumTo(8)) | 0; return s; } \
                 workload(10, 1);",
        work: "workload(2000000, 1);",
    },
    // T7.4: an object literal inside the callee, no loop there. The copied
    // body allocates through the caller's scope and returns a register value.
    Kernel {
        name: "probe-alloc-call",
        define: "function make(x){ var o = {}; return x + 1; } \
                 function workload(iterations, seed){ var s = seed; for (var i = 0; i < iterations; i++) s = (s + make(i)) | 0; return s; } \
                 workload(10, 1);",
        work: "workload(2000000, 1);",
    },
    // T7.3: a try/catch body whose fast path never throws. The copied body
    // runs inline; a throw would bail to the general path.
    Kernel {
        name: "probe-try-catch",
        define: "function guarded(x){ try { return x + 1; } catch (e) { return 0; } } \
                 function workload(iterations, seed){ var s = seed; for (var i = 0; i < iterations; i++) s = (s + guarded(i)) | 0; return s; } \
                 workload(10, 1);",
        work: "workload(2000000, 1);",
    },
    // T7.2: a two-level call chain. The callee passed as an argument keeps the
    // copied bodies free of globals/closures, so both levels inline.
    Kernel {
        name: "probe-nested-call",
        define: "function leaf(x){ return x + 1; } \
                 function mid(f, x){ return f(x) * 2; } \
                 function workload(iterations, seed){ var s = seed; for (var i = 0; i < iterations; i++) s = (s + mid(leaf, i)) | 0; return s; } \
                 workload(10, 1);",
        work: "workload(2000000, 1);",
    },
    // float64array-traversal.js: Int32Array -> Float64Array conversion, both
    // buffers handed in as one argument object.
    Kernel {
        name: "float64array-traversal",
        define: "var f64ints = new Int32Array(2000); var f64floats = new Float64Array(2000); \
                 for (var i = 0; i < f64ints.length; i++) f64ints[i] = (i * 17) & 0xffff; \
                 var f64buffers = { ints: f64ints, floats: f64floats }; \
                 function convertAndSum(seed, buffers){ var ints = buffers.ints; var floats = buffers.floats; var sum = seed; \
                 for (var i = 0; i < ints.length; i++){ floats[i] = ints[i] * 0.25 + 0.5; sum += floats[i]; } return sum; } \
                 function workload(rounds, seed){ var s = 0; for (var r = 0; r < rounds; r++) s += convertAndSum(seed, f64buffers); return s; } \
                 workload(2, 1);",
        work: "workload(100, 1);",
    },
    // for-of-array.js: unlike collections.js the array is built once, so the
    // timed part is the iterator protocol alone.
    Kernel {
        name: "for-of-array",
        define: "function buildValues(iterations){ var values = []; for (var i = 0; i < iterations; i++) values.push((i * 17) & 1023); return values; } \
                 var forOfValues = buildValues(20000); \
                 function sumForOf(values){ var sum = 0; for (const value of values) sum += value; return sum; } \
                 function workload(rounds){ var s = 0; for (var r = 0; r < rounds; r++) s += sumForOf(forOfValues); return String(s); } \
                 workload(1);",
        work: "workload(50);",
    },
    // generic-call-entry.js: a non-numeric (Bool) argument excludes the
    // numeric direct-call signature, so this measures the generic entry.
    Kernel {
        name: "generic-call-entry",
        define: "function incrementIf(value, enabled){ if (enabled) return value + 1; return value; } \
                 function workload(iterations, seed, target){ var value = seed; \
                 for (var i = 0; i < iterations; i++) value = target(value, true); return value; } workload(10, 1, incrementIf);",
        work: "workload(400000, 1, incrementIf);",
    },
    // generic-call-fallback.js: the callee mutates its argument, keeping it
    // outside the pure direct-leaf shape.
    Kernel {
        name: "generic-call-fallback",
        define: "function incrementAndRecord(value, enabled, state){ state.calls = state.calls + 1; if (enabled) return value + 1; return value; } \
                 var gcfState = { target: incrementAndRecord, calls: 0 }; \
                 function workload(iterations, seed, state){ state.calls = 0; var target = state.target; var value = seed; \
                 for (var i = 0; i < iterations; i++) value = target(value, true, state); return value + state.calls; } \
                 workload(10, 1, gcfState);",
        work: "workload(300000, 1, gcfState);",
    },
    // host-compute.js: gpui-shell's layout kernel, verbatim. The reference's
    // one-render call is microseconds, so `workload` repeats it; the template
    // string of the reference is dropped (the number is the observable).
    Kernel {
        name: "host-compute",
        define: "function layoutKernel(batches, seed){ var checksum = seed; \
                 for (var batch = 0; batch < batches; batch += 1){ var a = 0; var b = 1; \
                 for (var i = 0; i < 40; i += 1){ var next = a + b; a = b; b = next; } checksum = b; } return checksum; } \
                 function workload(renders, seed){ var last = 0; for (var r = 0; r < renders; r++) last = layoutKernel(100, seed); return last; } \
                 workload(1, 0);",
        work: "workload(200, 0);",
    },
    // int32array-traversal.js: Int32Array element reads only.
    Kernel {
        name: "int32array-traversal",
        define: "var i32values = new Int32Array(2000); for (var i = 0; i < i32values.length; i++) i32values[i] = (i * 17) & 0xffff; \
                 function sumInt32(seed, values){ var sum = seed; for (var i = 0; i < values.length; i++) sum = (sum + values[i]) | 0; return sum; } \
                 function workload(rounds, seed, values){ var s = 0; for (var r = 0; r < rounds; r++) s = sumInt32(seed, values); return s; } \
                 workload(2, 1, i32values);",
        work: "workload(150, 1, i32values);",
    },
    // json-codec.js: stringify + parse per iteration; both are builtin calls,
    // so this measures the surrounding native frame, not JSON itself.
    Kernel {
        name: "json-codec",
        define: "function workload(iterations, seed){ var payload = { name: 'rquickjs', enabled: true, values: [1, 2.5, null, 'four'], \
                 nested: { seed: seed, tags: ['jit', 'quickjs', 'benchmark'] } }; var total = 0; var last = ''; \
                 for (var i = 0; i < iterations; i++){ payload.nested.seed = i + seed; last = JSON.stringify(payload); \
                 var decoded = JSON.parse(last); total += decoded.values.length + decoded.nested.tags.length + decoded.nested.seed; } \
                 return total + ':' + last.length; } workload(10, 1);",
        work: "workload(3000, 1);",
    },
    // map-set-bigint.js: Map/Set churn plus 64-bit BigInt arithmetic.
    Kernel {
        name: "map-set-bigint",
        define: "function workload(iterations, seed){ var map = new Map(); var set = new Set(); var big = BigInt(seed); \
                 for (var i = 0; i < iterations; i++){ var key = (i * 17) & 255; map.set(key, (map.get(key) || 0) + 1); set.add(key); \
                 big = (big * 33n + BigInt(key + 1)) & 0xffffffffffffffffn; } var count = 0; for (const value of map.values()) count += value; \
                 return count + ':' + set.size + ':' + big.toString(16); } workload(10, 1);",
        work: "workload(30000, 1);",
    },
    // methods-dynamic.js: constructor + prototype methods, destructuring,
    // `**`, `instanceof`, `in`, and a polymorphic `typeof` dispatch.
    Kernel {
        name: "methods-dynamic",
        define: "function Vec(x, y){ this.x = x; this.y = y; } \
                 Vec.prototype.dot = function (other){ return this.x * other.x + this.y * other.y; }; \
                 Vec.prototype.scale = function (k){ return new Vec(this.x * k, this.y * k); }; \
                 function describe(value){ if (typeof value === 'undefined') return 0; if (typeof value === 'function') return 1; \
                 if (value instanceof Vec) return 2; if (typeof value !== 'object') return 3; if ('kind' in value) return 4 + value.kind; return 7; } \
                 function tally(counts, key){ counts[key] += 1; } \
                 function workload(iterations, seed){ var values = [undefined, describe, new Vec(1, 2), { kind: 1 }, { other: 1 }, 'text', { kind: 2 }]; \
                 var counts = [0, 0, 0, 0, 0, 0, 0, 0]; var sum = 0; \
                 for (var i = 0; i < iterations; i++){ var v = new Vec(i & 7, seed + 1); var { x, y } = v.scale(2); \
                 sum += v.dot({ x: x, y: y }) + (x ** 2) % 5; tally(counts, describe(values[i % values.length])); } \
                 return sum + ':' + counts.join(','); } workload(10, 1);",
        work: "workload(8000, 1);",
    },
    // mixed-quotes.js: 96 quotes per render, sorted with a comparator, then
    // flattened into a report string. The reference does one render per call,
    // which is microseconds, so `workload` repeats the render.
    Kernel {
        name: "mixed-quotes",
        define: "function quoteScore(seed, index){ var previous = seed; var current = seed + index + 1; var aggregate = 0; \
                 for (var sample = 0; sample < 32; sample += 1){ var next = previous + current; previous = current; current = next; \
                 aggregate += current & 2047; } return aggregate; } \
                 function compareQuotes(left, right){ return right.score - left.score; } \
                 function render(seed){ var quotes = []; var total = 0; \
                 for (var index = 0; index < 96; index += 1){ var score = quoteScore(seed, index); total += score; quotes.push({ index: index, score: score }); } \
                 quotes.sort(compareQuotes); var visible = ''; \
                 for (var rank = 0; rank < 12; rank += 1){ var quote = quotes[rank]; visible += 'SYM' + quote.index + ':' + quote.score + ';'; } \
                 return 'total:' + total + ';' + visible; } \
                 function workload(renders, seed){ var visible = ''; for (var r = 0; r < renders; r++) visible = render(seed); return visible; } \
                 workload(1, 17);",
        work: "workload(100, 17);",
    },
    // objects-polymorphic.js: four shapes per site, one prototype-less.
    Kernel {
        name: "objects-polymorphic",
        define: "function workload(iterations, seed){ var objects = []; \
                 for (var i = 0; i < iterations; i++){ var object; switch (i & 3){ case 0: object = { x: i, y: seed, kind: 0 }; break; \
                 case 1: object = { y: seed, x: i, extra: 1, kind: 1 }; break; case 2: object = { x: i, kind: 2 }; object.y = seed; break; \
                 default: object = Object.create(null); object.kind = 3; object.x = i; object.y = seed; } \
                 object.x += object.kind; objects.push(object); } var sum = 0; \
                 for (var j = 0; j < objects.length; j++) sum += objects[j].x + objects[j].y; return sum; } workload(10, 1);",
        work: "workload(20000, 1);",
    },
    // quickjs-int-arith.js: int_arith out of QuickJS's microbench. The
    // Math.max call is outside the loops; see handover 6.20 for why the tier2
    // column used to be slow here.
    Kernel {
        name: "quickjs-int-arith",
        define: "function workload(iterations){ var globalResult = 0; var outer = Math.max(1, iterations >> 7); \
                 for (var j = 0; j < outer; j++){ var sum = 0; for (var i = 0; i < 1000; i++) sum += i * i; globalResult += sum; } \
                 return String(globalResult); } workload(10);",
        work: "workload(128000);",
    },
    // strings-json.js: rope growth via fromCharCode, then a JSON report.
    Kernel {
        name: "strings-json",
        define: "function workload(iterations){ var text = ''; \
                 for (var i = 0; i < iterations; i++) text += String.fromCharCode(97 + (i % 26)); \
                 return JSON.stringify({ length: text.length, first: text.slice(0, 8) }); } workload(10);",
        work: "workload(20000);",
    },
    // strings-regexp.js: build a delimited string, then a global match and a
    // callback-free replace. The regexp literals are `regexp` opcodes, which
    // have no lowering yet (2.6), so they leave the native frame.
    Kernel {
        name: "strings-regexp",
        define: "function workload(iterations, seed){ var pieces = []; \
                 for (var i = 0; i < iterations; i++) pieces.push('item-' + ((i + seed) % 97) + ':' + (i % 13)); \
                 var text = pieces.join('|'); var matches = text.match(/item-(?:1[0-9]|2[0-9]|3[0-2]):\\d+/g) || []; \
                 var replaced = text.replace(/item-(\\d+):(\\d+)/g, '$2@$1'); \
                 return matches.length + ':' + replaced.length + ':' + replaced.slice(-16); } workload(10, 1);",
        work: "workload(6000, 1);",
    },
    // typed-convert-traversal.js: both typed arrays reach one callee; only the
    // source's length bounds the loop.
    Kernel {
        name: "typed-convert-traversal",
        define: "var tcInts = new Int32Array(2000); var tcFloats = new Float64Array(2000); \
                 for (var i = 0; i < tcInts.length; i++) tcInts[i] = (i * 17) & 0xffff; \
                 function convertAndSum(ints, floats){ var mixed = 0.0; for (var i = 0; i < ints.length; i++){ \
                 floats[i] = ints[i] * 0.25 + 0.5; mixed += floats[i]; } return mixed; } \
                 function workload(rounds, seed, ints){ var s = seed; for (var r = 0; r < rounds; r++) s += convertAndSum(ints, tcFloats); return s; } \
                 workload(2, 1, tcInts);",
        work: "workload(100, 1, tcInts);",
    },
];

/// Parse `--modes interpreter,tier1,tier2,automatic`; default is every
/// runnable mode. Unknown modes exit non-zero.
fn parse_modes(args: &[String]) -> Vec<Mode> {
    let mut selected = vec![Mode::Interpreter, Mode::Tier1, Mode::Tier2, Mode::Automatic];
    let Some(i) = args.iter().position(|a| a == "--modes") else {
        return selected;
    };
    let Some(list) = args.get(i + 1) else {
        eprintln!("--modes needs a comma list, e.g. --modes interpreter,tier1,tier2,automatic");
        std::process::exit(2);
    };
    selected.clear();
    for tok in list.split(',') {
        let mode = match tok.trim() {
            "" => continue,
            "interpreter" => Mode::Interpreter,
            "tier1" => Mode::Tier1,
            "tier2" => Mode::Tier2,
            "automatic" => Mode::Automatic,
            other => {
                eprintln!(
                    "unknown mode {other:?}: valid modes are interpreter, tier1, tier2, automatic"
                );
                std::process::exit(2);
            }
        };
        if !selected.contains(&mode) {
            selected.push(mode);
        }
    }
    selected
}

fn eval(ctx: &mut Context, src: &[u8]) -> JSValue {
    ctx.eval(
        src,
        &EvalName::Name("jit-bench.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap()
}

/// Parse the compile thresholds: `--call-threshold N`, `--loop-threshold N`,
/// `--tier2-call-threshold N`, `--tier2-loop-threshold N`. Unset flags keep
/// the default (eager 1/1 for the first gate).
fn parse_thresholds(args: &[String]) -> Thresholds {
    let mut th = Thresholds::default();
    let set = |flag: &str, slot: &mut u32| {
        let Some(i) = args.iter().position(|a| a == flag) else {
            return;
        };
        let Some(value) = args.get(i + 1).and_then(|v| v.parse::<u32>().ok()) else {
            eprintln!("{flag} needs a number");
            std::process::exit(2);
        };
        *slot = value.max(1);
    };
    set("--call-threshold", &mut th.call_threshold);
    set("--loop-threshold", &mut th.loop_threshold);
    set("--tier2-call-threshold", &mut th.tier2_call_threshold);
    set("--tier2-loop-threshold", &mut th.tier2_loop_threshold);
    th
}

/// The kernels to run: `--kernel <name>` picks one (for a focused
/// measurement); without it every kernel runs.
fn parse_kernels(args: &[String]) -> Vec<&'static Kernel> {
    let Some(i) = args.iter().position(|a| a == "--kernel") else {
        return KERNELS.iter().collect();
    };
    let Some(name) = args.get(i + 1) else {
        eprintln!("--kernel needs a kernel name");
        std::process::exit(2);
    };
    let kernel = KERNELS
        .iter()
        .find(|k| k.name == name.as_str())
        .unwrap_or_else(|| {
            eprintln!("unknown kernel {name:?}");
            std::process::exit(2);
        });
    vec![kernel]
}

/// Result digest used for the cross-mode equality check. Kernels may return a
/// number or a string (many reference kernels build a report string), so a
/// number keeps its exact bit pattern and a string is folded into a 64-bit FNV
/// over its code units. Computed *outside* the timed window so it never lands
/// in a round's duration.
fn digest(v: &JSValue) -> u64 {
    // Normalize Int/Float64 tags: numeric equality is what the bench checks
    // (tag-exact equality is covered by tests/jit_differential.rs).
    if let Some(f) = v.as_int().map(|i| i as f64).or_else(|| v.as_float()) {
        return f.to_bits();
    }
    let s = v
        .as_string(true)
        .expect("kernel must return a number or a string");
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for i in 0..s.len() {
        h ^= s.get(i).unwrap_or(0) as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The site-level counters a run ends with (T18): the inline guards' hits and
/// misses, the direct-call path's, the frame inlining, and the speculative
/// int guards' misses. They are plain `JitMetrics` fields — always compiled in,
/// only *printed* here.
#[derive(Clone, Copy, Default)]
struct SiteCounts {
    property_hits: u64,
    property_misses: u64,
    global_hits: u64,
    global_misses: u64,
    inline_hits: u64,
    inline_misses: u64,
    frame_inline: u64,
    spec_guard_misses: u64,
    scope_local_peak: u64,
}

impl SiteCounts {
    /// The `--one` field names, in [`Self::values`] order.
    const KEYS: [&str; 9] = [
        "property_guard_hits",
        "property_guard_misses",
        "global_guard_hits",
        "global_guard_misses",
        "inline_call_hits",
        "inline_call_misses",
        "frame_inline_hits",
        "specialized_guard_misses",
        "scope_local_peak",
    ];

    fn of(m: &JitMetrics) -> Self {
        Self {
            property_hits: m.property_guard_hits,
            property_misses: m.property_guard_misses,
            global_hits: m.global_guard_hits,
            global_misses: m.global_guard_misses,
            inline_hits: m.inline_call_hits,
            inline_misses: m.inline_call_misses,
            frame_inline: m.frame_inline_hits,
            spec_guard_misses: m.specialized_guard_misses,
            scope_local_peak: m.scope_local_peak,
        }
    }

    /// The counters in [`Self::KEYS`] order (the `--one` wire format).
    const fn values(self) -> [u64; 9] {
        [
            self.property_hits,
            self.property_misses,
            self.global_hits,
            self.global_misses,
            self.inline_hits,
            self.inline_misses,
            self.frame_inline,
            self.spec_guard_misses,
            self.scope_local_peak,
        ]
    }

    fn from_values(v: [u64; 9]) -> Self {
        Self {
            property_hits: v[0],
            property_misses: v[1],
            global_hits: v[2],
            global_misses: v[3],
            inline_hits: v[4],
            inline_misses: v[5],
            frame_inline: v[6],
            spec_guard_misses: v[7],
            scope_local_peak: v[8],
        }
    }

    fn accumulate(&mut self, other: &SiteCounts) {
        self.property_hits += other.property_hits;
        self.property_misses += other.property_misses;
        self.global_hits += other.global_hits;
        self.global_misses += other.global_misses;
        self.inline_hits += other.inline_hits;
        self.inline_misses += other.inline_misses;
        self.frame_inline += other.frame_inline;
        self.spec_guard_misses += other.spec_guard_misses;
        // A high-water mark: the max over the rounds, not a sum.
        self.scope_local_peak = self.scope_local_peak.max(other.scope_local_peak);
    }
}

/// The `key=value` half `--one` appends to its line, so a fresh process
/// reports the same counters the in-process matrix prints.
fn site_count_fields(c: &SiteCounts) -> String {
    SiteCounts::KEYS
        .iter()
        .zip(c.values())
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The five summary lines both protocols print at the end of a matrix run.
fn site_count_lines(entries: &[(Mode, SiteCounts)]) -> Vec<String> {
    let pair = |name: &str, get: fn(&SiteCounts) -> (u64, u64)| {
        format!(
            "{name}: {}",
            entries
                .iter()
                .map(|(m, c)| {
                    let (hits, misses) = get(c);
                    format!("{}={hits}/{misses}", m.label())
                })
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let single = |name: &str, get: fn(&SiteCounts) -> u64| {
        format!(
            "{name}: {}",
            entries
                .iter()
                .map(|(m, c)| format!("{}={}", m.label(), get(c)))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    vec![
        pair("property_guard (hits/misses)", |c| {
            (c.property_hits, c.property_misses)
        }),
        pair("global_guard (hits/misses)", |c| {
            (c.global_hits, c.global_misses)
        }),
        pair("inline_call (hits/misses)", |c| {
            (c.inline_hits, c.inline_misses)
        }),
        single("frame_inline_hits", |c| c.frame_inline),
        single("specialized_guard_misses", |c| c.spec_guard_misses),
        single("scope_local_peak", |c| c.scope_local_peak),
    ]
}

struct Run {
    result: u64,
    median: Duration,
    compiled: u64,
    /// The guard/inline counters of the whole run (warm-up + timed rounds).
    sites: SiteCounts,
    /// Functions promoted to the optimizing tier (TH2's second gate).
    promotions: u64,
    /// Sites that got a tier-2 speculative fast path.
    specialized: u64,
    /// Nanoseconds spent compiling (verification + codegen).
    compile_nanos: u64,
    /// Compile attempts that were measured.
    compile_attempts: u64,
    /// Mappings the code cache retired for quota.
    retired: u64,
    /// Installed mappings a recompile replaced.
    replaced: u64,
    /// Installations retired or replaced without ever running.
    never_entered: u64,
}

fn run_kernel(kernel: &Kernel, mode: Mode, th: &Thresholds) -> Run {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, mode.config(th)).unwrap();
    eval(&mut ctx, kernel.define.as_bytes());
    settle(&jit);

    // Verify run first, then warm-up (discarded), then timed rounds. The
    // verify run is where a production-threshold run crosses the hotness
    // gate, so the JIT is settled again before the timed window: the rounds
    // must measure installed code, not a compile.
    let result = digest(&eval(&mut ctx, kernel.work.as_bytes()));
    settle(&jit);
    let metrics = jit.metrics();
    let compiled = metrics.functions_compiled;
    match mode {
        Mode::Interpreter => assert_eq!(
            compiled, 0,
            "{}: interpreter policy must not compile",
            kernel.name
        ),
        _ => assert!(
            compiled > 0,
            "{}: {} did not compile the kernel",
            kernel.name,
            mode.label()
        ),
    }
    for _ in 0..WARMUP {
        std::hint::black_box(eval(&mut ctx, kernel.work.as_bytes()));
    }
    let mut samples: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let start = Instant::now();
        let r = eval(&mut ctx, kernel.work.as_bytes());
        let elapsed = start.elapsed();
        // Every timed round must still produce the verified result.
        assert_eq!(
            digest(&r),
            result,
            "{}: {} timed round drifted",
            kernel.name,
            mode.label()
        );
        samples.push(elapsed);
    }
    samples.sort_unstable();
    // The site counters keep growing through the warm-up and the timed rounds,
    // so they are read at the end of the run, not with the compile metrics.
    let sites = SiteCounts::of(&jit.metrics());
    Run {
        result,
        median: samples[ROUNDS / 2],
        compiled,
        sites,
        promotions: metrics.tier2_promotions,
        specialized: metrics.specialized_ops,
        compile_nanos: metrics.compile_nanos,
        compile_attempts: metrics.compile_attempts,
        retired: metrics.code_retired,
        replaced: metrics.code_replaced,
        never_entered: metrics.code_never_entered,
    }
}

// ── epic G4: fresh-process, interleaved, bootstrap protocol ──

/// Rounds (fresh processes per mode) used by `--protocol process`.
const PROCESS_ROUNDS: usize = 30;
/// Bootstrap resamples for the speedup confidence interval.
const BOOTSTRAP: usize = 10_000;

fn mode_by_name(name: &str) -> Option<Mode> {
    match name {
        "interpreter" => Some(Mode::Interpreter),
        "tier1" => Some(Mode::Tier1),
        "tier2" => Some(Mode::Tier2),
        "automatic" => Some(Mode::Automatic),
        _ => None,
    }
}

/// The child half of `--protocol process`: one kernel, one mode, one process.
/// Prints `ms=<f64> result=<f64> compiled=<u64>` on stdout.
fn run_one(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("--one") {
        return false;
    }
    let (Some(mode), Some(name)) = (args.get(1), args.get(2)) else {
        eprintln!("--one needs <mode> <kernel>");
        std::process::exit(2);
    };
    let Some(mode) = mode_by_name(mode) else {
        eprintln!("unknown mode {mode:?}");
        std::process::exit(2);
    };
    let kernel = KERNELS.iter().find(|k| k.name == name).unwrap_or_else(|| {
        eprintln!("unknown kernel {name:?}");
        std::process::exit(2);
    });
    let run = run_kernel(kernel, mode, &parse_thresholds(args));
    println!(
        "ms={} result={:016x} compiled={} promoted={} specialized={} compile_ns={} compile_attempts={} \
         retired={} replaced={} never_entered={} {}",
        run.median.as_secs_f64() * 1e3,
        run.result,
        run.compiled,
        run.promotions,
        run.specialized,
        run.compile_nanos,
        run.compile_attempts,
        run.retired,
        run.replaced,
        run.never_entered,
        site_count_fields(&run.sites)
    );
    true
}

/// Deterministic xorshift64: a bootstrap must be reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

/// 95% bootstrap interval for `median(base) / median(candidate)`, resampling the
/// two per-round samples *together* (they are paired by round: the interleaving
/// exists precisely so a round's machine state applies to both).
fn bootstrap_speedup(base: &[f64], candidate: &[f64], iterations: usize) -> (f64, f64) {
    let n = base.len().min(candidate.len());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut speedups = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let (mut b, mut c) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for _ in 0..n {
            let i = rng.pick(n);
            b.push(base[i]);
            c.push(candidate[i]);
        }
        speedups.push(median(&b) / median(&c).max(f64::MIN_POSITIVE));
    }
    speedups.sort_by(f64::total_cmp);
    // A 95% interval: trim 2.5% off each tail (iterations is large).
    (
        speedups[iterations / 40],
        speedups[iterations - 1 - iterations / 40],
    )
}

/// `--protocol process`: spawn one child per (mode, round), interleaved.
fn run_process_protocol(show: &[Mode], rounds: usize, th: &Thresholds, kernels: &[&Kernel]) {
    let exe = std::env::current_exe().expect("current_exe");
    println!(
        "jit-bench matrix (process protocol): {rounds} fresh processes per mode, interleaved; \
         speedup vs interpreter with a {:.0}% bootstrap interval",
        95.0
    );
    let mut run = show.to_vec();
    if !run.contains(&Mode::Interpreter) {
        run.insert(0, Mode::Interpreter);
    }
    let mut compiled: Vec<(Mode, u64)> = run.iter().map(|&m| (m, 0)).collect();
    // The compile cost of every child, summed per mode (P0-1).
    let mut compile: Vec<(Mode, u64, u64)> = run.iter().map(|&m| (m, 0, 0)).collect();
    // Code retired / replaced / never entered, summed per mode.
    let mut code_usage: Vec<(Mode, [u64; 3])> = run.iter().map(|&m| (m, [0; 3])).collect();
    // The inline guards' and call paths' counters, summed per mode (T18).
    let mut sites: Vec<(Mode, SiteCounts)> =
        run.iter().map(|&m| (m, SiteCounts::default())).collect();
    // The child re-parses the thresholds, so a sweep runs the same
    // configuration in every fresh process.
    let th_args: Vec<String> = vec![
        "--call-threshold".into(),
        th.call_threshold.to_string(),
        "--loop-threshold".into(),
        th.loop_threshold.to_string(),
        "--tier2-call-threshold".into(),
        th.tier2_call_threshold.to_string(),
        "--tier2-loop-threshold".into(),
        th.tier2_loop_threshold.to_string(),
    ];

    for kernel in kernels {
        let mut samples: Vec<(Mode, Vec<f64>)> = run.iter().map(|&m| (m, Vec::new())).collect();
        let mut result: Option<u64> = None;
        for _ in 0..rounds {
            for mode in &run {
                let out = std::process::Command::new(&exe)
                    .args(["--one", mode.label(), kernel.name])
                    .args(&th_args)
                    .output()
                    .expect("spawn bench child");
                let text = String::from_utf8_lossy(&out.stdout);
                let fields: std::collections::HashMap<&str, &str> = text
                    .split_whitespace()
                    .filter_map(|kv| kv.split_once('='))
                    .collect();
                assert!(
                    out.status.success() && fields.contains_key("ms"),
                    "{}: child {} failed: {text}{}",
                    kernel.name,
                    mode.label(),
                    String::from_utf8_lossy(&out.stderr)
                );
                let ms: f64 = fields["ms"].parse().expect("ms");
                let value = u64::from_str_radix(fields["result"], 16).expect("result");
                match result {
                    Some(expected) => assert_eq!(
                        value,
                        expected,
                        "{}: {} disagrees with the interpreter",
                        kernel.name,
                        mode.label()
                    ),
                    None => result = Some(value),
                }
                let slot = compiled.iter_mut().find(|(m, _)| m == mode).unwrap();
                slot.1 += fields["compiled"].parse::<u64>().expect("compiled");
                let cslot = compile.iter_mut().find(|(m, _, _)| m == mode).unwrap();
                cslot.1 += fields["compile_ns"].parse::<u64>().expect("compile_ns");
                cslot.2 += fields["compile_attempts"]
                    .parse::<u64>()
                    .expect("compile_attempts");
                let usage = code_usage.iter_mut().find(|(m, _)| m == mode).unwrap();
                usage.1[0] += fields["retired"].parse::<u64>().expect("retired");
                usage.1[1] += fields["replaced"].parse::<u64>().expect("replaced");
                usage.1[2] += fields["never_entered"]
                    .parse::<u64>()
                    .expect("never_entered");
                let mut values = [0u64; 9];
                for (slot, key) in values.iter_mut().zip(SiteCounts::KEYS) {
                    *slot = fields[key].parse::<u64>().expect(key);
                }
                sites
                    .iter_mut()
                    .find(|(m, _)| m == mode)
                    .unwrap()
                    .1
                    .accumulate(&SiteCounts::from_values(values));
                samples
                    .iter_mut()
                    .find(|(m, _)| m == mode)
                    .unwrap()
                    .1
                    .push(ms);
            }
        }

        let base = samples
            .iter()
            .find(|(m, _)| *m == Mode::Interpreter)
            .expect("interpreter baseline")
            .1
            .clone();
        let base_median = median(&base);
        let row: String = samples
            .iter()
            .map(|(mode, values)| {
                if !show.contains(mode) {
                    return String::new();
                }
                let mid = median(values);
                let (lo, hi) = bootstrap_speedup(&base, values, BOOTSTRAP);
                format!(
                    "{:>9.3}ms {:>5.2}x [{:.2},{:.2}]",
                    mid,
                    base_median / mid.max(f64::MIN_POSITIVE),
                    lo,
                    hi
                )
            })
            .collect();
        println!("{:<24}{}", kernel.name, row);
    }

    println!("results: identical across modes (asserted bit-exact per kernel)");
    let summary: String = compiled
        .iter()
        .map(|(m, c)| format!("{}={c}", m.label()))
        .collect::<Vec<_>>()
        .join(" ");
    println!("functions_compiled: {summary}");
    println!("{}", compile_summary_line(&compile));
    println!("{}", code_usage_line(&code_usage));
    for line in site_count_lines(&sites) {
        println!("{line}");
    }
}

/// The code that was compiled but not used: retired for quota, replaced by a
/// recompile (a tier-2 promotion or a tier-down), and never entered at all.
fn code_usage_line(entries: &[(Mode, [u64; 3])]) -> String {
    let summary: String = entries
        .iter()
        .map(|(m, [retired, replaced, unused])| {
            format!("{}={retired}/{replaced}/{unused}", m.label())
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("code_usage (retired/replaced/never_entered): {summary}")
}

/// The compile cost per mode: total time, number of attempts, mean per
/// attempt (P0-1). The unit is milliseconds.
fn compile_summary_line(entries: &[(Mode, u64, u64)]) -> String {
    let summary: String = entries
        .iter()
        .map(|(m, nanos, attempts)| {
            let avg = if *attempts == 0 {
                0.0
            } else {
                *nanos as f64 / *attempts as f64
            };
            format!(
                "{}={:.1}ms/{}/avg {:.2}ms",
                m.label(),
                *nanos as f64 / 1e6,
                attempts,
                avg / 1e6
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("compile_time (total/attempts/avg): {summary}")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Cross-engine comparison: dump the kernels as TSV (name/define/work) so
    // the same workload can run under another engine's harness.
    if args.first().map(String::as_str) == Some("--dump-kernels") {
        for k in KERNELS {
            println!("{}\t{}\t{}", k.name, k.define, k.work);
        }
        return;
    }
    if run_one(&args) {
        return;
    }
    let thresholds = parse_thresholds(&args);
    let kernels = parse_kernels(&args);
    let rounds = args
        .iter()
        .position(|a| a == "--rounds")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(PROCESS_ROUNDS);
    if args.iter().any(|a| a == "--protocol") {
        let protocol = args
            .iter()
            .position(|a| a == "--protocol")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        if protocol == Some("process") {
            run_process_protocol(&parse_modes(&args), rounds, &thresholds, &kernels);
            return;
        } else if protocol != Some("inproc") && protocol.is_some() {
            eprintln!(
                "unknown --protocol {:?}: use inproc or process",
                protocol.unwrap()
            );
            std::process::exit(2);
        }
    }
    let show = parse_modes(&args);
    if show.is_empty() {
        println!("no runnable mode selected; nothing to benchmark");
        return;
    }
    // The interpreter column is the speedup baseline, so always run it even
    // when it was not selected.
    let mut run = show.clone();
    if !run.contains(&Mode::Interpreter) {
        run.insert(0, Mode::Interpreter);
    }

    println!(
        "jit-bench matrix: median of {ROUNDS} timed rounds, {WARMUP} warm-up rounds discarded"
    );
    let header: String = run
        .iter()
        .map(|m| {
            if show.contains(m) {
                format!("{:>19}", m.label())
            } else {
                String::new()
            }
        })
        .collect();
    println!("{:<24}{}", "kernel", header);

    let mut compiled: Vec<(Mode, u64)> = run.iter().map(|&m| (m, 0)).collect();
    let mut promotions: Vec<(Mode, u64)> = run.iter().map(|&m| (m, 0)).collect();
    let mut specialized: Vec<(Mode, u64)> = run.iter().map(|&m| (m, 0)).collect();
    // The compile cost per mode (P0-1): nanoseconds and attempts.
    let mut compile: Vec<(Mode, u64, u64)> = run.iter().map(|&m| (m, 0, 0)).collect();
    // Code retired / replaced / never entered, per mode.
    let mut code_usage: Vec<(Mode, [u64; 3])> = run.iter().map(|&m| (m, [0; 3])).collect();
    // The guard/inline counters, per mode (T18).
    let mut sites: Vec<(Mode, SiteCounts)> =
        run.iter().map(|&m| (m, SiteCounts::default())).collect();
    for kernel in &kernels {
        let runs: Vec<(Mode, Run)> = run
            .iter()
            .map(|&m| (m, run_kernel(kernel, m, &thresholds)))
            .collect();
        let base = runs
            .iter()
            .find(|(m, _)| *m == Mode::Interpreter)
            .expect("interpreter baseline")
            .1
            .result;
        // Result consistency across modes is part of the acceptance.
        for (m, r) in &runs {
            assert_eq!(
                r.result,
                base,
                "{}: {} disagrees with interpreter",
                kernel.name,
                m.label()
            );
        }

        let row: String = runs
            .iter()
            .map(|(m, r)| {
                if !show.contains(m) {
                    return String::new();
                }
                let base_s = runs
                    .iter()
                    .find(|(bm, _)| *bm == Mode::Interpreter)
                    .unwrap()
                    .1
                    .median
                    .as_secs_f64();
                let speedup = base_s / r.median.as_secs_f64().max(f64::EPSILON);
                format!("{:>9.3}ms {:>6.2}x", r.median.as_secs_f64() * 1e3, speedup)
            })
            .collect::<Vec<_>>()
            .join("");
        println!("{:<24}{}", kernel.name, row);

        for (m, r) in &runs {
            let slot = compiled.iter_mut().find(|(cm, _)| cm == m).unwrap();
            slot.1 += r.compiled;
            let promo = promotions.iter_mut().find(|(pm, _)| pm == m).unwrap();
            promo.1 += r.promotions;
            let spec = specialized.iter_mut().find(|(sm, _)| sm == m).unwrap();
            spec.1 += r.specialized;
            let cost = compile.iter_mut().find(|(cm, _, _)| cm == m).unwrap();
            cost.1 += r.compile_nanos;
            cost.2 += r.compile_attempts;
            let usage = code_usage.iter_mut().find(|(um, _)| um == m).unwrap();
            usage.1[0] += r.retired;
            usage.1[1] += r.replaced;
            usage.1[2] += r.never_entered;
            let site = sites.iter_mut().find(|(sm, _)| sm == m).unwrap();
            site.1.accumulate(&r.sites);
        }
    }

    println!("results: identical across modes (asserted bit-exact per kernel)");
    let summary: String = compiled
        .iter()
        .map(|(m, c)| format!("{}={c}", m.label()))
        .collect::<Vec<_>>()
        .join(" ");
    println!("functions_compiled: {summary}");
    println!(
        "tier2_promotions: {}",
        promotions
            .iter()
            .map(|(m, p)| format!("{}={p}", m.label()))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "specialized_ops: {}",
        specialized
            .iter()
            .map(|(m, s)| format!("{}={s}", m.label()))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("{}", compile_summary_line(&compile));
    println!("{}", code_usage_line(&code_usage));
    for line in site_count_lines(&sites) {
        println!("{line}");
    }
}
