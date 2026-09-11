// Benchmark: math_min
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function math_min(n)
{
    var i, j, r;
    r = 0;
    for(j = 0; j < n; j++) {
        for(i = 0; i < 1000; i++)
            r = Math.min(i, 500);
        global_res = r;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = math_min(n);
console.log("global_res:", global_res, "iterations:", iterations);
