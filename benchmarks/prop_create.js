// Benchmark: prop_create
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function prop_create(n)
{
    var obj, i, j;
    for(j = 0; j < n; j++) {
        obj = {};
        obj.a = 1;
        obj.b = 2;
        obj.c = 3;
        obj.d = 4;
        obj.e = 5;
        obj.f = 6;
        obj.g = 7;
        obj.h = 8;
        obj.i = 9;
        obj.j = 10;
        for(i = 0; i < 10; i++) {
            obj[i] = i;
        }
    }
    return n * 20;
}

// Run with n=1
var n = 1;
var iterations = prop_create(n);
console.log("global_res:", global_res, "iterations:", iterations);
