// SPDX-License-Identifier: Apache-2.0
//
// GC regression: wide containers and duplicated references (VM-G1,
// memo/vm_todo.md §1).
//
// ── Environment ──────────────────────────────────────────────────────────
// - Host: `examples/zjs` (needs `std.gc()` / `std.gcMemory()`; those are
//   host extensions, not part of the engine's JS API).
// - Build: release is fine; debug also works.
// - Run: `./target/release/examples/zjs tests/test_gc_wide_container.mjs`
// - Exit code: 0 = pass; 1 = failed assertion (the Error is not caught).
//
// ── Why this test ────────────────────────────────────────────────────────
// VM-G1 was a quadratic mark: `GcTraceCtx::add_node` deduplicated by scanning
// its work buffer, so tracing an n-element container cost n²/2 pointer
// comparisons. The fix enqueues children unconditionally (the mark loop
// already skips non-white nodes), which makes *duplicate* references possible
// in the work list. This test pins both sides of that trade-off:
//
//  1. reachable objects behind a wide array survive repeated collections with
//     their identity and contents intact;
//  2. the same child reachable many times (array of one shared object,
//     diamond graph) survives — the duplicate-enqueue path;
//  3. a wide object (many properties) and Map/Set values survive;
//  4. dropping a wide container actually frees it (sweep still works), memory
//     returning to the pre-allocation baseline (bounded).
//
// The *cost* of the quadratic mark is a benchmark, not a gate
// (`tests/bench_gc_retain.js`); timing assertions are too machine-dependent.
import * as std from "std";

const WIDE = 50000;
const DUP = 20000;
const MEM_LIMIT = 4 * 1024 * 1024;

function assert(cond, msg) {
  if (!cond) throw new Error("assertion failed: " + msg);
}

// `std.gc()` only requests a collection; the safepoint after the next opcode
// runs it. Spin a few no-op iterations so the reading is post-sweep.
function collect() {
  std.gc();
  for (let i = 0; i < 64; i++) {}
}

function restingMemory() {
  collect();
  return std.gcMemory();
}

// ── 1. wide array of reachable objects ───────────────────────────────────
let sink = null;

function wideArray() {
  const n = WIDE;
  const a = new Array(n);
  for (let i = 0; i < n; i++) a[i] = { v: i, s: "s" + i };
  const first = a[0];
  const last = a[n - 1];

  collect();
  collect();

  assert(a.length === n, "wide array length changed");
  assert(a[0] === first, "first element identity lost across GC");
  assert(a[n - 1] === last, "last element identity lost across GC");
  for (let i = 0; i < n; i += 997) {
    assert(a[i].v === i, "element " + i + " value corrupted: " + a[i].v);
    assert(a[i].s === "s" + i, "element " + i + " string corrupted");
  }
  assert(first.v === 0 && last.v === n - 1, "cached elements corrupted");
  return a;
}

// ── 2. duplicated references (the de-duplication the mark loop now owns) ──
function duplicatedReferences() {
  const shared = { tag: "shared" };
  const a = new Array(DUP);
  for (let i = 0; i < DUP; i++) a[i] = shared;

  // diamond: every node references both children
  let g = { v: 0 };
  for (let i = 1; i <= 64; i++) {
    const left = { v: i, a: g, b: g };
    g = left;
  }

  collect();
  collect();

  for (let i = 0; i < DUP; i += 1021) {
    assert(a[i] === shared, "duplicated element identity lost at " + i);
  }
  assert(shared.tag === "shared", "shared child corrupted");
  let depth = 0;
  for (let p = g; p; p = p.a) {
    assert(p.a === p.b, "diamond child pair diverged");
    assert(p.v === 64 - depth, "diamond node value corrupted");
    depth++;
  }
  assert(depth === 65, "diamond depth changed: " + depth);
  return [a, g];
}

// ── 3. wide object (many properties) and Map/Set values ──────────────────
function wideObjectAndCollections() {
  const o = {};
  for (let i = 0; i < 2000; i++) o["k" + i] = { v: i };

  const map = new Map();
  const set = new Set();
  for (let i = 0; i < 5000; i++) {
    const v = { v: i };
    map.set("k" + i, v);
    set.add(v);
  }

  collect();
  collect();

  for (let i = 0; i < 2000; i += 37) {
    assert(o["k" + i].v === i, "wide-object prop k" + i + " corrupted");
  }
  for (let i = 0; i < 5000; i += 53) {
    const v = map.get("k" + i);
    assert(v !== undefined && v.v === i, "map entry k" + i + " lost");
    assert(set.has(v), "set lost map value k" + i);
  }
  assert(map.size === 5000 && set.size === 5000, "map/set size changed");
  return [o, map, set];
}

// ── 4. a dropped wide container is really freed ──────────────────────────
function droppedContainer() {
  const baseline = restingMemory();

  (function () {
    const a = new Array(WIDE);
    for (let i = 0; i < WIDE; i++) a[i] = { v: i, s: "d" + i };
    assert(a.length === WIDE, "transient array length");
  })();

  const after = restingMemory();
  const growth = after - baseline;
  console.log("dropped wide container: growth " + growth + " bytes");
  assert(
    growth <= MEM_LIMIT,
    "a dropped 50k-element container was not freed: " +
      growth +
      " bytes still live (limit " +
      MEM_LIMIT +
      ")",
  );
}

// ── run ──────────────────────────────────────────────────────────────────
const t0 = Date.now();
sink = wideArray();
const duplicated = duplicatedReferences();
const collections = wideObjectAndCollections();
droppedContainer();

// A final collection with everything above still reachable: it must survive.
collect();
assert(
  sink.length === WIDE && sink[WIDE - 1].v === WIDE - 1,
  "wide array lost after the final collection",
);
assert(duplicated[0][0].tag === "shared", "duplicated child lost");
assert(collections[1].size === 5000, "map lost after the final collection");
assert(collections[2].size === 5000, "set lost after the final collection");
console.log("wide-container GC: ok in " + (Date.now() - t0) + "ms");
