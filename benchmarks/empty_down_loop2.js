// Benchmark: empty_down_loop2
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function empty_down_loop2(n) {
    var j;
    for(j = n; j --> 0;) {
    }
    return n;
}

// Run with n=1
var n = 1;
var iterations = empty_down_loop2(n);
console.log("global_res:", global_res, "iterations:", iterations);
