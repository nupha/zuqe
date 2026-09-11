// Benchmark: global_func_call
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function g(a)
{
    return 1;
}

function global_func_call(n)
{
    var j, sum;
    sum = 0;
    for(j = 0; j < n; j++) {
        sum += g(j);
        sum += g(j);
        sum += g(j);
        sum += g(j);
    }
    global_res = sum;
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = global_func_call(n);
console.log("global_res:", global_res, "iterations:", iterations);
