// Smoke test for the zapi_raw example (examples/zapi_raw).
//
// Covers, against a module written purely on the raw `ZqApi` FFI (no safe
// layer, no zapi-macros):
//   1. basic types crossing the boundary (number / string / boolean / symbol /
//      object / array / function)
//   2. the Box class: constructor, instance methods, static method, static
//      accessor, plus the class-payload gc_trace / finalize path
//   3. the module: FUNC / CLASS / VALUE exports and the init hook
//   4. the thread_local global node kept alive by a global GC trace callback
//
// Run after building the extension:
//   cargo build --manifest-path examples/zapi_raw/Cargo.toml
//   target/release/examples/zjs --module examples/zapi_raw/test_raw.mjs

import * as raw from "./target/debug/libzapi_raw.dylib";

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

// ── 1. basic types ──
test("add(1, 2) = 3", () => eq(raw.add(1, 2), 3));
test("add handles doubles", () => eq(raw.add(1.5, 2.25), 3.75));
test("concat joins strings", () => eq(raw.concat("foo", "bar"), "foobar"));
test("typeOf covers the type codes", () => {
    eq(raw.typeOf(42), "number");
    eq(raw.typeOf("hi"), "string");
    eq(raw.typeOf(true), "boolean");
    eq(raw.typeOf(undefined), "undefined");
    eq(raw.typeOf(null), "object");
    eq(raw.typeOf({}), "object");
    eq(raw.typeOf(() => 1), "function");
    eq(raw.typeOf(10n), "bigint");
});
test("id round-trips values", () => {
    eq(raw.id(42), 42);
    eq(raw.id("x"), "x");
});
test("truthy = ToBoolean", () => {
    eq(raw.truthy(1), true);
    eq(raw.truthy(0), false);
    eq(raw.truthy(""), false);
    eq(raw.truthy("x"), true);
});
test("symbol creation + description", () => {
    const s = raw.makeSymbol("tag");
    eq(typeof s, "symbol");
    eq(raw.symDesc(s), "tag");
});

// ── 2. objects & arrays ──
test("makePoint builds {x, y}", () => {
    const p = raw.makePoint(3, 4);
    eq(p.x, 3);
    eq(p.y, 4);
    eq(raw.pointDist(p), 5);
});
test("pointDist reads a JS-made object", () => {
    eq(raw.pointDist({ x: 1, y: 2 }), Math.sqrt(5));
});
test("makeList / listLen", () => {
    const a = raw.makeList(4);
    eq(raw.listLen(a), 4);
    eq(a[0], 0);
    eq(a[3], 3);
    eq(Array.isArray(a), true);
});
test("callJs invokes a JS function", () => {
    eq(raw.callJs((x, y) => x * y, 6, 7), 42);
});

// ── 3. Box class ──
test("new Box stores label and default child", () => {
    const b = new raw.Box("hello");
    eq(b.label(), "hello");
    eq(typeof b.child(), "object");
    eq(raw.Box.isBox(b), true);
    eq(raw.Box.isBox({}), false);
    eq(raw.Box.kind, "native-box");
});
test("Box label/child are read/write via payload", () => {
    const b = new raw.Box("a");
    b.setLabel("b");
    eq(b.label(), "b");
    b.setChild(42);
    eq(b.child(), 42);
});

// ── 4. module (STR constant + VALUE export + init hook) ──
test("VERSION string constant export", () => eq(raw.VERSION, "0.1.0"));
test("STATS lazy value export", () => {
    eq(raw.STATS.built, true);
});

// ── 5. host constant values (read from the ZqApi table, no FFI call) ──
test("constants() exposes host value handles", () => {
    const c = raw.constants();
    eq(c.undef, undefined);
    eq(c.null, null);
    eq(c.true, true);
    eq(c.false, false);
    eq(typeof c.nan, "number");
    eq(Number.isNaN(c.nan), true);
    eq(c.zero, 0);
    eq(c.one, 1);
    eq(c.negOne, -1);
    eq(c.inf, Infinity);
    eq(c.negInf, -Infinity);
});

// ── 6. error signaling: returning value_exception makes the host throw ──
test("boom() throws via the exception sentinel", () => {
    let threw = false;
    try {
        raw.boom();
    } catch (e) {
        threw = true;
    }
    eq(threw, true);
});

// ── 7. thread_local global node (kept alive by global GC trace) ──
test("setGlobal / getGlobal round-trip", () => {
    raw.setGlobal("stored");
    eq(raw.getGlobal(), "stored");
    raw.setGlobal(123);
    eq(raw.getGlobal(), 123);
});

// ── 8. GC stress: class payload trace + thread_local trace must keep the
//      values alive across many collections. The engine triggers a GC roughly
//      every 32 allocations, so the loop below forces dozens of collections. ──
test("payload & thread_local survive GC pressure", () => {
    const b = new raw.Box("keep-me");
    raw.setGlobal("keep-global");
    let sink = [];
    for (let round = 0; round < 100; round++) {
        for (let i = 0; i < 500; i++) {
            sink.push({ i, s: "t" + i, arr: [i, i + 1, i + 2] });
        }
        if (sink.length > 5000) {
            sink.length = 0;
        }
    }
    eq(b.label(), "keep-me");
    eq(raw.getGlobal(), "keep-global");
});

// ── 9. finalize: the GC must drop short-lived Box payloads ──
test("finalize drops collected Box payloads", () => {
    const before = raw.droppedBoxes();
    let sink = [];
    for (let round = 0; round < 200; round++) {
        for (let i = 0; i < 500; i++) {
            new raw.Box("tmp").label(); // short-lived instance
            sink.push({ i, arr: [i, i + 1, i + 2] });
        }
        if (sink.length > 5000) {
            sink.length = 0;
        }
    }
    const after = raw.droppedBoxes();
    if (!(after > before)) {
        throw Error("finalize not observed: " + before + " -> " + after);
    }
});

// ── summary ──
if (fail === 0) {
    console.log("ALL PASS (" + pass + " tests)");
} else {
    console.log(fail + " FAILURES (" + pass + " passed, " + fail + " failed)");
}
