// Benchmark: float_toFixed
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function float_toFixed(n)
{
    var s, r, j;
    r = 0;
    for(j = 0; j < n; j++) {
        s = (j % 10 + 0.1).toFixed(j % 16);
        s = (j + 0.1).toFixed(j % 16);
        s = (j * 12345678 + 0.1).toFixed(j % 16);
    }
    return n * 3;
}

// Run with n=1
var n = 1;
var iterations = float_toFixed(n);
console.log("global_res:", global_res, "iterations:", iterations);
