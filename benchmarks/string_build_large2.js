// Benchmark: string_build_large2
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function string_build_large2(n)
{
    var i, j, r, len = 20000;
    for(j = 0; j < n; j++) {
        r = "";
        for(i = 0; i < len; i++)
            r = "abcdef" + r;
        global_res = r;
    }
    return n * len;
}

// Run with n=1
var n = 1;
var iterations = string_build_large2(n);
console.log("global_res:", global_res, "iterations:", iterations);
