// Benchmark: float_arith
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function float_arith(n)
{
    var i, j, sum, a, incr, a0;
    global_res = 0;
    a0 = 0.1;
    incr = 1.1;
    for(j = 0; j < n; j++) {
        sum = 0;
        a = a0;
        for(i = 0; i < 1000; i++) {
            sum += a * a;
            a += incr;
        }
        global_res += sum;
    }
    return n * 1000;
}

// Run with n=1
var n = 1;
var iterations = float_arith(n);
console.log("global_res:", global_res, "iterations:", iterations);
