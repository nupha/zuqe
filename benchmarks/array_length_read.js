// Benchmark: array_length_read
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function array_length_read(n)
{
    var tab, sum, j;
    tab = [1, 2, 3];
    sum = 0;
    for(j = 0; j < n; j++) {
        sum += tab.length;
        sum += tab.length;
        sum += tab.length;
        sum += tab.length;
    }
    global_res = sum;
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = array_length_read(n);
console.log("global_res:", global_res, "iterations:", iterations);
