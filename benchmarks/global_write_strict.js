// Benchmark: global_write_strict
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;
var global_var0;

function global_write_strict(n)
{
    "use strict";
    var j;
    for(j = 0; j < n; j++) {
        global_var0 = j;
        global_var0 = j;
        global_var0 = j;
        global_var0 = j;
    }
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = global_write_strict(n);
console.log("global_res:", global_res, "iterations:", iterations);
