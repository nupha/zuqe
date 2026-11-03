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

print("ok");
