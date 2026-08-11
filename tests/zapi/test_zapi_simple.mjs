// Merged demo test for the zapi_demo extension (examples/zapi_basic).
//
// Covers, at the extension boundary:
//   1. simple functions (add / mul / greet)
//   2. every value type: Number, String, Symbol, Object, Array, Function, Arguments
//   3. standard host objects used inside the extension: Date, Uint8Array
//   4. the Counter class
//
// Run after building the extension:
//   cargo build --release --manifest-path examples/zapi_basic/Cargo.toml
//   cp examples/zapi_basic/target/release/libzapi_demo.dylib tests/zapi/
//   target/release/examples/zjs --module tests/zapi/test_zapi_simple.mjs

import * as demo from "./zapi_demo.dylib";

let pass = 0;
let fail = 0;

function test(name, fn) {
    try {
        fn();
        console.log("PASS " + name);
        pass++;
    } catch (e) {
        console.log("FAIL " + name + "  (" + e + ")");
        fail++;
    }
}

function eq(actual, expected, msg) {
    if (actual !== expected) {
        throw Error((msg ? msg + ": " : "") + "got " + String(actual) + ", expected " + String(expected));
    }
}

function check(cond, msg) {
    if (!cond) {
        throw Error(msg || "condition failed");
    }
}

// ── 1. simple functions (merged from test_zapi.mjs) ──
test("add(1, 2) = 3", () => {
    eq(demo.add(1, 2), 3);
});
test("mul(3, 4) = 12", () => {
    eq(demo.mul(3, 4), 12);
});
test("greet() = 42", () => {
    eq(demo.greet(), 42);
});

// ── 2a. Number ──
test("typeOf(42) is number", () => {
    eq(demo.typeOf(42), "number");
});
test("typeOf(3.14) is number", () => {
    eq(demo.typeOf(3.14), "number");
});
test("id round-trips a number", () => {
    eq(demo.id(42), 42);
});
test("add handles doubles precisely", () => {
    eq(demo.add(1.5, 2.25), 3.75);
});
test("add handles negative numbers", () => {
    eq(demo.add(-5, 7), 2);
});

// ── 2b. String ──
test("typeOf('hi') is string", () => {
    eq(demo.typeOf("hi"), "string");
});
test("id round-trips a string", () => {
    eq(demo.id("hello"), "hello");
});
test("echoString reads and returns a string", () => {
    eq(demo.echoString("world"), "echo:world");
});
test("strLen counts UTF-8 bytes", () => {
    eq(demo.strLen("hello"), 5);
    eq(demo.strLen("你好"), 6);
});

// ── 2c. Symbol ──
test("makeSymbol creates a symbol", () => {
    const s = demo.makeSymbol("tag");
    eq(typeof s, "symbol");
    eq(demo.typeOf(s), "symbol");
});
test("symDesc reads the description", () => {
    eq(demo.symDesc(demo.makeSymbol("mytag")), "mytag");
    eq(demo.symDesc(demo.makeSymbol("")), "");
});
test("id round-trips a symbol", () => {
    const s = demo.makeSymbol("s1");
    eq(demo.id(s), s);
});

// ── 2d. Object ──
test("typeOf({}) is object", () => {
    eq(demo.typeOf({}), "object");
});
test("makeObject builds an object in the extension", () => {
    const o = demo.makeObject();
    eq(typeof o, "object");
    eq(o.kind, "demo");
    eq(o.count, 7);
    check(Array.isArray(o.nested), "nested must be an array");
    eq(o.nested[0], true);
    eq(o.nested[1], 1.5);
    eq(o.nested[2], "x");
});
test("getProp reads a property from the extension", () => {
    eq(demo.getProp({ a: 1 }, "a"), 1);
});
test("setProp writes a property from the extension", () => {
    const o = {};
    demo.setProp(o, "b", "two");
    eq(o.b, "two");
});
test("hasProp / delProp from the extension", () => {
    const o = { a: 1 };
    eq(demo.hasProp(o, "a"), true);
    eq(demo.hasProp(o, "nope"), false);
    eq(demo.delProp(o, "a"), true);
    eq(demo.hasProp(o, "a"), false);
});

