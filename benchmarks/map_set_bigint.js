// Benchmark: map_set_bigint
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function map_set_bigint(n)
{
    var s, i, j, len = 1000;
    for(j = 0; j < n; j++) {
        s = new Map();
        for(i = 0; i < len; i++) {
            s.set(BigInt(i), i);
        }
        for(i = 0; i < len; i++) {
            if (!s.has(BigInt(i)))
                throw Error("bug in Map");
        }
    }
    return n * len;
}

// Run with n=1
var n = 1;
var iterations = map_set_bigint(n);
console.log("global_res:", global_res, "iterations:", iterations);
