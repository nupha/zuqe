// SPDX-License-Identifier: Apache-2.0
// VM-P9 attribution probe: closure creation with 0/1/2 captured locals, so the
// per-capture `get_var_ref` cost shows up as the delta between the kernels.
// One kernel per process, 2 warm-up + 7 timed rounds, median.
//
//   zjs tests/bench_closure_var.mjs capt0
//   zjs tests/bench_closure_var.mjs capt1
//   zjs tests/bench_closure_var.mjs capt2
//   zjs tests/bench_closure_var.mjs read1

import * as os from "os";

function median(a) {
    a = a.slice().sort((x, y) => x - y);
    return a[a.length >> 1];
}

function bench(name, fn, perIter) {
    let r;
    for (let i = 0; i < 2; i++) r = fn();
    const samples = [];
    for (let i = 0; i < 7; i++) {
        const t0 = os.now();
        r = fn();
        samples.push(os.now() - t0);
    }
    const ns = (median(samples) * 1e6) / perIter;
    console.log(`${name} ${ns.toFixed(2)} ns/iter result=${String(r)}`);
}

function capt0(n) {
    let sum = 0;
    for (let i = 0; i < n; i++) {
        const f = () => 1;
        sum = (sum + f()) | 0;
    }
    return sum;
}

function capt1(n) {
    let sum = 0;
    for (let i = 0; i < n; i++) {
        const x = i;
        const f = () => x;
        sum = (sum + f()) | 0;
    }
    return sum;
}

function capt2(n) {
    let sum = 0;
    for (let i = 0; i < n; i++) {
        const x = i;
        const y = i + 1;
        const f = () => x + y;
        sum = (sum + f()) | 0;
    }
    return sum;
}

// One closure, many calls: exercises captured read/write but not `get_var_ref`
// on the call path; guards against regressions in the captured-var access.
function read1(n) {
    let x = 0;
    const f = () => (x = (x + 1) | 0);
    let sum = 0;
    for (let i = 0; i < n; i++) sum = (sum + f()) | 0;
    return sum;
}

const N = Number(scriptArgs[2] ?? 400000);

const KERNELS = {
    capt0: [() => capt0(N), N],
    capt1: [() => capt1(N), N],
    capt2: [() => capt2(N), N],
    read1: [() => read1(N), N],
};

const name = scriptArgs[1];
if (!KERNELS[name]) {
    console.log(
        "usage: zjs tests/bench_closure_var.mjs <" +
            Object.keys(KERNELS).join("|") +
            "> [iterations=400000]",
    );
} else {
    const [fn, per] = KERNELS[name];
    bench(name, fn, per);
}
