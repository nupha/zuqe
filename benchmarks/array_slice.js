// Benchmark: array_slice
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function array_slice(n)
{
    var ref, a, i, j, len;
    len = 1000;
    ref = [];
    for(i = 0; i < len; i++)
        ref[i] = i;
    for(j = 0; j < n; j++) {
        ref[0] = j;
        a = ref.slice();
        a[0] = 0;
        global_res = a;
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = array_slice(n);
console.log("global_res:", global_res, "iterations:", iterations);
