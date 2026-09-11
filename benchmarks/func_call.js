// Benchmark: func_call
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function func_call(n)
{
    function f(a)
    {
        return 1;
    }

    var j, sum;
    sum = 0;
    for(j = 0; j < n; j++) {
        sum += f(j);
        sum += f(j);
        sum += f(j);
        sum += f(j);
    }
    global_res = sum;
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = func_call(n);
console.log("global_res:", global_res, "iterations:", iterations);
