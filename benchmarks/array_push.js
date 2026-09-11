// Benchmark: array_push
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function array_push(n)
{
    var tab, i, j, len;
    len = 500;
    for(j = 0; j < n; j++) {
        tab = [];
        for(i = 0; i < len; i++)
            tab.push(i);
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = array_push(n);
console.log("global_res:", global_res, "iterations:", iterations);
