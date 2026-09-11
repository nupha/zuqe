// Benchmark: int_to_string
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function int_to_string(n)
{
    var s, r, j;
    r = 0;
    for(j = 0; j < n; j++) {
        s = (j % 10) + '';
        s = (j % 100) + '';
        s = (j) + '';
    }
    return n * 3;
}

// Run with n=1
var n = 1;
var iterations = int_to_string(n);
console.log("global_res:", global_res, "iterations:", iterations);
