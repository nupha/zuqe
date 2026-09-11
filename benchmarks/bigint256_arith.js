// Benchmark: bigint256_arith
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function bigint_arith(n, bits)
{
    var i, j, sum, a, incr, a0, sum0;
    sum0 = global_res = BigInt(0);
    a0 = BigInt(1) << BigInt(Math.floor((bits - 10) * 0.5));
    incr = BigInt(1);
    for(j = 0; j < n; j++) {
        sum = sum0;
        a = a0;
        for(i = 0; i < 1000; i++) {
            sum += a * a;
            a += incr;
        }
        global_res += sum;
    }
    return n * 1000;
}

function bigint256_arith(n)
{
    return bigint_arith(n, 256);
}

// Run with n=1
var n = 1;
var iterations = bigint256_arith(n);
console.log("global_res:", global_res, "iterations:", iterations);
