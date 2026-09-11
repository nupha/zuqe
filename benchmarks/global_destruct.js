// Benchmark: global_destruct
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;
var global_v1, global_v2, global_v3, global_v4;
var global_a, global_b, global_c, global_d;

function global_destruct(n)
{
    var j, v1, v2, v3, v4;
    var array = [ 1, 2, 3, 4, 5 ];
    var o = { a:1, b:2, c:3, d:4 };
    var a, b, c, d;
    for(j = 0; j < n; j++) {
        [ global_v1, global_v2,, global_v3, ...global_v4] = array;
        ({ a: global_a, b: global_b, c: global_c, d: global_d } = o);
    }
    return n * 8;
}

// Run with n=1
var n = 1;
var iterations = global_destruct(n);
console.log("global_res:", global_res, "iterations:", iterations);
