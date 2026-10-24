// SPDX-License-Identifier: Apache-2.0
//
// GC regression: constructor instances must not accumulate in the caller's GC
// scope while the constructor body runs (VM-B3, memo/vm_todo.md §0).
//
// ── Environment ──────────────────────────────────────────────────────────
// - Host: `examples/zjs`. The test needs `std.gcMemory()`, a zjs-only host
//   extension backed by `RuntimeImpl::gc_memory_used()`; it is NOT part of the
//   engine's JS API, so this script does not run on other hosts.
// - Build: release (`cargo build --release --example zjs`). Debug builds are
//   valid but ~10x slower; the bound is unaffected.
// - Run: `./target/release/examples/zjs tests/test_gc_ctor_scope.mjs`
// - Exit code: 0 = pass; 1 = failed assertion (the Error is not caught).
//
// ── What is measured ─────────────────────────────────────────────────────
// `std.gcMemory()` returns the bytes currently accounted live by the GC heap.
// Sampling it *inside* the allocation loop is the point: once the loop's
// function returns, its scope is popped and the leak drains, so a post-hoc
// measurement would miss the bug.
//
// ── Why a baseline-relative bound ────────────────────────────────────────
// `peak - baseline` isolates the loop's own retention from the runtime's
// builtin baseline (which varies with enabled features). Before the fix the
// growth scaled with the iteration count (~62 MB for 400k instances); after
// the fix it stays around the automatic-GC threshold (total live set ~155 KB,
// i.e. tens of KB above the baseline). The 4 MiB bound is two orders of
// magnitude above the fixed value and one order below the buggy one.
//
// ── Method ───────────────────────────────────────────────────────────────
// 1. Force a collection and read the resting live set as `baseline`.
// 2. Run `ctor 2x -> objlit 2x` (the jit_metrics §8.4 order-probe shape),
//    sampling the live set every 16k iterations.
// 3. Assert the last instance/literal is intact.
// 4. Assert `peak - baseline <= 4 MiB`.
import * as std from "std";

const N = 200000;
const MEM_LIMIT = 4 * 1024 * 1024;

function assert(cond, msg) {
    if (!cond) throw new Error("assertion failed: " + msg);
}

function C(i) {
    this.a = i;
    this.b = i + 1;
    this.c = i + 2;
}

let peak = 0;

function sample() {
    const m = std.gcMemory();
    if (m > peak) peak = m;
}

// `std.gc()` only *requests* a collection; the safepoint after the next opcode
// actually runs it. Spin a few no-op iterations so the reading is post-sweep.
function restingMemory() {
    std.gc();
    for (let i = 0; i < 64; i++) {}
    return std.gcMemory();
}

function runCtor(n) {
    let sink = null;
    for (let i = 0; i < n; i++) {
        sink = new C(i);
        if ((i & 0x3fff) === 0) sample();
    }
    assert(sink !== null, "ctor loop did not run");
    assert(
        sink.a === n - 1 && sink.b === n && sink.c === n + 1,
        "constructor instance corrupted: " + sink.a + "," + sink.b + "," + sink.c,
    );
}

function runObjlit(n) {
    let sink = null;
    for (let i = 0; i < n; i++) {
        sink = { a: i, b: i + 1, c: i + 2 };
        if ((i & 0x3fff) === 0) sample();
    }
    assert(sink !== null, "objlit loop did not run");
    assert(
        sink.a === n - 1 && sink.b === n && sink.c === n + 1,
        "object literal corrupted: " + sink.a + "," + sink.b + "," + sink.c,
    );
}

const baseline = restingMemory();
peak = baseline;

// The exact jit_metrics §8.4 order-probe shape: ctor x2, then objlit x2.
runCtor(N);
runCtor(N);
runObjlit(N);
runObjlit(N);

const growth = peak - baseline;
assert(
    growth <= MEM_LIMIT,
    "live GC memory grew by " +
        growth +
        " bytes (baseline " +
        baseline +
        ", peak " +
        peak +
        ", limit " +
        MEM_LIMIT +
        "): caller-scope LOCAL nodes are leaking",
);

console.log(
    "test_gc_ctor_scope: passed (baseline " +
        baseline +
        " B, peak " +
        peak +
        " B, growth " +
        growth +
        " B)",
);
