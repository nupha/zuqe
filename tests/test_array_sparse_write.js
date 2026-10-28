// VM-P16 回归：稀疏写入（index > count）走 shape 慢路径的语义与成本。
// 修复前：new Array(n) 从后往前填走 O(n²)（50k 次 56 s）；修复后摊还 O(1)。
// 语义与 QuickJS 逐项对照（tests/probe_holes.js diff IDENTICAL）。
function assert(cond, msg) { if (!cond) throw new Error("FAIL: " + msg); }

// 1. backward fill：洞语义、length、keys
var n = 2000;
var a = new Array(n);
for (var i = n - 1; i >= 0; i--) a[i] = i;
assert(a.length === n, "backward length");
assert(a[0] === 0 && a[n - 1] === n - 1, "backward values");
assert((0 in a) && ((n >> 1) in a), "backward in");
assert(Object.keys(a).length === n, "backward keys");
assert(JSON.stringify(a).length > 100, "backward json");

// 2. 部分填充（中间洞）：keys 只列实有索引
var b = new Array(10);
b[9] = 1;
assert(Object.keys(b).join(",") === "9", "partial keys");
assert(!(5 in b), "partial hole in");
assert(b[5] === undefined, "partial hole read");
assert(b.length === 10, "partial length");

// 3. sparse jump（i*2）
var c = new Array(500);
for (var i = 0; i < 500; i++) c[i * 2] = i;
assert(c[0] === 0 && c[998] === 499, "jump values");
assert(!(1 in c) && (998 in c), "jump holes");
assert(c.length === 999, "jump length grows to max index + 1");

// 4. 普通对象唯一 key（同一 shape 慢路径）
var o = {};
for (var i = 5000; i >= 1; i--) o[i] = i * 2;
assert(o[1] === 2 && o[5000] === 10000, "obj values");
assert(Object.keys(o).length === 5000, "obj keys");
assert(Object.keys(o)[0] === "1", "obj key order");

// 5. 稀疏化后再 delete + 重加（re-add 路径与就地追加交互）
var d = new Array(10);
d[5] = 5;
delete d[5];
d[5] = 55;
assert(d[5] === 55, "delete then re-add");
assert(Object.keys(d).join(",") === "5", "delete re-add keys order");

// 6. 稀疏写入后追加普通属性（shape 链继续增长）
var e = new Array(4);
e[3] = "x";
e.foo = 1;
e.bar = 2;
assert(e[3] === "x" && e.foo === 1 && e.bar === 2, "mixed props");
assert(Object.keys(e).join(",") === "3,foo,bar", "mixed key order");

// 7. getter/setter 不受影响
var f = new Array(4);
f[2] = 1;
Object.defineProperty(f, "g", { get: function () { return f[2] * 10; } });
assert(f.g === 10, "getter after sparse");
f[1] = 7; // 写入继续走 shape 路径
assert(f[1] === 7 && Object.keys(f).join(",") === "1,2", "write after getter");

// 8. 非 extensible / 冻结
var h = new Array(2);
h[1] = 1;
Object.preventExtensions(h);
try { h[5] = 5; assert(false, "preventExtensions should throw in strict? non-strict no-throw"); } catch (ex) { /* sloppy: no throw */ }
assert(!(5 in h), "preventExtensions blocks index");
var k = Object.freeze([1, , 3]);
try { k[4] = 4; } catch (ex) { /* strict in module? tolerate */ }
assert(k.length === 3, "frozen length");

// 9. 原型链 setter 优先于稀疏创建
var proto = { set p(v) { this._p = v; } };
var j = Object.create(proto);
j[3] = 1; // 普通对象，无稀疏语义，但验证 setter 原型不受影响
j.p = 2;
assert(j._p === 2, "proto setter");

// 10. 性 sanity：50k backward fill 不得退化（修复前 56 s，这里 2 s 超时即抓回归）
var t = Date.now();
var big = new Array(50000);
for (var i = 49999; i >= 0; i--) big[i] = i;
var ms = Date.now() - t;
assert(big[0] === 0 && big[49999] === 49999, "big backward values");
assert(ms < 2000, "backward fill must stay O(n), took " + ms + " ms");

console.log("test_array_sparse_write: all pass");
