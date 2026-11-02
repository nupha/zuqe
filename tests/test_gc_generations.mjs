// SPDX-License-Identifier: Apache-2.0
//
// Phase 2 generational-GC stress: build an old object, write a young value into
// it through every supported write path, run a minor, then force a major, and
// verify the value is still there.
//
// Host: examples/zjs. Run with `--features gc_generations` to exercise minors;
// without the feature `std.gcMinor()` is a no-op and the same script still
// passes (the values simply stay reachable through the roots).
//
//   ./target/release/examples/zjs tests/test_gc_generations.mjs

import * as std from "std";

function check(cond, msg) {
    if (!cond) throw new Error(msg);
}

// The interpreter checks the GC request at every opcode; a short loop lets the
// pending collection run without leaving this function.
function spin() {
    for (let i = 0; i < 32; i++) {
        if (i === -1) throw new Error("unreachable");
    }
}

// With the feature off `std.gcMinor()` is a no-op; the script still passes.
function minor() {
    std.gcMinor();
    spin();
}

function major() {
    std.gc();
    spin();
}

function roundTrip(name, read) {
    minor();
    const before = read();
    minor();
    check(read() === before, name + ": value lost after minor");
    major();
    check(read() === before, name + ": value lost after major");
}

// ── property slot ──────────────────────────────────────────────────────────
{
    const obj = { x: 1 };
    minor(); // promote obj to the old generation
    const value = { tag: "property" };
    obj.x = value;
    roundTrip("property", () => obj.x);
}

// ── array element ──────────────────────────────────────────────────────────
{
    const arr = [0, 1, 2];
    minor();
    const value = { tag: "array" };
    arr[1] = value;
    roundTrip("array", () => arr[1]);
}

// ── Map / Set / WeakMap ────────────────────────────────────────────────────
{
    const key = { tag: "key" };
    const map = new Map();
    const set = new Set();
    const weak = new WeakMap();
    minor();

    const mapValue = { tag: "map-value" };
    const setValue = { tag: "set-value" };
    const weakValue = { tag: "weakmap-value" };
    map.set(key, mapValue);
    set.add(setValue);
    weak.set(key, weakValue);

    roundTrip("map", () => map.get(key));
    roundTrip("set", () => set.has(setValue));
    roundTrip("weakmap", () => weak.get(key));
}

// ── closure captured variable (VarRef::set_value) ──────────────────────────
{
    let captured = 0;
    const setter = (v) => { captured = v; };
    const getter = () => captured;
    minor(); // promote the closure and its VarRef

    const value = { tag: "closure-set" };
    setter(value);
    roundTrip("closure-set", () => getter());
}

// ── closure frame close (VarRef::detach) ───────────────────────────────────
{
    function makeGetter() {
        let captured = 0;
        const getter = () => captured;
        minor(); // promote the VarRef while its frame is still open
        captured = { tag: "closure-detach" }; // young value in the frame slot
        return getter; // closing the frame detaches the old VarRef
    }

    const getter = makeGetter();
    roundTrip("closure-detach", () => getter());
}

// ── Promise executor binding data (native function payload) ────────────────
{
    let resolve;
    const promise = new Promise((res) => { resolve = res; });
    minor();
    const value = { tag: "promise" };
    resolve(value);
    minor();
    major();
    check(typeof promise.then === "function", "promise lost its prototype");
}

// ── JIT call feedback ──────────────────────────────────────────────────────
{
    const makeCallee = () => (() => 7);
    const caller = (fn) => fn();
    let callee = makeCallee();
    for (let i = 0; i < 20000; i++) caller(callee); // record the callee site
    minor();
    callee = null; // the caller's feedback is now the only edge to the callee
    minor();
    major();
    check(caller(() => 7) === 7, "call feedback path broke");
}

// ── FinalizationRegistry registration ──────────────────────────────────────
{
    const registry = new FinalizationRegistry(() => { });
    minor();
    (() => {
        const target = {};
        registry.register(target, { tag: "held" });
    })();
    minor();
    major();
    check(typeof registry.unregister === "function", "registry lost");
}

// ── promotion count 0/1/2 ──────────────────────────────────────────────────
if (std.gcStats().generations) {
    std.gcPromote(0);
    // Sample AFTER the switch: with `--gc-stress` a minor can land between a
    // sample and the call that disables minors, which would report a minor the
    // test never allowed to run in the first place.
    const before = std.gcStats().minorCount;

    for (let i = 0; i < 20000; i++) ({ a: i, b: { c: i }, d: [i, i + 1] });
    spin();
    check(
        std.gcStats().minorCount === before,
        "gcPromote(0) must not run minors",
    );
    std.gcMinor();
    spin();
    check(
        std.gcStats().minorCount === before,
        "gcMinor() must be a no-op at gcPromote(0)",
    );

    std.gcPromote(2);
    std.gcMinor();
    spin();
    check(
        std.gcStats().minorCount > before,
        "minors work again after gcPromote(2)",
    );

    let rejected = false;
    try {
        std.gcPromote(9);
    } catch (e) {
        rejected = e instanceof RangeError;
    }
    check(rejected, "gcPromote(9) must throw RangeError");
}

// Automatic minor path: churn enough young objects that the young budget is
// exceeded; the count is only meaningful with the feature compiled in, so it is
// only checked for monotonicity here.
{
    const before = std.gcStats().minorCount;
    for (let i = 0; i < 20000; i++) {
        ({ a: i, b: { c: i }, d: [i, i + 1] });
    }
    spin();
    check(
        std.gcStats().minorCount >= before,
        "minor counter went backwards",
    );
}

std.puts("gc generations: ok\n");