// ── 2e. Array ──
test("typeOf(array) is object", () => {
    eq(demo.typeOf([1, 2]), "object");
});
test("makeArray builds an array in the extension", () => {
    const a = demo.makeArray();
    check(Array.isArray(a), "makeArray must return an array");
    eq(a.length, 5);
    eq(a[0], 1);
    eq(a[1], 2);
    eq(a[2], 3);
    eq(a[3], "four");
    eq(a[4], true);
});
test("arrayLen / arrayGet / arraySet from the extension", () => {
    const a = demo.makeArray();
    eq(demo.arrayLen(a), 5);
    eq(demo.arrayGet(a, 2), 3);
    demo.arraySet(a, 1, 99);
    eq(a[1], 99);
});
test("extension reads a JS array", () => {
    const a = [10, 20, 30];
    eq(demo.arrayLen(a), 3);
    eq(demo.arrayGet(a, 1), 20);
});

// ── 2f. Function ──
test("typeOf(function) is function", () => {
    eq(demo.typeOf(function () {}), "function");
});
test("id round-trips a function", () => {
    const f = (x) => x * 2;
    eq(demo.id(f), f);
});
test("callJs invokes a function from the extension", () => {
    eq(demo.callJs((x, y) => x * y, 6, 7), 42);
});

// ── 2g. Arguments ──
test("typeOf(arguments) is object", () => {
    (function () {
        eq(demo.typeOf(arguments), "object");
    })(1, 2);
});
test("extension reads the arguments object", () => {
    function argsToLen() {
        return demo.arrayLen(arguments);
    }
    function argsToFirst() {
        return demo.arrayGet(arguments, 0);
    }
    eq(argsToLen(1, 2, 3), 3);
    eq(argsToFirst("a", "b"), "a");
});

// ── 3a. Date used inside the extension ──
test("makeDate creates a Date (epoch)", () => {
    const d = demo.makeDate(0);
    check(d instanceof Date, "makeDate must return a Date");
    eq(d.getTime(), 0);
});
test("dateTime reads d.getTime() from the extension", () => {
    eq(demo.dateTime(new Date(123456789)), 123456789);
    const now = Date.now();
    eq(demo.dateTime(new Date(now)), now);
});
test("makeDate(now) round-trips the current time", () => {
    const now = Date.now();
    const d = demo.makeDate(now);
    check(d instanceof Date, "must be a Date");
    eq(demo.dateTime(d), now);
});

// ── 3b. Uint8Array used inside the extension ──
test("makeU8 creates a Uint8Array", () => {
    const u = demo.makeU8(4);
    check(u instanceof Uint8Array, "makeU8 must return a Uint8Array");
    eq(u.length, 4);
});
test("extension reads/writes a Uint8Array", () => {
    const u = demo.makeU8(4);
    eq(demo.u8Get(u, 0), 0);
    demo.u8Set(u, 1, 255);
    eq(u[1], 255);
});
test("extension operates on a JS-created Uint8Array", () => {
    const u = new Uint8Array([7, 8, 9]);
    eq(demo.u8Get(u, 2), 9);
    demo.u8Set(u, 0, 1);
    eq(u[0], 1);
});

// ── 4. Counter class (merged from test_zapi.mjs) ──
test("new Counter stores the label", () => {
    const c = new demo.Counter("my-counter");
    eq(c.label(), "my-counter");
});
test("Counter increments and reports its value", () => {
    const c = new demo.Counter("my-counter");
    eq(c.value(), 0);
    eq(c.increment(), 1);
    eq(c.increment(), 2);
    eq(c.value(), 2);
});

// ── summary ──
if (fail === 0) {
    console.log("ALL PASS (" + pass + " tests)");
} else {
    console.log(fail + " FAILURES (" + pass + " passed, " + fail + " failed)");
}
