// SPDX-License-Identifier: Apache-2.0
//
// Promotion-count (X) probe: keep a large survivor set alive and force many
// minors. With X=1 the survivors promote on the first minor; with X=N they stay
// young and are re-scanned for N minors, so this isolates the cost of delayed
// promotion (the adaptive threshold otherwise makes minors too rare to see it).
//
//   ./target/release/examples/zjs tests/bench_promotion_x.mjs <X> [M] [ROUNDS] [CHURN]
//   X=1 | 2 | 4 | 8 | 16 ...

import * as std from "std";

const X = +(scriptArgs[1] || 2);
const M = +(scriptArgs[2] || 50000); // survivors kept for the whole run
const ROUNDS = +(scriptArgs[3] || 200); // forced minors
const CHURN = +(scriptArgs[4] || 200); // young garbage per round

if (std.gcPromote) std.gcPromote(X);

function spin() {
    for (let i = 0; i < 8; i++) {
        if (i === -1) throw new Error("unreachable");
    }
}

// Reachable survivors (a root array keeps them alive through every minor).
const keep = new Array(M);
for (let i = 0; i < M; i++) keep[i] = { v: i, a: i + 1, b: { c: i } };
spin();

const s0 = std.gcStats();
const t0 = Date.now();
for (let round = 0; round < ROUNDS; round++) {
    for (let i = 0; i < CHURN; i++) {
        // Short-lived garbage; `keep` is the only survivor set.
        const o = { a: i, b: { c: i } };
        if (i === -1) keep.push(o);
    }
    std.gcMinor();
    spin();
}
const wall = Date.now() - t0;
const s1 = std.gcStats();
const gcMs = (s1.timeNs - s0.timeNs) / 1e6;

print(JSON.stringify({
    x: X,
    m: M,
    rounds: ROUNDS,
    churn: CHURN,
    wallMs: wall,
    gcMs: +gcMs.toFixed(2),
    minors: s1.minorCount - s0.minorCount,
    majors: s1.majorCount - s0.majorCount,
    peakBytes: std.gcMemory(),
}));
