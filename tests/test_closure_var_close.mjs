// SPDX-License-Identifier: Apache-2.0
//
// VM-P9 regression: closing a captured block binding (CLOSE_LOC) must drop the
// frame's `var_ref` entry, not just freeze it.
//
// ── Environment ──────────────────────────────────────────────────────────
// - Host: `examples/zjs` (needs `std.gc()` / `std.gcMemory()` for the
//   retention half; those are host extensions, not part of the JS API).
// - Run: `./target/release/examples/zjs tests/test_closure_var_close.mjs`
// - Exit code: 0 = pass; 1 = failed assertion (the Error is not caught).
//
// ── Why this test ────────────────────────────────────────────────────────
// A per-iteration binding (`const x = i` in a loop body) is captured, then
// closed at the end of the iteration. The closed ref is `Detached` and can
// never match again, so keeping it in the frame list made the list grow with
// the iteration count: every later capture re-scanned every dead entry
// (quadratic) and every dead ref stayed GC-reachable until the frame returned.
// 50k iterations took 4.5 s (QuickJS 12 ms); 100k is now ~80 ms.
//
// Expected values are hand-computed; nothing here compares the engine against
// itself.
import * as std from "std";

function assert(cond, msg) {
  if (!cond) throw new Error("assertion failed: " + msg);
}

function collect() {
  std.gc();
  for (let i = 0; i < 64; i++) {}
}

function restingMemory() {
  collect();
  return std.gcMemory();
}

// ── 1. per-iteration `const` binding keeps its own value ─────────────────
function perIterationConst(n) {
  const fns = [];
  for (let i = 0; i < n; i++) {
    const x = i * 3;
    fns.push(() => x);
  }
  return fns.map((f) => f()).join(",");
}
assert(perIterationConst(5) === "0,3,6,9,12", "per-iteration const values");

// ── 2. the loop variable itself, captured per iteration ──────────────────
function perIterationLoopVar(n) {
  const fns = [];
  for (let i = 0; i < n; i++) fns.push(() => i);
  return fns.map((f) => f()).join(",");
}
assert(perIterationLoopVar(4) === "0,1,2,3", "per-iteration loop variable");

// ── 3. two closures sharing one live binding see the same box ───────────
function sharedBinding(n) {
  const before = [];
  const after = [];
  for (let i = 0; i < n; i++) {
    let x = i;
    before.push(() => x);
    x = x * 10;
    after.push(() => x);
  }
  return [before.map((f) => f()).join(","), after.map((f) => f()).join(",")];
}
const [before, after] = sharedBinding(3);
assert(before === "0,10,20" && after === "0,10,20", "shared binding box");

// ── 4. a closed binding stays isolated from a later block's binding ─────
function siblingBlocks() {
  let read1;
  let write1;
  let read2;
  {
    let x = 1;
    read1 = () => x;
    write1 = (v) => {
      x = v;
    };
  }
  {
    let x = 2;
    read2 = () => x;
    write1(10); // writes block 1's closed box, not block 2's binding
    assert(read1() === 10, "write through a closed ref was lost");
    assert(read2() === 2, "closed ref leaked into a sibling binding");
  }
  return [read1(), read2()];
}
const [closedValue, siblingValue] = siblingBlocks();
assert(closedValue === 10 && siblingValue === 2, "sibling block isolation");

// ── 5. `for...of` per-iteration bindings ────────────────────────────────
function forOfClosures() {
  const fns = [];
  for (const v of [1, 2, 3]) fns.push(() => v);
  return fns.map((f) => f()).join(",");
}
assert(forOfClosures() === "1,2,3", "for-of per-iteration binding");

// ── 6. destructuring targets (make_loc_ref puts a VarRef in the list) ───
function destructured(n) {
  const fns = [];
  for (let i = 0; i < n; i++) {
    let a;
    [a] = [i + 100];
    fns.push(() => a);
  }
  return fns.map((f) => f()).join(",");
}
assert(destructured(3) === "100,101,102", "destructured captured target");

// ── 7. catch parameter captured, then closed ────────────────────────────
let caught;
try {
  throw 7;
} catch (e) {
  caught = () => e;
}
assert(caught() === 7, "captured catch binding");

// ── 8. captured argument outliving the frame (frame-free detach path) ───
function makeAdder(x) {
  return (y) => x + y;
}
const add5 = makeAdder(5);
assert(add5(1) === 6 && add5(10) === 15, "captured argument outlives frame");

// ── 9. nested capture inside a per-iteration binding ────────────────────
function nested(n) {
  const fns = [];
  for (let i = 0; i < n; i++) {
    const x = i;
    fns.push(() => () => x * 2);
  }
  return fns.map((f) => f()()).join(",");
}
assert(nested(3) === "0,2,4", "nested capture in per-iteration binding");

// ── 10. cost + retention stay bounded inside one long-lived frame ───────
// The frame is alive while the loop runs, so a kept closed entry shows up as
// retained memory here (old: ~48 B/iteration) and as quadratic time.
function captureLoop(n) {
  const base = restingMemory();
  const t0 = Date.now();
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const x = i;
    const f = () => x;
    sum = sum + f();
  }
  const ms = Date.now() - t0;
  const grown = restingMemory() - base;
  return { sum, ms, grown };
}

const N = 100000;
const r = captureLoop(N);
assert(r.sum === ((N - 1) * N) / 2, "capture loop result");
assert(r.grown < 1024 * 1024, "closed var_refs retained: " + r.grown + " B");
assert(r.ms < 2000, "capture loop must stay O(n), took " + r.ms + " ms");

// Closed refs still held by live closures stay valid: the same loop, but the
// closures escape into an array and are read after the loop's block ends.
function escapeLoop(n) {
  const fns = [];
  for (let i = 0; i < n; i++) {
    const x = i;
    fns.push(() => x);
  }
  collect();
  return fns.map((f) => f()).join(",");
}
const escaped = escapeLoop(1000);
assert(
  escaped === Array.from({ length: 1000 }, (_, i) => i).join(","),
  "escaped closures corrupted across GC",
);

console.log("test_closure_var_close: all pass");
