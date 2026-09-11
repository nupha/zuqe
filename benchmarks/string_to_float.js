// Benchmark: string_to_float
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function string_to_float(n)
{
    var s, r, j;
    r = 0;
    s = "12345.6";
    for(j = 0; j < n; j++) {
        r -= s;
    }
    global_res = r;
    return n;
}

// Run with n=1
var n = 1;
var iterations = string_to_float(n);
console.log("global_res:", global_res, "iterations:", iterations);
