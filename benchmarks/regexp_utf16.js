// Benchmark: regexp_utf16
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function regexp_utf16(n)
{
    var i, j, r, s;
    s = "the quick brown ᶠᵒˣ jumped over the lazy ᵈᵒᵍ"
    for(j = 0; j < n; j++) {
        for(i = 0; i < 1000; i++)
            r = /the quick brown ᶠᵒˣ/.exec(s)
        global_res = r;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = regexp_utf16(n);
console.log("global_res:", global_res, "iterations:", iterations);
