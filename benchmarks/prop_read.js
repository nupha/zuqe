// Benchmark: prop_read
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function prop_read(n)
{
    var obj, sum, j;
    obj = {a: 1, b: 2, c:3, d:4 };
    sum = 0;
    for(j = 0; j < n; j++) {
        sum += obj.a;
        sum += obj.b;
        sum += obj.c;
        sum += obj.d;
    }
    global_res = sum;
    return n * 4;
}

// Run with n=1
var n = 1;
var iterations = prop_read(n);
console.log("global_res:", global_res, "iterations:", iterations);
