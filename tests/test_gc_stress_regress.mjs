// SPDX-License-Identifier: Apache-2.0
//
// Regression for the defects §4.6 of memo/gc_bind_guard.md recorded after P3
// landed. They only show up when a minor runs after (almost) every allocation,
// so run it with stress:
//
//   ./target/debug/examples/zjs --gc-stress tests/test_gc_stress_regress.mjs
//
// Without the flag the script still passes — it then proves nothing, which is
// exactly why these findings needed `--gc-stress`.
//
// (§4.6 finding 3 needs a second realm, so its regression is the test262 case
// `harness/asyncHelpers-throwsAsync-same-realm.js`, run the same way.)

import * as std from "std";

// Finding 1: a dying object's finalize upgrades a GcWeak to another object of
// the same white list whose payload has already been freed → double free.
// Fixed by clearing the white list's weak slots before any payload drop
// (gc-lite `sweep_unlink` / `minor_sweep_unlink`).
{
    const key = {};
    const weak = new WeakMap(), map = new Map(), set = new Set();
    std.gcMinor();               // promote the key
    weak.set(key, {});
    map.set(key, {});
    set.add({});                 // key / weak / map / set all die here
    for (let i = 0; i < 300; i++) { const k = {}; std.gcMinor(); }
}

// Finding 2: TypedArray::create_from collected the iterator's results into a
// C-stack Vec the GC never traces, so the next minor freed a BigInt before
// ToBigInt read it (dtype assert at `as_bigint_unchecked`).
{
    const it = (function* () { yield 7n; yield 42n; })();
    const ta = new BigInt64Array(it);
    if (ta.length !== 2 || ta[0] !== 7n || ta[1] !== 42n) {
        throw new Error(`BigInt64Array from generator: len=${ta.length} [0]=${ta[0]}`);
    }
}

// Finding 4: `op_binary_logic` popped both operands off the bytecode stack
// before ToPrimitive ran user code, so the BigInt produced by `valueOf` had no
// root across the GC safepoint (dtype assert / bad node).
{
    const a = { valueOf: function () { return 2n; } } << 1n;
    const b = 1n << { toString: function () { return 2n; } };
    const c = { [Symbol.toPrimitive]: function () { return 3n; } } | 4n;
    if (a !== 4n || b !== 4n || c !== 7n) {
        throw new Error(`bigint operators: ${a} ${b} ${c}`);
    }
}

// Same family as finding 4: every site that pops operands off the bytecode
// stack before running user code must keep the slots populated. These cover
// the audited sites (delete, instanceof, private-in, for-in start and its
// prototype walk, class definition, iterator close while unwinding).
{
    // delete: the key's toString runs user code between pop and use
    const key = () => ({ toString: function () { let z = {}; for (let i = 0; i < 32; i++) z = { i, z }; return "a"; } });
    const mk = () => ({ a: 1 });
    if (!delete mk()[key()]) throw new Error("delete");

    // instanceof: Symbol.hasInstance runs user code
    function Ctor() {}
    Object.defineProperty(Ctor, Symbol.hasInstance, { value: function () {
        let z = {}; for (let i = 0; i < 32; i++) z = { i, z }; return true; } });
    if (!({} instanceof Ctor)) throw new Error("instanceof");

    // private-in
    class Has { #x = 1; static test(o) { return #x in o; } }
    if (!Has.test(new Has())) throw new Error("private in");

    // for-in over a proxy: ownKeys / getOwnPropertyDescriptor / getPrototypeOf
    const proxy = new Proxy({ a: 1 }, {
        ownKeys() { let z = {}; for (let i = 0; i < 32; i++) z = { i, z }; return ["a"]; },
        getOwnPropertyDescriptor() { let z = {}; for (let i = 0; i < 32; i++) z = { i, z };
            return { value: 1, enumerable: true, configurable: true }; },
        getPrototypeOf() { let z = {}; return null; },
    });
    const names = [];
    for (const n in proxy) names.push(n);
    if (names.join() !== "a") throw new Error("for-in: " + names);

    // class definition: superclass `prototype` read through a proxy trap
    const proto = { m() {} };
    const parent = new Proxy(function () {}, {
        get(t, p, r) { let z = {}; for (let i = 0; i < 32; i++) z = { i, z };
            return p === "prototype" ? proto : Reflect.get(t, p, r); },
    });
    class Sub extends parent {}
    if (typeof Sub.prototype.m !== "function") throw new Error("class extends");

    // iterator close while unwinding: the iterator's return() runs user code
    let closed = 0;
    const iterable = { [Symbol.iterator]() { let i = 0; return {
        next() { return { value: i++, done: i > 5 }; },
        return() { let z = {}; for (let j = 0; j < 32; j++) z = { j, z }; closed++; return { done: true }; } }; } };
    try {
        for (const v of iterable) { if (v === 1) throw new Error("stop"); }
    } catch (e) {
        if (e.message !== "stop") throw e;
    }
    if (closed !== 1) throw new Error("iterator close: " + closed);
}

print("ok");
