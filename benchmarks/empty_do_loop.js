// Benchmark: empty_do_loop
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function empty_do_loop(n) {
    var j = n;
    do { } while (--j > 0);
    return n;
}

// Run with n=1
var n = 1;
var iterations = empty_do_loop(n);
console.log("global_res:", global_res, "iterations:", iterations);
