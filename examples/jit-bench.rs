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
    Context, EvalFlags, EvalName, EvalType, JSValue, Jit, JitConfig, JitTierPolicy, Runtime,
    ScopedContext,
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

    /// Eager thresholds so the single warm-up call in `define` queues the
    /// function and `poll` installs it before any timing starts.
    fn config(self) -> JitConfig {
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(self.policy())
            .build()
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

struct Run {
    result: u64,
    median: Duration,
    compiled: u64,
}

fn run_kernel(kernel: &Kernel, mode: Mode) -> Run {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, mode.config()).unwrap();
    eval(&mut ctx, kernel.define.as_bytes());
    jit.poll();
    let compiled = jit.metrics().functions_compiled;
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

    // Verify run first, then warm-up (discarded), then timed rounds.
    let result = digest(&eval(&mut ctx, kernel.work.as_bytes()));
    for _ in 0..WARMUP {
        std::hint::black_box(eval(&mut ctx, kernel.work.as_bytes()));
    }
    let mut samples: Vec<Duration> = (0..ROUNDS)
        .map(|_| {
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
            elapsed
        })
        .collect();
    samples.sort_unstable();
    Run {
        result,
        median: samples[ROUNDS / 2],
        compiled,
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
    let run = run_kernel(kernel, mode);
    println!(
        "ms={} result={:016x} compiled={}",
        run.median.as_secs_f64() * 1e3,
        run.result,
        run.compiled
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
fn run_process_protocol(show: &[Mode], rounds: usize) {
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

    for kernel in KERNELS {
        let mut samples: Vec<(Mode, Vec<f64>)> = run.iter().map(|&m| (m, Vec::new())).collect();
        let mut result: Option<u64> = None;
        for _ in 0..rounds {
            for mode in &run {
                let out = std::process::Command::new(&exe)
                    .args(["--one", mode.label(), kernel.name])
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
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run_one(&args) {
        return;
    }
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
            run_process_protocol(&parse_modes(&args), rounds);
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
    for kernel in KERNELS {
        let runs: Vec<(Mode, Run)> = run.iter().map(|&m| (m, run_kernel(kernel, m))).collect();
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
        }
    }

    println!("results: identical across modes (asserted bit-exact per kernel)");
    let summary: String = compiled
        .iter()
        .map(|(m, c)| format!("{}={c}", m.label()))
        .collect::<Vec<_>>()
        .join(" ");
    println!("functions_compiled: {summary}");
}
