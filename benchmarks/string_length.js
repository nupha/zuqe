// Benchmark: string_length
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function string_length(n)
{
    var str, sum, j;
    str = "abcde";
    sum = 0;
    for(j = 0; j < n; j++) {
        sum += str.length;
        sum += str.length;
        sum += str.length;
        sum += str.length;
    }
    global_res = sum;
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = string_length(n);
console.log("global_res:", global_res, "iterations:", iterations);
