// Benchmark: date_now
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function date_now(n) {
    var j;
    for(j = 0; j < n; j++) {
        Date.now();
    }
    return n;
}

// Run with n=1
var n = 1;
var iterations = date_now(n);
console.log("global_res:", global_res, "iterations:", iterations);
