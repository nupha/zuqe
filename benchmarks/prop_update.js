// Benchmark: prop_update
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function prop_update(n)
{
    var obj, j;
    obj = {a: 1, b: 2, c:3, d:4 };
    for(j = 0; j < n; j++) {
        obj.a += j;
        obj.b += j;
        obj.c += j;
        obj.d += j;
    }
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = prop_update(n);
console.log("global_res:", global_res, "iterations:", iterations);
