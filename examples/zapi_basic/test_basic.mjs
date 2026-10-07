// Smoke test for the zapi_basic example (examples/zapi_basic).
//
// Covers the safe zapi layer + zapi-macros:
//   1. basic types crossing the boundary (number / string / symbol / object /
//      array / function / arguments)
//   2. standard host objects (Date, Uint8Array) invoked from inside the
//      extension
//   3. the Counter class: constructor, instance methods, static method, plus
//      the class-payload gc_trace / finalize path
//   4. a module-level value export (`VERSION`)
//   5. the module's own load path (`modulePath()` vs the injected `__file__`)
//
// Run after building the extension:
//   cargo build --release --manifest-path examples/zapi_basic/Cargo.toml
//   target/debug/examples/zjs --module examples/zapi_basic/test_basic.mjs

import * as basic from "./target/release/libzapi_demo.dylib";

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

// ── 1. numbers ──
test("add(1, 2) = 3", () => eq(basic.add(1, 2), 3));
test("add handles doubles", () => eq(basic.add(1.5, 2.25), 3.75));
test("mul(6, 7) = 42", () => eq(basic.mul(6, 7), 42));

// ── 2. strings ──
test("echoString echoes a prefix", () => eq(basic.echoString("hi"), "echo:hi"));
test("strLen is the byte length", () => eq(basic.strLen("hello"), 5));

// ── 3. symbols ──
test("makeSymbol / symDesc round-trip", () => {
    const s = basic.makeSymbol("tag");
    eq(typeof s, "symbol");
    eq(basic.symDesc(s), "tag");
});

// ── 4. objects ──
test("makeObject builds a nested object", () => {
    const o = basic.makeObject();
    eq(o.kind, "demo");
    eq(o.count, 7);
    eq(o.nested[0], true);
    eq(o.nested[2], "x");
});
test("getProp / setProp / hasProp / delProp", () => {
    const o = { a: 1 };
    eq(basic.getProp(o, "a"), 1);
    eq(basic.hasProp(o, "a"), true);
    basic.setProp(o, "b", 2);
    eq(o.b, 2);
    eq(basic.delProp(o, "a"), true);
    eq(basic.hasProp(o, "a"), false);
});

// ── 5. arrays ──
test("makeArray / arrayLen / arrayGet / arraySet", () => {
    const a = basic.makeArray();
    eq(Array.isArray(a), true);
    eq(basic.arrayLen(a), 5);
    eq(basic.arrayGet(a, 0), 1);
    eq(a[4], true);
    basic.arraySet(a, 0, 99);
    eq(a[0], 99);
});

// ── 6. calling JS from the extension ──
test("callJs forwards extra args", () => eq(basic.callJs((x, y) => x * y, 6, 7), 42));

// ── 7. standard host objects ──
test("makeDate / dateTime", () => {
    const d = basic.makeDate(0);
    eq(d instanceof Date, true);
    eq(basic.dateTime(d), 0);
});
test("makeU8 / u8Get / u8Set", () => {
    const a = basic.makeU8(3);
    eq(a instanceof Uint8Array, true);
    eq(basic.u8Get(a, 0), 0);
    basic.u8Set(a, 1, 200);
    eq(basic.u8Get(a, 1), 200);
});

// ── 8. Counter class ──
test("Counter methods + static", () => {
    const c = new basic.Counter("first");
    eq(c.increment(), 1);
    eq(c.value(), 1);
    eq(c.label(), "first");
    eq(basic.Counter.isCounter(c), true);
    eq(basic.Counter.isCounter({}), false);
});

// ── 9. payload label survives GC pressure ──
test("Counter payload label survives GC", () => {
    const c = new basic.Counter("keep-me");
    let sink = [];
    for (let round = 0; round < 50; round++) {
        for (let i = 0; i < 500; i++) {
            sink.push({ i, s: "t" + i, arr: [i, i + 1, i + 2] });
        }
        if (sink.length > 5000) {
            sink.length = 0;
        }
    }
    eq(c.label(), "keep-me");
});

// ── 10. module value export ──
test("VERSION value export", () => eq(basic.VERSION, "0.1.0"));

// ── 11. the module's own load path ──
// `modulePath()` comes from the host via `ZqModuleInfo.path`; `__file__` is
// injected into the namespace. Both are derived from the same canonical path, so
// they must read identically — a divergence means one of them leaked the Windows
// `\\?\` prefix.
test("modulePath() is a non-empty string", () =>
    eq(typeof basic.modulePath(), "string"));
test("modulePath() matches the injected __file__", () =>
    eq(basic.modulePath(), basic.__file__, "modulePath vs __file__"));
test("modulePath() names this dylib", () => {
    const p = basic.modulePath();
    if (!p.endsWith("zapi_demo.dll") && !p.endsWith("libzapi_demo.dylib")) {
        throw Error("unexpected path: " + p);
    }
    if (p.startsWith("\\\\?\\")) throw Error("verbatim prefix leaked: " + p);
});

// ── summary ──
console.log(fail === 0 ? `ALL PASS (${pass} tests)` : `${fail} FAILURES (${pass} passed, ${fail} failed)`);