// Benchmark: prop_clone
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function prop_clone(n)
{
    var ref, obj, j, k;
    ref = { a:1, b:2, c:3, d:4, e:5, f:6, g:7, h:8, i:9, j:10 };
    for(k = 0; k < 10; k++) {
        ref[k] = k;
    }
    for (j = 0; j < n; j++) {
        global_res = { ...ref };
    }
    return n * 20;
}

// Run with n=1
var n = 1;
var iterations = prop_clone(n);
console.log("global_res:", global_res, "iterations:", iterations);
