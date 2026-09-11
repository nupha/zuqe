// Benchmark: regexp_replace
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function regexp_replace(n)
{
    var i, j, r, s;
    s = "the quick abc brown fox jumped abc over the lazy dog"
    for(j = 0; j < n; j++) {
        for(i = 0; i < 1000; i++)
            r = s.replace(/abc /g, "-");
        global_res = r;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = regexp_replace(n);
console.log("global_res:", global_res, "iterations:", iterations);
