// VM-P16 语义探针：fast array 的洞语义（与 qjs 对照应一致）。
var a = new Array(3);
console.log("new Array(3): length=" + a.length + " 0 in a=" + (0 in a) + " a[0]=" + a[0]);
a[2] = 9;
console.log("after a[2]=9: 1 in a=" + (1 in a) + " a[1]=" + a[1] + " length=" + a.length);
console.log("keys=" + Object.keys(a).join(",") + " ownNames=" + Object.getOwnPropertyNames(a).join(","));
console.log("JSON=" + JSON.stringify(a));

var b = [1, , 3];
console.log("literal [1,,3]: length=" + b.length + " 1 in b=" + (1 in b) + " JSON=" + JSON.stringify(b));
console.log("keys=" + Object.keys(b).join(","));

b.length = 1;
console.log("after length=1: " + JSON.stringify(b) + " length=" + b.length);

// sort 对洞
var c = [3, , 1, , 2];
c.sort();
console.log("sorted [3,,1,,2]: " + JSON.stringify(c) + " keys=" + Object.keys(c).join(","));

// join/map 对洞
console.log("join=" + [1, , 3].join("-"));
var d = [1, , 3].map(function (x) { return x * 2; });
console.log("map=" + JSON.stringify(d) + " keys=" + Object.keys(d).join(","));

// from/concat 保洞？
console.log("concat=" + JSON.stringify([1].concat([, 2])));

// 扩展后的 in 语义（修法要保住的行为）
var e = [];
e[2] = "x";
console.log("sparse set: 0 in e=" + (0 in e) + " 1 in e=" + (1 in e) + " length=" + e.length + " keys=" + Object.keys(e).join(","));

// delete 洞
var f = [1, 2, 3];
delete f[1];
console.log("delete f[1]: 1 in f=" + (1 in f) + " f[1]=" + f[1] + " keys=" + Object.keys(f).join(",") + " length=" + f.length);
