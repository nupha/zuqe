// VM-B4 regression: `with` binding lookups must be fail-closed.
//
// A Proxy `has`/`get` trap that throws is the only script-visible way to inject
// an error into the `with` binding path. The contract under test: the *original*
// error must propagate. Fail-open shows up as (a) no exception at all, or
// (b) a follow-up TypeError caused by the swallowed error degrading to
// `undefined` / a bogus property key.
//
// Run: cargo run --example zjs -- tests/test_with_proxy_exception.js

var failures = 0;

function fail(label, msg) {
    failures++;
    console.log("FAIL " + label + ": " + msg);
}

function checkThrows(label, fn, expected) {
    var got = null;
    try {
        fn();
    } catch (e) {
        got = e === null || e === undefined ? String(e)
            : ((e.name ? e.name + ": " : "") +
               (e.message !== undefined ? e.message : String(e)));
    }
    if (got === null) {
        fail(label, "no exception (error swallowed)");
    } else if (got.indexOf(expected) < 0) {
        fail(label, "expected |" + expected + "|, got |" + got + "|");
    }
}

// ── WITH_GET_VAR: read through `with`, `has` trap throws ───────────────────
checkThrows("with-get-var/has", function () {
    var p = new Proxy({ x: 1 }, {
        has: function () { throw Error("has boom"); }
    });
    with (p) { x; }
}, "has boom");

// ── WITH_GET_VAR: read through `with`, `get` trap throws ───────────────────
checkThrows("with-get-var/get", function () {
    var p = new Proxy({ x: 1 }, {
        has: function () { return true; },
        get: function (t, k) {
            if (typeof k === "symbol") return undefined; // @@unscopables probe
            throw Error("get boom");
        }
    });
    with (p) { x; }
}, "get boom");

// ── WITH_GET_VAR: the post-@@unscopables HasProperty re-check throws.
checkThrows("with-get-var/recheck", function () {
    var n = 0;
    var p = new Proxy({ x: 1 }, {
        has: function () {
            n++;
            if (n === 2) throw Error("has boom");
            return true;
        }
    });
    with (p) { x; }
    throw Error("no exception after " + n + " HasProperty call(s)");
}, "has boom");

// ── WITH_PUT_VAR: `for (var x = ... in ...)` emits a with-scope put; `has`
// throws on the SetMutableBinding re-check (the 1st call gates the body).
checkThrows("with-put-var/has", function () {
    var n = 0;
    var p = new Proxy({ x: 1 }, {
        has: function () {
            n++;
            if (n === 2) throw Error("has boom");
            return true;
        }
    });
    with (p) { for (var x = 0 in { a: 1 }) { } }
    throw Error("no exception after " + n + " HasProperty call(s)");
}, "has boom");

// ── WITH_MAKE_REF → PUT_REF_VALUE: plain assignment through `with` does
// HasProperty again inside PutValue, and that one must not be swallowed
// (non-strict mode used to treat any Err as "property absent").
checkThrows("put-ref-value/has", function () {
    var n = 0;
    var p = new Proxy({ x: 1 }, {
        has: function () {
            n++;
            if (n === 2) throw Error("has boom");
            return true;
        }
    });
    with (p) { x = 2; }
    throw Error("no exception after " + n + " HasProperty call(s)");
}, "has boom");

// ── WITH_GET_REF: identifier call needs a reference; `get` trap throws.
// Fail-open turns this into `undefined()` → a bogus TypeError.
checkThrows("with-get-ref/get", function () {
    var p = new Proxy({ f: function () {} }, {
        has: function () { return true; },
        get: function (t, k) {
            if (typeof k === "symbol") return undefined; // @@unscopables probe
            throw Error("get boom");
        }
    });
    with (p) { f(); }
}, "get boom");

// ── Control: a well-behaved proxy still resolves reads, writes, compound
// assignments and calls through `with` (this exercises MAKE_REF/GET_REF).
// A `with` binding's reference base is the binding object itself, so a plain
// `f()` call must see the proxy as `this`.
(function () {
    var target = {
        v: 7,
        f: function () { return this === p; }
    };
    var p = new Proxy(target, {});
    with (p) {
        if (v !== 7) throw Error("with read through proxy");
        if (!f()) throw Error("with call lost its binding object as this");
        v = 9;
        if (target.v !== 9) throw Error("with write through proxy");
        v += 1;
        if (target.v !== 10) throw Error("with compound assign through proxy");
    }
}());

if (failures !== 0) throw Error("test_with_proxy_exception: " + failures + " failure(s)");
console.log("test_with_proxy_exception: ok");
