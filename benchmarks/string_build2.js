// Benchmark: string_build2
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function string_build2(n, r)
{
    var i, j;
    for(j = 0; j < n; j++) {
        r = "";
        for(i = 0; i < 1000; i++)
            r += "x";
        global_res = r;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = string_build2(n, "");
console.log("global_res:", global_res, "iterations:", iterations);
