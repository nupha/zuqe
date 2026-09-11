// Benchmark: weak_map_set
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function weak_map_set(n)
{
    var a, i, j, tab;

    len = 1000;
    tab = [];
    for(i = 0; i < len; i++) {
        tab.push({ key: i });
    }
    for(j = 0; j < n; j++) {
        a = new WeakMap();
        for(i = 0; i < len; i++) {
            a.set(tab[i], i);
        }
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = weak_map_set(n);
console.log("global_res:", global_res, "iterations:", iterations);
