// Benchmark: float_toPrecision
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function float_toPrecision(n)
{
    var s, r, j;
    r = 0;
    for(j = 0; j < n; j++) {
        s = (j % 10 + 0.1).toPrecision(j % 16 + 1);
        s = (j + 0.1).toPrecision(j % 16 + 1);
        s = (j * 12345678 + 0.1).toPrecision(j % 16 + 1);
    }
    return n * 3;
}

// Run with n=1
var n = 1;
var iterations = float_toPrecision(n);
console.log("global_res:", global_res, "iterations:", iterations);
