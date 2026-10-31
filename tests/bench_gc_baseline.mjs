// SPDX-License-Identifier: Apache-2.0
//
// Phase 0 GC baseline (manual probe, not a pass/fail test).
//
// One kernel per process so a previous kernel's heap state cannot bias the
// next one. Prints one JSON line: wall time, GC time/share, heap peak and the
// major/minor counters from `std.gcStats()`.
//
//   ./target/release/examples/zjs tests/bench_gc_baseline.mjs <kernel> <N>
//
// kernels: churn-ctor | churn-objlit | retain-objects | mixed
//
// The driver `tests/bench_gc_baseline.sh` runs each kernel 6 times and takes
// the median (see memo/gc-generations.md §7 for the recorded numbers).

import * as std from "std";

const KERNEL = scriptArgs[1] || "churn-ctor";
const N = +(scriptArgs[2] || 200000);
const PROMOTE_AFTER = scriptArgs[3] === undefined ? -1 : +scriptArgs[3];

// X-generation sweep: 0 disables minors, 1/2 select the promotion count;
// -1 keeps the engine default.
if (PROMOTE_AFTER >= 0) std.gcPromote(PROMOTE_AFTER);

let peak = 0;

function sample_mem() {
    const m = std.gcMemory();
    if (m > peak) peak = m;
}

// Request a collection and execute one small loop: the interpreter checks
// `gc_req` at every opcode, so the request is honored without waiting for tick.
function force_gc() {
    std.gc();
    for (let i = 0; i < 16; i++) {
        // no allocation: keep the loop from being folded away
        if (i === -1) print("unreachable");
    }
}

function C(a, b, c) {
    this.a = a;
    this.b = b;
    this.c = c;
}

function churn_ctor(n) {
    let sink = null;
    const step = Math.max(1, n >> 6);
    for (let i = 0; i < n; i++) {
        sink = new C(i, i + 1, i + 2);
        if ((i % step) === 0) sample_mem();
    }
    return sink === null ? 0 : 1;
}

function churn_objlit(n) {
    let sink = null;
    const step = Math.max(1, n >> 6);
    for (let i = 0; i < n; i++) {
        sink = { a: i, b: i + 1, c: i + 2 };
        if ((i % step) === 0) sample_mem();
    }
    return sink === null ? 0 : 1;
}

// Retained set with four shapes per round (the `objects-polymorphic` shape).
function retain_objects(n) {
    const keep = new Array(n);
    const step = Math.max(1, n >> 6);
    for (let i = 0; i < n; i++) {
        switch (i & 3) {
        case 0: keep[i] = { a: i }; break;
        case 1: keep[i] = { a: i, b: i + 1 }; break;
        case 2: keep[i] = { a: i, b: i + 1, c: i + 2 }; break;
        default: keep[i] = { a: i, b: i + 1, c: i + 2, d: i + 3 }; break;
        }
        if ((i % step) === 0) sample_mem();
    }
    return keep.length;
}

// Half the objects stay live; the other half is short-lived churn.
function mixed(n) {
    const live = new Array(n >> 1);
    for (let i = 0; i < live.length; i++) live[i] = { v: i };
    let sink = null;
    const step = Math.max(1, n >> 6);
    for (let i = 0; i < n; i++) {
        sink = { a: i, b: i + 1, c: i + 2 };
        live[i % live.length].v = i;
        if ((i % step) === 0) sample_mem();
    }
    return live.length + (sink === null ? 0 : 1);
}

const KERNELS = {
    "churn-ctor": churn_ctor,
    "churn-objlit": churn_objlit,
    "retain-objects": retain_objects,
    "mixed": mixed,
};

const fn = KERNELS[KERNEL];
if (!fn) throw new Error("unknown kernel: " + KERNEL);

// Warm up: JIT/IC/shape machinery, plus a collection so the timed window
// starts from a comparable heap state.
fn(Math.max(1000, N >> 4));
force_gc();
peak = 0;

const s0 = std.gcStats();
const base = std.gcMemory();
const t0 = Date.now();
const result = fn(N);
const wall_ms = Date.now() - t0;
const s1 = std.gcStats();
const end = std.gcMemory();

const gc_ns = s1.timeNs - s0.timeNs;
const out = {
    kernel: KERNEL,
    n: N,
    promoteAfter: PROMOTE_AFTER,
    result: result,
    wallMs: wall_ms,
    gcMs: +(gc_ns / 1e6).toFixed(2),
    gcShare: wall_ms > 0 ? +((gc_ns / 1e6) / wall_ms).toFixed(4) : 0,
    baseBytes: base,
    endBytes: end,
    peakBytes: peak,
    majors: s1.majorCount - s0.majorCount,
    minors: s1.minorCount - s0.minorCount,
    majorFreed: s1.majorFreed - s0.majorFreed,
    minorFreed: s1.minorFreed - s0.minorFreed,
};
print(JSON.stringify(out));
