// Benchmark: array_pop
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function array_pop(n)
{
    var tab, ref, i, j, len, sum;
    len = 500;
    ref = [];
    for(i = 0; i < len; i++)
        ref[i] = i;
    for(j = 0; j < n; j++) {
        tab = ref.slice();
        sum = 0;
        for(i = 0; i < len; i++)
            sum += tab.pop();
        global_res = sum;
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = array_pop(n);
console.log("global_res:", global_res, "iterations:", iterations);
