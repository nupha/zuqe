// Benchmark: sort_bench
// Extracted from quickjs-2026-06-04/tests/microbench.js

function get_clock() {
    return Date.now();
}
var clocks_per_sec = 1000;

function sort_bench(text) {
    function random(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[(Math.random() * n) >> 0];
    }
    function random8(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[(Math.random() * 256) >> 0];
    }
    function random1(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[(Math.random() * 2) >> 0];
    }
    function hill(arr, n, def) {
        var mid = n >> 1;
        for (var i = 0; i < mid; i++)
            arr[i] = def[i];
        for (var i = mid; i < n; i++)
            arr[i] = def[n - i];
    }
    function comb(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[(i & 1) * i];
    }
    function crisscross(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[(i & 1) ? n - i : i];
    }
    function zero(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[0];
    }
    function increasing(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[i];
    }
    function decreasing(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[n - 1 - i];
    }
    function alternate(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[i ^ 1];
    }
    function jigsaw(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[i % (n >> 4)];
    }
    function incbutone(arr, n, def) {
        for (var i = 0; i < n; i++)
            arr[i] = def[i];
        if (n > 0)
            arr[n >> 2] = def[n];
    }
    function incbutfirst(arr, n, def) {
        if (n > 0)
            arr[0] = def[n];
        for (var i = 1; i < n; i++)
            arr[i] = def[i];
    }
    function incbutlast(arr, n, def) {
        for (var i = 0; i < n - 1; i++)
            arr[i] = def[i + 1];
        if (n > 0)
            arr[n - 1] = def[0];
    }

    var sort_cases = [ random, random8, random1, jigsaw, hill, comb,
                      crisscross, zero, increasing, decreasing, alternate,
                      incbutone, incbutlast, incbutfirst ];

    var n = sort_bench.array_size || 10000;
    var array_type = sort_bench.array_type || Array;
    var def, arr;
    var i, j, x, y;
    var total = 0;

    var save_total_score = total_score;
    var save_total_scale = total_scale;

    // initialize default sorted array (n + 1 elements)
    def = new array_type(n + 1);
    if (array_type == Array) {
        for (i = 0; i <= n; i++) {
            def[i] = i + "";
        }
    } else {
        for (i = 0; i <= n; i++) {
            def[i] = i;
        }
    }
    def.sort();
    for (var f of sort_cases) {
        var ti = 0, tx = 0;
        for (j = 0; j < 100; j++) {
            arr = new array_type(n);
            f(arr, n, def);
            var t1 = get_clock();
            arr.sort();
            t1 = get_clock() - t1;
            tx += t1;
            if (!ti || ti > t1)
                ti = t1;
            if (tx >= clocks_per_sec)
                break;
        }
        total += ti;

        i = 0;
        x = arr[0];
        if (x !== void 0) {
            for (i = 1; i < n; i++) {
                y = arr[i];
                if (y === void 0)
                    break;
                if (x > y)
                    break;
                x = y;
            }
        }
        while (i < n && arr[i] === void 0)
            i++;
        if (i < n) {
            console.log("sort_bench: out of order error for " + f.name +
                        " at offset " + (i - 1) +
                        ": " + arr[i - 1] + " > " + arr[i]);
        }
        if (sort_bench.verbose)
            log_one("sort_" + f.name, 1, ti / 100);
    }
    total_score = save_total_score;
    total_scale = save_total_scale;
    return total / n / 100;
}
sort_bench.bench = true;
sort_bench.verbose = false;

// Run
var result = sort_bench();
console.log("Result:", result);
