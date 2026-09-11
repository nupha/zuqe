// Benchmark: local_destruct
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function local_destruct(n)
{
    var j, v1, v2, v3, v4;
    var array = [ 1, 2, 3, 4, 5];
    var o = { a:1, b:2, c:3, d:4 };
    var a, b, c, d;
    for(j = 0; j < n; j++) {
        [ v1, v2,, v3, ...v4] = array;
        ({ a, b, c, d } = o);
        ({ a: a, b: b, c: c, d: d } = o);
    }
    return n * 12;
}

// Run with n=1
var n = 1;
var iterations = local_destruct(n);
console.log("global_res:", global_res, "iterations:", iterations);
