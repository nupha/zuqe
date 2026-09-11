// Benchmark: regexp_ascii
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function regexp_ascii(n)
{
    var i, j, r, s;
    s = "the quick brown fox jumped over the lazy dog"
    for(j = 0; j < n; j++) {
        for(i = 0; i < 1000; i++)
            r = /the quick brown fox/.exec(s)
        global_res = r;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = regexp_ascii(n);
console.log("global_res:", global_res, "iterations:", iterations);
