// Benchmark: array_for_of
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function array_for_of(n)
{
    var r, i, j, sum, len = 100;
    r = [];
    for(i = 0; i < len; i++)
        r[i] = i;
    for(j = 0; j < n; j++) {
        sum = 0;
        for(i of r) {
            sum += i;
        }
        global_res = sum;
    }
    return n * len;
}

// Run with n=1
var n = 1;
var iterations = array_for_of(n);
console.log("global_res:", global_res, "iterations:", iterations);
