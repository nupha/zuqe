// Benchmark: float_toString
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function float_toString(n)
{
    var s, r, j;
    r = 0;
    for(j = 0; j < n; j++) {
        s = (j % 10 + 0.1).toString();
        s = (j + 0.1).toString();
        s = (j * 12345678 + 0.1).toString();
    }
    return n * 3;
}

// Run with n=1
var n = 1;
var iterations = float_toString(n);
console.log("global_res:", global_res, "iterations:", iterations);
