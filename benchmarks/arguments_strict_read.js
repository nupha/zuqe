// Benchmark: arguments_strict_read
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function arguments_strict_test()
{
    "use strict";
    return arguments[0] + arguments[1] + arguments[2];
}

function arguments_strict_read(n) {
    sum = 0;
    for(j = 0; j < n; j++) {
        sum += arguments_strict_test(j, j, j);
        sum += arguments_strict_test(j, j, j);
        sum += arguments_strict_test(j, j, j);
        sum += arguments_strict_test(j, j, j);
    }
    global_res = sum;
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = arguments_strict_read(n);
console.log("global_res:", global_res, "iterations:", iterations);
