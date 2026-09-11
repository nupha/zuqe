// Benchmark: empty_down_loop
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function empty_down_loop(n) {
    var j;
    for(j = n; j > 0; j--) {
    }
    return n;
}

// Run with n=1
var n = 1;
var iterations = empty_down_loop(n);
console.log("global_res:", global_res, "iterations:", iterations);
