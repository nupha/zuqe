// VM-P2 regression: interpreter property get/set fast paths (IC hits)
// must preserve full semantics — accessors, tombstones, frozen, proxies,
// prototype chains, slow-path fallback after shape change.
var failures = 0;
function check(name, cond) {
    if (!cond) { failures++; console.log("FAIL: " + name); }
}

// 1. plain monomorphic data properties (the fast path itself)
var o = { a: 1, b: 2 };
check("plain get", o.a === 1 && o.b === 2);
o.a = 10;
check("plain set", o.a === 10);

// 2. getter/setter must be called, not bypassed, by the IC
var calls = [];
var acc = {
    get x() { calls.push("get"); return 42; },
    set x(v) { calls.push("set:" + v); }
};
for (var i = 0; i < 5; i++) { check("getter", acc.x === 42); }
acc.x = 7;
acc.x = 8;
check("setter called", calls.length === 7 && calls[5] === "set:7" && calls[6] === "set:8");

// 3. delete then re-read: tombstone must fall to slow path => undefined
var d = { p: 1, q: 2 };
check("pre-delete", d.p === 1);
delete d.p;
check("post-delete undefined", d.p === undefined);
check("sibling intact", d.q === 2);
d.p = 3; // re-add
check("re-add", d.p === 3);

// 4. frozen object: write via IC must not silently mutate
var fr = Object.freeze({ v: 1 });
check("frozen read", fr.v === 1);
var threw = false;
try { fr.v = 9; } catch (e) { threw = true; }
check("frozen write throws in strict... (sloppy ok)", fr.v === 1);

// 5. accessor converted from data property mid-loop
var conv = { k: 1 };
for (var i = 0; i < 5; i++) { check("conv before", conv.k === 1); }
Object.defineProperty(conv, "k", { get: function () { return 99; } });
check("conv after", conv.k === 99);

// 6. prototype chain read via same site (monomorphic -> polymorphic shape)
function Base() { this.base = "B"; }
Base.prototype.bp = "PB";
function Sub() { Base.call(this); this.sub = "S"; }
Sub.prototype = Object.create(Base.prototype);
var s1 = new Base(), s2 = new Sub();
var acc2 = 0;
for (var i = 0; i < 5; i++) { acc2 += s1.base.length; }
check("proto own", acc2 === 5);
check("proto inherited", s2.bp === "PB" && s1.bp === "PB");
check("sub own", s2.sub === "S");

// 7. proxy: get/set traps must fire (no IC bypass possible, but verify fallback)
var seen = [];
var px = new Proxy({ t: 1 }, {
    get: function (t, k) { seen.push("g:" + String(k)); return t[k]; },
    set: function (t, k, v) { seen.push("s:" + String(k)); t[k] = v; return true; }
});
for (var i = 0; i < 5; i++) { check("proxy get", px.t === 1); }
px.t = 2;
check("proxy traps", seen.length === 6 && px.t === 2);

// 8. shape polymorphism at one site: 3 shapes through the same IC site
function rd(x) { return x.m; }
var shapes = [{ m: 1 }, { m: 2, n: 20 }, { m: 3, n: 30, o: 300 }];
var sum = 0;
for (var i = 0; i < 30; i++) { sum += rd(shapes[i % 3]); }
check("poly shapes", sum === 10 * (1 + 2 + 3));

// 9. write barrier: store GC value (string) via fast path, read after GC
var wb = { s: "init" };
for (var i = 0; i < 5; i++) { wb.s = "round" + i; }
if (typeof std !== "undefined" && std.gcMemory) { std.gcMemory(); }
check("gc value survives", wb.s === "round4");

// 10. write non-gc then gc value alternating through same site
var alt = { v: 0 };
for (var i = 0; i < 10; i++) { alt.v = (i % 2) ? "str" + i : i; }
if (typeof std !== "undefined" && std.gcMemory) { std.gcMemory(); }
check("alternating gc/non-gc", alt.v === "str9");

// 11. with-statement + getter (with path must not be confused by IC)
var wo = { g: 5 };
Object.defineProperty(wo, "gg", { get: function () { return this.g * 2; } });
with (wo) { check("with getter", gg === 10); }

// 12. arguments object / length-like accessor unaffected
function flen(arr) { return arr.length; }
check("array length", flen([1, 2, 3]) === 3);

if (failures === 0) { console.log("ALL PASS"); }
else { throw failures + " failures"; }
