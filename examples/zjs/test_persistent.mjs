// Persistent dump/restore round-trip test for zjs (`dumpValue` / `restoreValue`).
// Covers the domains exercised by examples/persistent/: primitives, objects,
// arrays (incl. cyclic), functions/closures, generators, module-scope exports,
// native-function behaviour and error paths.
//
// Run with:
//   cargo run --example zjs --features persistent -- examples/zjs/test_persistent.mjs

let pass = 0;
let fail = 0;

function assertEq(desc, got, expect) {
    const ok = got === expect;
    if (ok) {
        pass++;
        console.log("OK  " + desc + ": " + got);
    } else {
        fail++;
        console.log("FAIL " + desc + ": got=" + got + " expect=" + expect);
    }
}

function assertThrows(desc, fn) {
    try {
        fn();
        fail++;
        console.log("FAIL " + desc + ": expected throw, none happened");
    } catch (e) {
        pass++;
        console.log("OK  " + desc + ": threw " + e);
    }
}

// ——— primitives ———
assertEq("undefined", restoreValue(dumpValue(undefined)), undefined);
assertEq("null", restoreValue(dumpValue(null)), null);
assertEq("bool", restoreValue(dumpValue(true)), true);
assertEq("int", restoreValue(dumpValue(42)), 42);
assertEq("negative int", restoreValue(dumpValue(-7)), -7);
assertEq("float", restoreValue(dumpValue(3.14159)), 3.14159);
assertEq("NaN", Number.isNaN(restoreValue(dumpValue(NaN))), true);
assertEq("Infinity", restoreValue(dumpValue(Infinity)), Infinity);
assertEq("string ascii", restoreValue(dumpValue("hello world")), "hello world");
assertEq("string utf8", restoreValue(dumpValue("你好，世界 🌍")), "你好，世界 🌍");
assertEq("bigint", restoreValue(dumpValue(12345678901234567890123456789n)), 12345678901234567890123456789n);
assertEq("symbol", restoreValue(dumpValue(Symbol.for("k"))) === Symbol.for("k"), true);

// ——— arrays / objects ———
assertEq(
    "array",
    JSON.stringify(restoreValue(dumpValue([1, 2, 3, "x", true, null]))),
    JSON.stringify([1, 2, 3, "x", true, null]),
);
assertEq(
    "array nested",
    JSON.stringify(restoreValue(dumpValue([[1, [2, 3]], [4, 5]]))),
    JSON.stringify([[1, [2, 3]], [4, 5]]),
);
assertEq(
    "object",
    JSON.stringify(restoreValue(dumpValue({ a: 1, b: "two", c: true }))),
    JSON.stringify({ a: 1, b: "two", c: true }),
);
assertEq(
    "object nested",
    JSON.stringify(restoreValue(dumpValue({ x: { y: { z: 42 } }, arr: [1, 2] }))),
    JSON.stringify({ x: { y: { z: 42 } }, arr: [1, 2] }),
);
{
    const o = {};
    o.self = o;
    const back = restoreValue(dumpValue(o));
    assertEq("cyclic object identity", back.self === back, true);
}

// ——— functions / closures (restore then call) ———
const inc = restoreValue(dumpValue(function (x) {
    return x + 1;
}));
assertEq("function call", inc(41), 42);

const arrow = restoreValue(dumpValue((x) => x * 3));
assertEq("arrow function", arrow(5), 15);

const def = restoreValue(dumpValue((function (a, b = 10) {
    return a + b;
})));
assertEq("default params", def(5), 15);

const rest = restoreValue(dumpValue((function (...args) {
    return args.length;
})));
assertEq("rest params", rest(1, 2, 3), 3);

{
    const base = 100;
    const g = restoreValue(dumpValue((function (x) {
        return x + base;
    })));
    assertEq("closure capture", g(5), 105);
}

{
    const g = restoreValue(dumpValue(function (a) {
        return function (b) {
            return a * b;
        };
    }));
    assertEq("nested closure", g(3)(4), 12);
}

const named = restoreValue(dumpValue(function myFunc(a, b) {
    return a + b;
}));
assertEq("function name", named.name, "myFunc");
assertEq("function length", named.length, 2);

const gen = restoreValue(dumpValue(function* () {
    yield 1;
    yield 2;
}));
assertEq("generator", gen().next().value, 1);

// ——— module-scope export (this file runs as a module) ———
export function addModule(a, b) {
    return a + b;
}
export const moduleConst = 42;
const addBack = restoreValue(dumpValue(addModule));
assertEq("module-scope function", addBack(2, 3), 5);

// ——— dumpValue returns a Uint8Array ———
assertEq("dumpValue returns Uint8Array", dumpValue(1) instanceof Uint8Array, true);

// ——— native function: persisted by name only; Math.abs's name "abs" is not
// ——— exposed on the global object, so restore must fail cleanly ———
assertThrows("restore native Math.abs", () => restoreValue(dumpValue(Math.abs)));

// ——— error paths ———
assertThrows("restore bad magic", () => restoreValue(new Uint8Array([1, 2, 3])));
assertThrows("restore truncated blob", () =>
    restoreValue(new Uint8Array([0x5a, 0x42, 0x43, 0x50, 0, 0, 0, 0, 4, 0]))
);
assertThrows("restore non-Uint8Array", () => restoreValue([1, 2, 3]));

console.log("");
console.log(pass + " passed, " + fail + " failed");
if (fail > 0) {
    throw new Error(fail + " persistent round-trip check(s) failed");
}
