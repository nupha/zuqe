// Benchmark: map_delete
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function map_delete(n)
{
    var a, i, j;

    len = 1000;
    for(j = 0; j < n; j++) {
        a = new Map();
        for(i = 0; i < len; i++) {
            a.set(String(i), i);
        }
        for(i = 0; i < len; i++) {
            a.delete(String(i));
        }
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = map_delete(n);
console.log("global_res:", global_res, "iterations:", iterations);
