// Zapi FFI / high-level API conformance suite.
//
// Drives the `zapi_ffi_test` extension; every `ZqApi` function pointer is
// exercised here or through the safe wrapper. Async cases use top-level await
// (zjs must be built with the `zapi` feature).
//
//   cargo build --manifest-path examples/zapi_ffi_test/Cargo.toml
//   cargo build --example zjs --features zapi
//   target/debug/examples/zjs --module examples/zapi_ffi_test/test_ffi.mjs

import * as t from "./target/debug/libzapi_ffi_test.dylib";

let pass = 0;
let fail = 0;
const failed = [];

async function test(name, fn) {
    try {
        await fn();
        pass++;
        console.log("PASS " + name);
    } catch (e) {
        fail++;
        failed.push(name);
        console.log("FAIL " + name + "  (" + e + ")");
    }
}

function eq(actual, expected, msg) {
    if (actual !== expected) {
        throw Error((msg ? msg + ": " : "") + "got " + String(actual) + ", expected " + String(expected));
    }
}

function check(cond, msg) {
    if (!cond) throw Error(msg || "condition failed");
}

async function settle() {
    // Force the event loop to run at least one tick (services pending GC).
    await t.asyncResolve(0);
}

// ── const_values ──
await test("kUndefined()", () => eq(t.kUndefined(), undefined));
await test("kNull()", () => eq(t.kNull(), null));
await test("kTrue()/kFalse()", () => { eq(t.kTrue(), true); eq(t.kFalse(), false); });
await test("kZero() is +0", () => { eq(t.kZero(), 0); check(Object.is(t.kZero(), 0)); });
await test("kNaN()", () => check(Number.isNaN(t.kNaN())));
await test("kPosInf()/kNegInf()", () => { eq(t.kPosInf(), Infinity); eq(t.kNegInf(), -Infinity); });
await test("const_values slots match constructors", () => eq(t.constSlots(), 9));

// ── constructors ──
await test("nInt32", () => eq(t.nInt32(42), 42));
await test("nNumber", () => eq(t.nNumber(3.5), 3.5));
await test("nString (UTF-8)", () => eq(t.nString("héllo 你好"), "héllo 你好"));
await test("nSymbol + symbolDesc", () => {
    const s = t.nSymbol("tag");
    eq(typeof s, "symbol");
    eq(t.symbolDesc(s), "tag");
});
await test("nObject", () => {
    const o = t.nObject();
    eq(o.kind, "demo");
    eq(o.n, 3);
    check(Array.isArray(o.arr));
    eq(o.arr[0], 1);
});
await test("nArray + array_push", () => {
    const a = t.nArray();
    check(Array.isArray(a));
    eq(a.length, 4);
    eq(a[3], "four");
});

// ── BigInt ──
await test("new_bigint_i64 + value_to_int64", () => eq(t.vI64(t.nBigIntI64(-9007199254740991)), -9007199254740991));
await test("new_bigint_u64 (full range)", () => {
    eq(t.nBigIntU64Str("18446744073709551615"), 18446744073709551615n);
});
await test("value_to_int64 rejects out-of-range BigInt", () => {
    let threw = false;
    try { t.vI64(t.nBigIntU64Str("18446744073709551615")); } catch (e) { threw = true; }
    check(threw, "expected overflow to throw");
});

// ── new_error ──
await test("nError creates typed Error values", () => {
    const e = t.nError(4, "boom");
    check(e instanceof TypeError);
    eq(e.message, "boom");
    check(t.nError(1, "r") instanceof RangeError);
    check(t.nError(3, "s") instanceof SyntaxError);
});

// ── value_get_type ──
await test("vType covers every JS type", () => {
    eq(t.vType(42), "number");
    eq(t.vType("s"), "string");
    eq(t.vType(Symbol("x")), "symbol");
    eq(t.vType(1n), "bigint");
    eq(t.vType({}), "object");
    eq(t.vType(() => {}), "function");
    eq(t.vType(true), "boolean");
    eq(t.vType(undefined), "undefined");
    eq(t.vType(null), "object");
});

// ── conversions ──
await test("value_to_bool", () => { eq(t.vBool(0), false); eq(t.vBool(""), false); eq(t.vBool("x"), true); });
await test("value_to_int64", () => eq(t.vI64(3.9), 3));
await test("value_to_float64", () => eq(t.vF64("3.5"), 3.5));
await test("value_to_int32 (ToInt32 wrap)", () => eq(t.vInt32(4294967301), 5));
await test("value_to_string", () => eq(t.vToString(123), "123"));
await test("string_read_utf8", () => eq(t.vUtf8("你好, world"), "你好, world"));
await test("value_strict_equal", () => {
    check(t.vStrict("a", "a"));
    check(!t.vStrict("a", "b"));
    check(t.vStrict(0, -0));
    check(!t.vStrict(NaN, NaN));
});

