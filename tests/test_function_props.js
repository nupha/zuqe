// VM-P7a regression: function objects are created from a cached
// length/name/prototype shape and a non-boxed lazy `prototype`. The observable
// contract the fast path must preserve (all expectations below are from the
// spec, not from the engine):
//   - own property order is length, name, prototype
//   - length/name are configurable, non-writable, non-enumerable
//   - prototype is a writable, non-enumerable, non-configurable lazy data
//     property that materializes once and back-links `constructor`
//   - arrow functions own no `prototype`; generator functions own a plain
//     data `prototype` object
//   - SetFunctionName (anonymous function expressions, methods, symbol keys)
var failures = 0;
function check(name, cond) {
    if (!cond) { failures++; console.log("FAIL: " + name); }
}

function ownNames(o) {
    var names = Object.getOwnPropertyNames(o);
    // length/name/prototype are the only own props of a fresh function
    return names.filter(function (n) {
        return n !== "arguments" && n !== "caller";
    });
}

// 1. own property order + flags of an anonymous function expression
var f = function (y) { return y; };
check("order", ownNames(f).join(",") === "length,name,prototype");
var dl = Object.getOwnPropertyDescriptor(f, "length");
var dn = Object.getOwnPropertyDescriptor(f, "name");
var dp = Object.getOwnPropertyDescriptor(f, "prototype");
check("length value", dl.value === 1);
check("length flags", dl.writable === false && dl.enumerable === false && dl.configurable === true);
check("name value", dn.value === "f");
check("name flags", dn.writable === false && dn.enumerable === false && dn.configurable === true);
check("prototype writable/non-enum", dp.writable === true && dp.enumerable === false);
check("prototype non-configurable", dp.configurable === false);
check("prototype is object", typeof dp.value === "object" && dp.value !== null);
check("prototype constructor back-link", dp.value.constructor === f);
check("not enumerable", Object.keys(f).length === 0);

// 2. the prototype is materialized once and shared (lazy slot)
var p1 = f.prototype;
var p2 = f.prototype;
check("prototype identity", p1 === p2 && p1 === dp.value);
check("prototype proto is Object.prototype", Object.getPrototypeOf(p1) === Object.prototype);
check("prototype still first-created value", Object.getOwnPropertyDescriptor(f, "prototype").value === p1);

// 3. writable prototype: the lazy slot is replaced by an ordinary value
var g = function () {};
g.prototype = 7;
check("prototype overwrite", g.prototype === 7);
var dg = Object.getOwnPropertyDescriptor(g, "prototype");
check("overwrite flags", dg.writable === true && dg.enumerable === false && dg.configurable === false);

// 4. arrow functions own no prototype and are not constructors
var a = (x) => x;
check("arrow order", ownNames(a).join(",") === "length,name");
check("arrow no prototype", !Object.prototype.hasOwnProperty.call(a, "prototype"));
check("arrow name", a.name === "a");
var threw = false;
try { new a(); } catch (e) { threw = true; }
check("arrow not constructor", threw);

// 5. generator function: `prototype` is a plain data property whose [[Prototype]]
// is %GeneratorPrototype%
var gen = function* (v) { yield v; };
var dgen = Object.getOwnPropertyDescriptor(gen, "prototype");
check("gen order", ownNames(gen).join(",") === "length,name,prototype");
check("gen prototype data prop", dgen.writable === true && dgen.enumerable === false && dgen.configurable === false);
var GeneratorFunctionPrototype = Object.getPrototypeOf(function* () {});
check("gen prototype chain", Object.getPrototypeOf(gen.prototype) === GeneratorFunctionPrototype.prototype);
check("gen runs", gen(3).next().value === 3);

// 6. SetFunctionName: variable initializer, method shorthand, symbol key
check("anonymous gets variable name", f.name === "f");
var h = function () {};
check("second anonymous", h.name === "h");
var obj = {
    m: function () {},
    n() {},
    get k() { return 1; },
    [Symbol.iterator]: function () {}
};
check("property value name", obj.m.name === "m");
check("method shorthand name", obj.n.name === "n");
check("getter name", Object.getOwnPropertyDescriptor(obj, "k").get.name === "get k");
check("symbol key name", obj[Symbol.iterator].name === "[Symbol.iterator]");

// 7. named function expression keeps its own name (no SetFunctionName)
var nf = function named() {};
check("named expression", nf.name === "named");
check("named expression self reference", (function inner() { return inner.name; })() === "inner");
// anonymous function dropped into a call gets no name
check("call argument name", (function (cb) { return cb.name; })(function () {}) === "");

// 8. class constructors go through the same shape
class A { constructor(a, b) {} }
check("class name", A.name === "A");
check("class length", A.length === 2);
check("class order", ownNames(A).join(",") === "length,name,prototype");
check("class prototype ctor", A.prototype.constructor === A);
// class constructors are created with MakeConstructor(f, writable=false)
check("class prototype non-writable", Object.getOwnPropertyDescriptor(A, "prototype").writable === false);

// 9. defaults/rest affect length; already-created shapes are reused
function len(a, b = 1, ...rest) { return a + b; }
check("default params length", len.length === 1);
function many(n) {
    var keep = [];
    for (var i = 0; i < 64; i++) keep.push(function (x) { return x + i; });
    var names = ownNames(keep[0]).join(",");
    for (var j = 1; j < keep.length; j++) {
        if (ownNames(keep[j]).join(",") !== names) return false;
        if (keep[j].prototype.constructor !== keep[j]) return false;
    }
    return true;
}
check("many closures share one shape contract", many(1));

// 10. deleting/mutating the name slot still behaves like a normal property
var dd = function () {};
check("delete name", delete dd.name === true);
check("name gone", !Object.prototype.hasOwnProperty.call(dd, "name"));
var strictDelete = false;
try { (function () { "use strict"; delete dd.prototype; })(); } catch (e) { strictDelete = true; }
check("delete non-configurable prototype throws in strict", strictDelete === true);
check("prototype survives delete attempt", dd.prototype !== undefined);

// 11. capture lists (inline ≤2, heap for more): values must alias the frame
function fiveCaptures() {
    var a = 1, b = 2, c = 3, d = 4, e = 5;
    var f = function () { return a + b + c + d + e; };
    a = 10; e = 50;
    return f();
}
check("five captures alias the frame", fiveCaptures() === 69);

function sharedCounter() {
    var n = 0;
    return function () { return ++n; };
}
var counter1 = sharedCounter();
counter1(); counter1();
var counter2 = sharedCounter();
check("captured variable is shared per closure", counter1() === 3 && counter2() === 1);

if (failures === 0) { console.log("ALL PASS"); }
else { throw failures + " failures"; }
