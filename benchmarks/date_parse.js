// Benchmark: date_parse
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function date_parse(n) {
    var x0 = 0, dx = 0;
    var j;
    for(j = 0; j < n; j++) {
        var x1 = x0 - x0 % 1000;
        var x2 = -x0;
        var x3 = -x1;
        var d0 = new Date(x0);
        var d1 = new Date(x1);
        var d2 = new Date(x2);
        var d3 = new Date(x3);
        if (Date.parse(d0.toISOString()) != x0
        ||  Date.parse(d1.toGMTString()) != x1
        ||  Date.parse(d1.toString()) != x1
        ||  Date.parse(d2.toISOString()) != x2
        ||  Date.parse(d3.toGMTString()) != x3
        ||  Date.parse(d3.toString()) != x3) {
            console.log("Date.parse error for " + x0);
            return -1;
        }
        dx = (dx * 1.1 + 1) >> 0;
        x0 = (x0 + dx) % 8.64e15;
    }
    return n * 6;
}

// Run with n=1
var n = 1;
var iterations = date_parse(n);
console.log("global_res:", global_res, "iterations:", iterations);
