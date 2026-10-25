// SPDX-License-Identifier: Apache-2.0
//
// VM-B3 timing probe (manual benchmark, NOT a pass/fail test): constructor
// allocations followed by object literals in the same long-lived context, the
// exact shape from memo/jit_metrics.md §8.4.
//
// The pass/fail regression for the same bug is `tests/test_gc_ctor_scope.mjs`
// (it asserts a live-memory bound). This file only prints phase timings, so it
// is sensitive to machine load — use it for A/B comparisons, not as a gate.
//
// ── Environment ──────────────────────────────────────────────────────────
// - Host: `examples/zjs`, release build:
//     cargo build --release --example zjs
//     ./target/release/examples/zjs tests/bench_gc_ctor_objlit.js
// - Same process / same context for all four phases (that is the point: the
//   bug is a cross-phase heap-state effect).
// - N = 2M per phase; expect ~10-11 s total after the fix, ~70 s before.
//
// ── Method ───────────────────────────────────────────────────────────────
// Run `ctor 2M` twice, then `objlit 2M` twice, printing each phase's wall
// time (Date.now()). Compare C1 vs C2 (same work; C2 should not be slower) and
// O1 vs O2 (should stay flat instead of growing 4.5x). A fresh-process objlit
// run is the reference baseline (~3 s on the jit_metrics machine).

function C(i) {
    this.a = i;
    this.b = i + 1;
    this.c = i + 2;
}

function runCtor(n) {
    let sink = null;
    for (let i = 0; i < n; i++) {
        sink = new C(i);
    }
    if (sink === null) console.log("unreachable");
}

function runObjlit(n) {
    let sink = null;
    for (let i = 0; i < n; i++) {
        sink = { a: i, b: i + 1, c: i + 2 };
    }
    if (sink === null) console.log("unreachable");
}

function phase(name, fn) {
    const t0 = Date.now();
    fn();
    const t1 = Date.now();
    console.log(name + ": " + (t1 - t0) + "ms");
}

const N = 2000000;
phase("C1 ctor", function () { runCtor(N); });
phase("C2 ctor", function () { runCtor(N); });
phase("O1 objlit", function () { runObjlit(N); });
phase("O2 objlit", function () { runObjlit(N); });
