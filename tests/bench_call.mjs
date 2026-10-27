// SPDX-License-Identifier: Apache-2.0
// VM-P7b attribution probe: call round-trip cost, one kernel per process,
// 2 warm-up + 7 timed rounds, median.
//
//   zjs tests/bench_call.js plain
//   zjs tests/bench_call.js closure
//   zjs tests/bench_call.js create2

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

function add1(x) {
    return (x + 1) | 0;
}

function plain(n) {
    let sum = 0;
    for (let i = 0; i < n; i++) sum = add1(sum);
    return sum;
}

function closure(n) {
    const f = (x) => (x + 1) | 0;
    let sum = 0;
    for (let i = 0; i < n; i++) sum = f(sum);
    return sum;
}

function makeAdder(x) {
    return (y) => (x + y) | 0;
}

function create2(n) {
    let sum = 0;
    for (let i = 0; i < n; i++) {
        const a = makeAdder(i);
        const b = makeAdder(i);
        sum = a(sum);
        sum = b(sum);
    }
    return sum;
}

function inlineAdd(n) {
    let sum = 0;
    for (let i = 0; i < n; i++) sum = (sum + 1) | 0;
    return sum;
}

function emptyLoop(n) {
    for (let i = 0; i < n; i++) {}
    return 0;
}

function emptyCall(n) {
    let sum = 0;
    for (let i = 0; i < n; i++) noop(sum);
    return sum;
}

function noop() {}

const KERNELS = {
    plain: () => plain(2000000),
    closure: () => closure(2000000),
    create2: () => create2(200000),
    inline: () => inlineAdd(2000000),
    emptyloop: () => emptyLoop(2000000),
    emptycall: () => emptyCall(2000000),
};

const name = scriptArgs[1];
const fn = KERNELS[name];
if (!fn) {
    console.log("usage: zjs tests/bench_call.js <" + Object.keys(KERNELS).join("|") + ">");
} else {
    const per = { plain: 2000000, closure: 2000000, create2: 200000, inline: 2000000, emptyloop: 2000000, emptycall: 2000000 }[name];
    bench(name, fn, per);
}
