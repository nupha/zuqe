// Comprehensive test for the zapi_event extension: multi-event / multi-handler
// dispatch with `on` / `once` / `off` / `removeAllListeners` / `emit`
// (Node-style, `emit` returns a boolean), plus static methods/accessors and
// magic dispatch.
//
// Run after building the extension:
//   cargo build --release --manifest-path examples/zapi_event/Cargo.toml
//   cp examples/zapi_event/target/release/libzapi_event.dylib tests/zapi/
//   target/release/examples/zjs --module tests/zapi/test_zapi_event.mjs

import { EventEmitter } from "./zapi_event.dylib";

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

// ── constructor ──
test("new EventEmitter() creates an instance", () => {
    const ee = new EventEmitter();
    eq(typeof ee, "object");
    eq(EventEmitter.isEmitter(ee), true);
});

test("EventEmitter() without new throws", () => {
    let threw = false;
    try {
        EventEmitter();
    } catch (e) {
        threw = true;
    }
    eq(threw, true);
});

// ── instance methods live on the prototype ──
test("instance methods are on the prototype", () => {
    eq(typeof EventEmitter.prototype.on, "function");
    eq(typeof EventEmitter.prototype.once, "function");
    eq(typeof EventEmitter.prototype.emit, "function");
    eq(typeof EventEmitter.prototype.off, "function");
    eq(typeof EventEmitter.prototype.removeAllListeners, "function");
});

// ── multiple handlers, one event, in registration order ──
test("multiple handlers per event, called in order", () => {
    const ee = new EventEmitter();
    const calls = [];
    ee.on("ping", () => calls.push("a"));
    ee.on("ping", () => calls.push("b"));
    ee.on("ping", () => calls.push("c"));
    eq(ee.emit("ping"), true);
    eq(calls.join(","), "a,b,c");
    eq(ee.emit("ping"), true);
    eq(calls.join(","), "a,b,c,a,b,c");
});

test("handlers receive event name + payload", () => {
    const ee = new EventEmitter();
    let got = null;
    ee.on("greet", (event, ...payload) => {
        got = [event, payload];
    });
    ee.emit("greet", "hello", 42);
    eq(got[0], "greet");
    eq(got[1].join(","), "hello,42");
});

// ── multiple events are independent ──
test("multiple events dispatch independently", () => {
    const ee = new EventEmitter();
    let pings = 0;
    let pongs = 0;
    ee.on("ping", () => pings++);
    ee.on("pong", () => pongs++);
    ee.emit("ping");
    ee.emit("ping");
    ee.emit("pong");
    eq(pings, 2);
    eq(pongs, 1);
});

test("emit returns false when no listener", () => {
    const ee = new EventEmitter();
    eq(ee.emit("nobody"), false);
});

// ── once: fires exactly one time ──
test("once fires exactly once then is removed", () => {
    const ee = new EventEmitter();
    let count = 0;
    ee.once("tick", () => count++);
    ee.emit("tick");
    ee.emit("tick");
    ee.emit("tick");
    eq(count, 1);
});

test("once and on mix independently", () => {
    const ee = new EventEmitter();
    let once = 0;
    let on = 0;
    ee.once("evt", () => once++);
    ee.on("evt", () => on++);
    ee.emit("evt");
    ee.emit("evt");
    eq(once, 1);
    eq(on, 2);
});

// ── off / removeAllListeners ──
test("off removes a specific (event, handler)", () => {
    const ee = new EventEmitter();
    let a = 0;
    let b = 0;
    const fa = () => a++;
    const fb = () => b++;
    ee.on("x", fa);
    ee.on("x", fb);
    ee.off("x", fa);
    ee.emit("x");
    eq(a, 0);
    eq(b, 1);
});

test("removeAllListeners clears an event", () => {
    const ee = new EventEmitter();
    let count = 0;
    ee.on("x", () => count++);
    ee.on("x", () => count++);
    ee.removeAllListeners("x");
    eq(ee.emit("x"), false);
    eq(count, 0);
});

// ── magic dispatch: one instance body, two JS names ──
test("magic dispatch ee.fire / ee.spark", () => {
    const ee = new EventEmitter();
    eq(ee.fire(), 1);
    eq(ee.spark(), 2);
});

// ── static methods live on the constructor ──
test("static method is an own property of the constructor", () => {
    eq(EventEmitter.hasOwnProperty("isEmitter"), true);
});

test("EventEmitter.isEmitter distinguishes instances", () => {
    const ee = new EventEmitter();
    eq(EventEmitter.isEmitter(ee), true);
    eq(EventEmitter.isEmitter({}), false);
    eq(EventEmitter.isEmitter(null), false);
});

// ── static getter/setter accessors ──
test("privateMode get/set accessor", () => {
    eq(EventEmitter.privateMode, undefined);
    EventEmitter.privateMode = true;
    eq(EventEmitter.privateMode, true);
    EventEmitter.privateMode = false;
    eq(EventEmitter.privateMode, false);
});

test("kind getter-only accessor", () => {
    eq(EventEmitter.kind, "native");
});

// ── summary ──
if (fail === 0) {
    console.log("ALL PASS (" + pass + " tests)");
} else {
    console.log(fail + " FAILURES (" + pass + " passed, " + fail + " failed)");
}
