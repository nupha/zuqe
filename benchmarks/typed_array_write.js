// Benchmark: typed_array_write
// Extracted from quickjs-2026-06-04/tests/microbench.js

var global_res;

function typed_array_write(n)
{
    var tab, len, i, j;
    len = 10;
    tab = new Int32Array(len);
    for(i = 0; i < len; i++)
        tab[i] = i;
    for(j = 0; j < n; j++) {
        tab[0] = j;
        tab[1] = j;
        tab[2] = j;
        tab[3] = j;
        tab[4] = j;
        tab[5] = j;
        tab[6] = j;
        tab[7] = j;
        tab[8] = j;
        tab[9] = j;
    }
    return len * n;
}

// Run with n=1
var n = 1;
var iterations = typed_array_write(n);
console.log("global_res:", global_res, "iterations:", iterations);
