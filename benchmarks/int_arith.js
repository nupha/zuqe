// Benchmark: int_arith
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function int_arith(n)
{
    var i, j, sum;
    global_res = 0;
    for(j = 0; j < n; j++) {
        sum = 0;
        for(i = 0; i < 1000; i++) {
            sum += i * i;
        }
        global_res += sum;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = int_arith(n);
console.log("global_res:", global_res, "iterations:", iterations);
