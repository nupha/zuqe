// Benchmark: array_prop_create
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function array_prop_create(n)
{
    var tab, i, j, len;
    len = 1000;
    for(j = 0; j < n; j++) {
        tab = [];
        for(i = 0; i < len; i++)
            tab[i] = i;
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = array_prop_create(n);
console.log("global_res:", global_res, "iterations:", iterations);
