// Benchmark: array_hole_length_decr
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function array_hole_length_decr(n)
{
    var tab, ref, i, j, len;
    len = 1000;
    ref = [];
    for(i = 0; i < len; i++) {
        if (i % 10 == 9)
            ref[i] = i;
    }
    for(j = 0; j < n; j++) {
        tab = ref.slice();
        for(i = len; i --> 0;)
            tab.length = i;
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = array_hole_length_decr(n);
console.log("global_res:", global_res, "iterations:", iterations);
