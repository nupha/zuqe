// Benchmark: global_read
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;
var global_var0;

function global_read(n)
{
    var sum, j;
    global_var0 = 0;
    sum = 0;
    for(j = 0; j < n; j++) {
        sum += global_var0;
        sum += global_var0;
        sum += global_var0;
        sum += global_var0;
    }
    global_res = sum;
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = global_read(n);
console.log("global_res:", global_res, "iterations:", iterations);