// ── properties (raw key) ──
await test("get_property / set_property", () => {
    eq(t.pGet({ a: 1 }, "a"), 1);
    const o = {};
    t.pSet(o, "b", 2);
    eq(o.b, 2);
});
await test("has_property / delete_property", () => {
    eq(t.pHas({ a: 1 }, "a"), true);
    eq(t.pHas({}, "a"), false);
    const o = { a: 1 };
    eq(t.pDel(o, "a"), true);
    eq("a" in o, false);
});

// ── properties (UTF-8 key) ──
await test("get_property_str / set_property_str", () => {
    eq(t.pGetStr({ abc: 7 }), 7);
    const o = {};
    t.pSetStr(o, 8);
    eq(o.abc, 8);
});
await test("has_property_str / delete_property_str", () => {
    eq(t.pHasStr({ abc: 1 }), true);
    eq(t.pHasStr({}), false);
    const o = { abc: 1 };
    eq(t.pDelStr(o), true);
    eq("abc" in o, false);
});
await test("get_length_prop", () => eq(t.pLen([1, 2, 3]), 3));

// ── property definition (descriptor) / own-property introspection ──
await test("define_property (data + flags)", () => {
    const o = {};
    t.pDefine(o, "k", 1, 1 | 2 | 4); // configurable+writable+enumerable
    eq(o.k, 1);
    eq(Object.keys(o).includes("k"), true);

    const hidden = {};
    t.pDefine(hidden, "k", 2, 2); // writable, non-enumerable
    eq(hidden.k, 2);
    eq(Object.keys(hidden).includes("k"), false);

    const ro = {};
    t.pDefine(ro, "k", 3, 4); // enumerable only -> non-writable
    let threw = false;
    try { ro.k = 9; } catch (e) { threw = true; }
    eq(ro.k, 3);
    check(threw, "write to a non-writable prop should throw in strict mode");
});
await test("define_property_str", () => {
    const o = {};
    t.pDefineStr(o, 5, 1 | 2 | 4);
    eq(o.abc, 5);
    eq(Object.keys(o).includes("abc"), true);
});
await test("define_accessor (get/set)", () => {
    const o = {};
    let store = 10;
    t.pDefineAccessor(o, "p", () => store, (v) => { store = v; }, 1 | 2 | 4);
    eq(o.p, 10);
    o.p = 20;
    eq(o.p, 20);
    eq(Object.keys(o).includes("p"), true);
});
await test("define_accessor_str", () => {
    const o = {};
    let v = 1;
    t.pDefineAccessorStr(o, () => v, (x) => { v = x; }, 0);
    eq(o.abc, 1);
    o.abc = 7;
    eq(o.abc, 7);
    eq(Object.keys(o).includes("abc"), false);
});
await test("get_own_property_descriptor", () => {
    const o = {};
    Object.defineProperty(o, "k", {
        value: 1, writable: false, enumerable: true, configurable: false,
    });
    const d = t.pOwnDesc(o, "k");
    eq(d.accessor, false);
    eq(d.value, 1);
    eq(d.writable, false);
    eq(d.enumerable, true);
    eq(d.configurable, false);
    eq(t.pOwnDesc(o, "missing"), undefined);

    const a = {};
    t.pDefineAccessor(a, "g", () => 42, undefined, 4);
    const da = t.pOwnDesc(a, "g");
    eq(da.accessor, true);
    eq(da.get(), 42);
    eq(da.set, undefined);
    eq(da.enumerable, true);
});
await test("get_own_property_descriptor_str", () => {
    const o = {};
    t.pDefineStr(o, 9, 1 | 2 | 4);
    const d = t.pOwnDescStr(o);
    eq(d.value, 9);
    eq(d.writable, true);
    eq(d.enumerable, true);
    eq(d.configurable, true);
});
await test("get_own_property_names", () => {
    const s = Symbol("s");
    const o = { a: 1, 2: true };
    o[s] = 3;
    eq(t.pOwnNames(o, false).join(","), "2,a");
    const withSym = t.pOwnNames(o, true);
    eq(withSym.length, 3);
    check(typeof withSym[2] === "symbol", "symbol key should be included last");
});

// ── type predicates + object-with-proto + arbitrary-target instanceof ──
await test("is_array", () => {
    eq(t.vIsArray([]), true);
    eq(t.vIsArray({}), false);
    eq(t.vIsArray("x"), false);
    eq(t.vIsArray(1), false);
});
await test("is_callable", () => {
    eq(t.vIsCallable(() => {}), true);
    eq(t.vIsCallable(function () {}), true);
    eq(t.vIsCallable({}), false);
    eq(t.vIsCallable(1), false);
});
await test("is_constructor", () => {
    eq(t.vIsConstructor(function () {}), true);
    eq(t.vIsConstructor(class {}), true);
    eq(t.vIsConstructor(() => {}), false);
    eq(t.vIsConstructor({}), false);
});
await test("ordinary_object_create_with_proto", () => {
    const proto = { m() { return 7; } };
    const o = t.nObjectWithProto(proto);
    eq(Object.getPrototypeOf(o), proto);
    eq(o.m(), 7);
    const nullProto = t.nObjectWithProto(null);
    eq(Object.getPrototypeOf(nullProto), null);
    eq(typeof nullProto.toString, "undefined");
});
await test("instanceof (arbitrary target / Symbol.hasInstance)", () => {
    class A {}
    const a = new A();
    eq(t.vInstanceOf(a, A), true);
    eq(t.vInstanceOf(a, Object), true);
    eq(t.vInstanceOf({}, A), false);

    class B { static [Symbol.hasInstance](x) { return x === 42; } }
    eq(t.vInstanceOf(42, B), true);
    eq(t.vInstanceOf(7, B), false);
});

