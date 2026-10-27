// Reproduction for the retained-object GC pathology (VM-G1):
// allocating N objects and *keeping them alive* is quadratic-ish — each GC
// round traces the whole live set again, and rounds keep coming.
//
// Usage: zjs tests/bench_gc_retain.js [N] [objects|closures]
//   N         items to keep (default 200000)
//   kind      `objects` (default) or `closures`
//
// Prints elapsed ms and ns per item. Compare against QuickJS on the same
// machine; the memo records the numbers (VM-G1 in memo/vm_todo.md).
var N = (typeof scriptArgs !== "undefined" && scriptArgs[1]) ? +scriptArgs[1] : 200000;
var KIND = (typeof scriptArgs !== "undefined" && scriptArgs[2]) ? scriptArgs[2] : "objects";

function keep_objects(n) {
    var a = new Array(n);
    for (var i = 0; i < n; i++) a[i] = {};
    return a.length;
}

function keep_closures(n) {
    var x = 7, a = new Array(n);
    for (var i = 0; i < n; i++) a[i] = function (y) { return (x + y) | 0; };
    return a.length;
}

var fn = KIND === "closures" ? keep_closures : keep_objects;
var got = 0;
var t0 = Date.now();
for (var round = 0; round < 3; round++) got = fn(N);
var ms = (Date.now() - t0) / 3;
if (got !== N) throw new Error("bad result " + got);
console.log("retain " + KIND + " N=" + N + ": " + ms + "ms  " + ((ms * 1e6) / N).toFixed(1) + " ns/item");
