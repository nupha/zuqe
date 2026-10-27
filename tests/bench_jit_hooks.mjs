// SPDX-License-Identifier: Apache-2.0
// VM-P14 attribution probe: one kernel per process, 2 warm-up + 7 timed
// rounds, median. Run the no-jit and default (jit feature, no JIT attached)
// zjs binaries alternately and compare the per-process medians.
//
//   zjs tests/bench_jit_hooks.mjs scalar-loop
//
// The kernels are the ones from memo/p6_dispatch_design.md appendix A.
import * as os from "os";

function median(a) {
    a = a.slice().sort((x, y) => x - y);
    return a[a.length >> 1];
}

function bench(name, fn) {
    let r;
    for (let i = 0; i < 2; i++) r = fn();
    const samples = [];
    for (let i = 0; i < 7; i++) {
        const t0 = os.now();
        r = fn();
        samples.push(os.now() - t0);
    }
    console.log(`${name} ${median(samples).toFixed(3)} result=${String(r)}`);
}

function scalar_loop(n, seed) {
    var sum = seed;
    for (var i = seed; i < n; i++) sum = sum + i;
    return sum;
}

function bitops(n, seed) {
    var v = seed | 0;
    for (var i = 0; i < n; i++) v = ((v << 5) ^ (v >>> 3) ^ i) | 0;
    return v;
}

var point = { x: 0, y: 1 };
function property(n, seed, p) {
    p.x = seed; p.y = 1;
    var toggle = 0;
    for (var i = 0; i < n; i++) { p.x = p.x + p.y; p.y = p.y + toggle; toggle = 1 - toggle; }
    return p.x + p.y;
}

var values = new Array(2000);
for (var i = 0; i < values.length; i++) values[i] = (i * 17) & 0xffff;
function array_sum(seed, a) {
    var s = seed;
    for (var r = 0; r < 500; r++) { s = seed; for (var j = 0; j < a.length; j++) s = (s + a[j]) | 0; }
    return s;
}

function scalar_control_flow(iterations, seed) {
    var sum = seed;
    for (var i = seed; i < iterations; i++) {
        var x, y;
        if (i & 1) { x = i + seed; y = i - seed; } else { x = i - seed; y = i + seed; }
        sum = (sum + ((x * y) & 1023)) | 0;
    }
    return sum;
}

function calls_closures(iterations) {
    var add = x => y => (x + y) | 0;
    var sum = 0;
    for (var i = 0; i < iterations; i++) sum = add(sum)(i & 31);
    return String(sum);
}

function exceptions_sync(iterations, seed) {
    var table = [{ w: 1 }, { w: 2 }, null, { w: 3 }];
    var caught = 0; var total = seed | 0; var finals = 0; var missing = 0;
    for (var i = 0; i < iterations; i++) {
        try { if ((i & 15) === 0) throw i; total = (total + i) | 0; }
        catch (value) { caught += value; } finally { finals++; }
        if ((i & 63) === 1) {
            try { total = (total + table[(i >> 6) & 3].w) | 0; } catch (error) { missing++; }
        }
    }
    return caught + ":" + total + ":" + finals + ":" + missing;
}

function probe_thin_nocall(iterations, seed) {
    var s = seed;
    for (var i = 0; i < iterations; i++) s = s;
    return s;
}

const KERNELS = {
    "scalar-loop": () => scalar_loop(2000000, 1),
    "scalar-control-flow": () => scalar_control_flow(3000000, 1),
    "bitops": () => bitops(1000000, 12345),
    "probe-thin-nocall": () => probe_thin_nocall(20000000, 1),
    "property": () => property(500000, 1, point),
    "array-sum": () => array_sum(7, values),
    "calls-closures": () => calls_closures(40000),
    "exceptions-sync": () => exceptions_sync(300000, 1),
};

const name = scriptArgs[1];
const fn = KERNELS[name];
if (!fn) {
    console.log("usage: zjs tests/bench_jit_hooks.mjs <" + Object.keys(KERNELS).join("|") + ">");
} else {
    bench(name, fn);
}
