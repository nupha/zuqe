// VM-P16 归因探针：fast array 填充形态的写入成本。
// 复现 memo 记录的病理：new Array(n) 后从后往前填（index > count）走定义慢路径。
// 用法：zjs tests/bench_array_holes.js [n]
var n = (typeof scriptArgs !== "undefined" && scriptArgs[1]) ? +scriptArgs[1] : 50000;

function time(label, fn) {
    fn(); // warmup
    var t0 = Date.now();
    var r = fn();
    var ms = Date.now() - t0;
    console.log(label + ": " + ms + " ms (check=" + r + ")");
}

time("forward fill (a[i]=v, i==len)", function () {
    var a = new Array(n);
    for (var i = 0; i < n; i++) a[i] = i;
    return a[n - 1];
});

time("backward fill (a[i]=v, i>len)", function () {
    var a = new Array(n);
    for (var i = n - 1; i >= 0; i--) a[i] = i;
    return a[0];
});

time("sparse jump fill (a[i*2]=v)", function () {
    var a = new Array(n);
    for (var i = 0; i < n; i++) a[i * 2] = i;
    return a[0];
});

time("push fill", function () {
    var a = [];
    for (var i = 0; i < n; i++) a.push(i);
    return a[n - 1];
});
