// SPDX-License-Identifier: Apache-2.0
//
// Phase 0 observability regression: `std.gcStats()` must track explicit
// (`std.gc()`) and automatic major collections, and `std.gcMemory()` keeps
// returning the live byte count.
//
// Host: examples/zjs (release or debug). Run:
//   ./target/release/examples/zjs tests/test_gc_stats.mjs

import * as std from "std";

function check(cond, msg) {
    if (!cond) throw new Error(msg);
}

// The interpreter checks `gc_req` at every opcode, so a short loop right after
// `std.gc()` performs the collection without waiting for tick.
function spin() {
    for (let i = 0; i < 16; i++) {
        if (i === -1) throw new Error("unreachable");
    }
}

const start = std.gcStats();
check(start.minorCount === 0, "no minor collections before Phase 2");
check(start.majorCount >= 0, "major counter is non-negative");
check(start.timeNs >= 0, "time counter is non-negative");

std.gc();
spin();
const explicit = std.gcStats();
check(
    explicit.majorCount === start.majorCount + 1,
    "std.gc() increments the major counter",
);

// Automatic path: allocate transient objects until the next threshold fires.
const autoStart = explicit.majorCount;
let n = 0;
while (std.gcStats().majorCount === autoStart) {
    for (let i = 0; i < 10000; i++) {
        ({ a: i, b: i + 1, c: i + 2 });
    }
    n += 10000;
    check(n <= 5000000, "automatic major collection did not happen");
}
const autoEnd = std.gcStats();
check(autoEnd.majorCount > autoStart, "automatic major increments the counter");
check(autoEnd.timeNs > explicit.timeNs, "gc time accumulates");
check(autoEnd.majorFreed >= explicit.majorFreed, "freed bytes accumulate");

const mem = std.gcMemory();
check(typeof mem === "number" && mem >= 0, "std.gcMemory() still returns bytes");

print("test_gc_stats: passed");
