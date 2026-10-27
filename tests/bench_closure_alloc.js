// VM-P7a probe: closure creation only (no call).
//
// Each kernel evaluates a function expression N times and keeps only the last
// one, so what is measured is `FCLOSURE` + the garbage it leaves behind — the
// form the P7a attribution used (memo/vm_todo.md §1, "只创建不调用").
//
// Timing with Date.now(); every kernel is run several times and the median is
// reported. Compare two builds by running them alternately on the same machine
// (single runs and cross-session numbers are not comparable — see the memo's
// VM-P1/VM-P2 measurement notes).
var N = 200000;

// Anonymous function with a `prototype` (non-arrow): the common case.
function closure_alloc(n) {
    var f;
    for (var i = 0; i < n; i++) f = function (y) { return y; };
    return typeof f;
}

// Same, plus one captured variable (VarRef reuse + captured list).
function closure_alloc_capture(n) {
    var x = 7, f;
    for (var i = 0; i < n; i++) f = function (y) { return (x + y) | 0; };
    return typeof f;
}

// Arrow: no `prototype` property, so no third slot.
function closure_alloc_arrow(n) {
    var f;
    for (var i = 0; i < n; i++) f = (y) => y;
    return typeof f;
}

// Named function: `name` is set at creation, no SetFunctionName afterwards.
function closure_alloc_named(n) {
    var f;
    for (var i = 0; i < n; i++) f = function g(y) { return y; };
    return typeof f;
}

// Generator function: `prototype` is a plain data property.
function closure_alloc_gen(n) {
    var f;
    for (var i = 0; i < n; i++) f = function* (y) { yield y; };
    return typeof f;
}

var KERNELS = [
    ["closure_alloc", closure_alloc],
    ["closure_alloc_capture", closure_alloc_capture],
    ["closure_alloc_arrow", closure_alloc_arrow],
    ["closure_alloc_named", closure_alloc_named],
    ["closure_alloc_gen", closure_alloc_gen],
];

function median(a) {
    a.sort(function (x, y) { return x - y; });
    return a[a.length >> 1];
}

function measure(name, f) {
    f(1000); // warm-up
    var samples = [];
    for (var r = 0; r < 7; r++) {
        var t0 = Date.now();
        var got = f(N);
        var dt = Date.now() - t0;
        if (got !== "function") throw new Error(name + ": bad result " + got);
        samples.push(dt);
    }
    var ms = median(samples);
    console.log(name + ": " + ms + "ms  " + ((ms * 1e6) / N).toFixed(1) + " ns/iter");
    return ms;
}

for (var i = 0; i < KERNELS.length; i++) {
    measure(KERNELS[i][0], KERNELS[i][1]);
}
