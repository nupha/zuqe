// Benchmark: string_build3
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function string_build3(n)
{
    var i, j, r;
    for(j = 0; j < n; j++) {
        r = "";
        for(i = 0; i < 1000; i++)
            r = "x" + r;
        global_res = r;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = string_build3(n);
console.log("global_res:", global_res, "iterations:", iterations);
