// SPDX-License-Identifier: Apache-2.0
//
// GC regression for VM-P7b: a same-realm `Light` callee (allocation only on a
// conditional opcode, e.g. string concatenation) is inlined into the caller's
// dispatch loop and runs in the caller's GC scope when that scope is cleared
// at safepoints. Its garbage must not accumulate across calls: every safepoint
// calls `clear_locals_from_depth`, so `peak - baseline` stays bounded.
//
// Requires the zjs host (`std.gc`, `std.gcMemory`).
// Run: ./target/release/examples/zjs tests/test_gc_light_call_scope.mjs
import * as std from "std";

var ITER = 100000;
var MEM_LIMIT = 4 * 1024 * 1024;

std.gc();
var baseline = std.gcMemory();
var peak = baseline;

// `+` on a string is a Conditional allocation: the function is `Light`.
function makeStr(i) {
    return "value-" + i;
}

function run() {
    var last = "";
    for (var i = 0; i < ITER; i++) {
        last = makeStr(i);
        if ((i & 1023) === 0) {
            var mem = std.gcMemory();
            if (mem > peak) peak = mem;
        }
    }
    return last;
}

var result = run();
var growth = peak - baseline;
if (result !== "value-" + (ITER - 1)) {
    throw new Error("wrong result: " + result);
}
if (growth > MEM_LIMIT) {
    throw new Error("Light callee garbage accumulated: " + growth + " bytes");
}
console.log("OK growth=" + growth);
