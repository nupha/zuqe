// SPDX-License-Identifier: Apache-2.0
// VM-P15 attribution probe: exception-path kernels, one per process.
//
//   zjs tests/bench_exceptions.mjs <kernel>
//
// Kernels isolate the pieces of bench_jit_hooks.mjs `exceptions-sync`:
//  - make-only:           new TypeError(...) without throwing (object+backtrace)
//  - native-throw:        null property access raises a TypeError internally
//  - throw-catch-finally: the `throw i` / catch / finally loop (1 throw per 16)
//  - type-error:          only the `table[...].w` TypeError branch (1 per 64)
//  - try-finally:         same shape as throw-catch-finally, no throw/typeerror
//  - catch-marker:        try/catch with no throw
//  - combined:            exceptions-sync as-is
import * as os from "os";

function median(a) {
    a = a.slice().sort((x, y) => x - y);
    return a[a.length >> 1];
}

function bench(name, fn) {
    let r;
    for (let i = 0; i < 2; i++) r = fn();
    const samples = [];
    for (let i = 0; i < 7; i++) {
        const t0 = os.now();
        r = fn();
        samples.push(os.now() - t0);
    }
    console.log(`${name} ${median(samples).toFixed(3)} result=${String(r)}`);
}

function throw_catch_finally(iterations, seed) {
    var caught = 0; var total = seed | 0; var finals = 0;
    for (var i = 0; i < iterations; i++) {
        try { if ((i & 15) === 0) throw i; total = (total + i) | 0; }
        catch (value) { caught += value; } finally { finals++; }
    }
    return caught + ":" + total + ":" + finals;
}

function type_error(iterations, seed) {
    var table = [{ w: 1 }, { w: 2 }, null, { w: 3 }];
    var total = seed | 0; var missing = 0;
    for (var i = 0; i < iterations; i++) {
        try { total = (total + table[i & 3].w) | 0; } catch (error) { missing++; }
    }
    return total + ":" + missing;
}

// Error-construction probe: same message, no throw. `new TypeError` builds a
// stack eagerly, so this isolates object + message + backtrace cost.
function make_only(iterations) {
    var e;
    for (var i = 0; i < iterations; i++) e = new TypeError("cannot read property of null");
    return e.message;
}

// Native-thrown error probe: the property access raises TypeError internally.
function native_throw(iterations) {
    var k = 0;
    for (var i = 0; i < iterations; i++) {
        try { var x = null; x.foo; } catch (e) { k++; }
    }
    return k;
}

function try_finally(iterations, seed) {
    var total = seed | 0; var finals = 0;
    for (var i = 0; i < iterations; i++) {
        try { total = (total + i) | 0; } finally { finals++; }
    }
    return total + ":" + finals;
}

function catch_marker(iterations, seed) {
    var total = seed | 0; var caught = 0;
    for (var i = 0; i < iterations; i++) {
        try { total = (total + i) | 0; } catch (value) { caught += value; }
    }
    return total + ":" + caught;
}

function combined(iterations, seed) {
    var table = [{ w: 1 }, { w: 2 }, null, { w: 3 }];
    var caught = 0; var total = seed | 0; var finals = 0; var missing = 0;
    for (var i = 0; i < iterations; i++) {
        try { if ((i & 15) === 0) throw i; total = (total + i) | 0; }
        catch (value) { caught += value; } finally { finals++; }
        if ((i & 63) === 1) {
            try { total = (total + table[(i >> 6) & 3].w) | 0; } catch (error) { missing++; }
        }
    }
    return caught + ":" + total + ":" + finals + ":" + missing;
}

const KERNELS = {
    "make-only": () => make_only(60000),
    "native-throw": () => native_throw(60000),
    "throw-catch-finally": () => throw_catch_finally(300000, 1),
    "type-error": () => type_error(60000, 1),
    "try-finally": () => try_finally(300000, 1),
    "catch-marker": () => catch_marker(300000, 1),
    "combined": () => combined(300000, 1),
};

const name = scriptArgs[1];
const fn = KERNELS[name];
if (!fn) {
    console.log("usage: zjs tests/bench_exceptions.mjs <" + Object.keys(KERNELS).join("|") + ">");
} else {
    bench(name, fn);
}