// ── globals ──
await test("get_global (incl. globalThis)", () => {
    check(t.gGetStr("Date") === Date);
    check(t.gGetStr("globalThis") === globalThis);
});
await test("set_global", () => {
    t.gSetStr("__zapi_str", 6);
    eq(globalThis.__zapi_str, 6);
});

// ── calls ──
await test("call_function", () => eq(t.cCall((a, b) => a + b, 2, 3), 5));
await test("call_constructor", () => {
    const d = t.cCtor(Date, 0);
    check(d instanceof Date);
    eq(d.getTime(), 0);
});
await test("invoke (property key)", () => eq(t.cInvoke([1, 2, 3], "join", "-"), "1-2-3"));
await test("invoke_str", () => eq(t.cInvokeStr([1, 2, 3], "join", "-"), "1-2-3"));
await test("new_native_function", () => {
    const f = t.makeNativeFn();
    eq(typeof f, "function");
    eq(f(), 123);
});

// ── exceptions ──
await test("throw Type/Range", () => {
    let e1 = null, e2 = null;
    try { t.eThrowType(); } catch (e) { e1 = e; }
    try { t.eThrowRange(); } catch (e) { e2 = e; }
    check(e1 instanceof TypeError, "expected TypeError");
    check(e2 instanceof RangeError, "expected RangeError");
    check(typeof e1.stack === "string", "expected a stack");
});
await test("exception peek + take_exception", () => {
    const e = t.eTakePeek(() => { throw new RangeError("inner"); });
    check(e instanceof RangeError);
    eq(e.message, "inner");
    // The pending exception was consumed, so later calls still work.
    eq(t.nInt32(1), 1);
});
await test("throw_value raises an arbitrary value", () => {
    let caught = null;
    try { t.eThrowValue({ code: 42 }); } catch (e) { caught = e; }
    check(caught !== null && caught.code === 42, "expected the thrown object");
});

// ── GC: gc_set_trace_func / gc_mark ──
await test("gc_set_trace_func keeps a value alive across GC", async () => {
    t.gcTracePin({ marker: 67890 });
    t.gcForce();
    await settle();
    const p = t.gcTraced();
    eq(p.marker, 67890);
    t.gcTraceUnpin();
});

// ── class payload GC trace (gc_mark via ZqGcTrace) ──
await test("class payload trace keeps held JS values alive", async () => {
    // The tag object has no JS reference once the call returns; only the
    // Point payload's traced `tag` keeps it reachable.
    const p = new t.Point(1, 2, t.nObject());
    t.gcForce();
    await settle();
    eq(p.tag().kind, "demo");
});

// ── promises (new_promise_capability / spawn_promise / polling / waker) ──
await test("new_promise_capability + resolve", async () => {
    check(t.capResolve(7) instanceof Promise);
    eq(await t.capResolve(7), 7);
});
await test("new_promise_capability + reject", async () => {
    let caught = null;
    await t.capReject(new RangeError("cap")).then(
        () => { throw Error("expected rejection"); },
        (e) => { caught = e; },
    );
    check(caught instanceof RangeError);
    eq(caught.message, "cap");
});
await test("spawn_promise resolves", async () => {
    eq(await t.asyncResolve(41), 42);
});
await test("spawn_promise rejects with the thrown error", async () => {
    let caught = null;
    await t.asyncReject().then(
        () => { throw Error("expected rejection"); },
        (e) => { caught = e; },
    );
    check(caught instanceof RangeError);
    eq(caught.message, "async boom");
});

// ── async iterator (new_async_iterator) ──
await test("new_async_iterator", async () => {
    const got = [];
    for await (const v of t.asyncSeq(3)) {
        got.push(v);
    }
    eq(got.join(","), "0,1,2");
});

// ── class: register_class / new_instance / get_native_ptr / is_instance_of ──
await test("Point class", () => {
    const p = new t.Point(1, 2, "tagval");
    check(p instanceof t.Point);
    eq(p.sum(), 3);
    eq(p.tag(), "tagval");
    check(t.Point.isPoint(p));
    check(!t.Point.isPoint({}));
    check(t.isPointViaClass(p));
    check(!t.isPointViaClass({}));
});

// ── summary ──
console.log("");
if (fail === 0) {
    console.log("ALL PASS (" + pass + " tests)");
} else {
    console.log(fail + " FAILURES (" + pass + " passed): " + failed.join(", "));
    throw Error(fail + " test(s) failed");
}
