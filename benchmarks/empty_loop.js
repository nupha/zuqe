// Benchmark: empty_loop
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function empty_loop(n) {
    var j;
    for(j = 0; j < n; j++) {
    }
    return n;
}

// Run with n=1
var n = 1;
var iterations = empty_loop(n);
console.log("global_res:", global_res, "iterations:", iterations);
