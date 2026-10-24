// VM-B0 regression: deleting a property and adding it back.
//
// Spec (verified against node/v8): a deleted-then-re-added property is a NEW
// property, so it moves to the END of the own-property order:
//   o = {a:1, b:2}; delete o.a; o.a = 2;  =>  keys(o) == ["b", "a"]
// The stale shape def must not be revived in place. The same shape may also be
// shared with other objects whose slot is still live, so it can never be
// mutated to "restore" the property.
//
// Run: cargo run --example zjs -- tests/test_shape_delete_readd.js

var failures = 0;

function fail(label, msg) {
    failures++;
    console.log("FAIL " + label + ": " + msg);
}

function checkEq(label, actual, expected) {
    var a = JSON.stringify(actual);
    var e = JSON.stringify(expected);
    if (a !== e) fail(label, "got |" + a + "|, expected |" + e + "|");
}

function check(label, cond, msg) {
    if (!cond) fail(label, msg || "check failed");
}

function keysOf(o) {
    var out = [];
    for (var k in o) {
        if (Object.prototype.hasOwnProperty.call(o, k)) out.push(k);
    }
    return out;
}

// ── 1. basic order: same flags ─────────────────────────────────────────────
(function test_basic_order() {
    var o = { a: 1, b: 2 };
    delete o.a;
    o.a = 2;
    checkEq("basic/Object.keys", Object.keys(o), ["b", "a"]);
    checkEq("basic/for-in", keysOf(o), ["b", "a"]);
    checkEq("basic/JSON.stringify", JSON.stringify(o), '{"b":2,"a":2}');
    checkEq("basic/descriptor", Object.getOwnPropertyDescriptor(o, "a"), {
        value: 2, writable: true, enumerable: true, configurable: true
    });
    check("basic/value", o.a === 2);

    // Deleting from the middle keeps the tail order.
    var p = { x: 1, y: 2, z: 3 };
    delete p.y;
    p.y = 9;
    checkEq("middle/Object.keys", Object.keys(p), ["x", "z", "y"]);
    checkEq("middle/JSON.stringify", JSON.stringify(p), '{"x":1,"z":3,"y":9}');
}());

// ── 2. re-add with different flags / accessor ──────────────────────────────
(function test_flags() {
    var o = { a: 1, b: 2 };
    delete o.a;
    Object.defineProperty(o, "a", { value: 3, enumerable: true, writable: true, configurable: true });
    checkEq("flags/Object.keys", Object.keys(o), ["b", "a"]);
    checkEq("flags/descriptor", Object.getOwnPropertyDescriptor(o, "a"), {
        value: 3, writable: true, enumerable: true, configurable: true
    });

    // Re-add as a non-enumerable data property: order still appends, but it
    // must not show up in enumeration.
    var q = { a: 1, b: 2 };
    delete q.a;
    Object.defineProperty(q, "a", { value: 4, enumerable: false, writable: true, configurable: true });
    checkEq("nonenum/Object.keys", Object.keys(q), ["b"]);
    check("nonenum/value", q.a === 4);

    // Re-add as accessor.
    var r = { a: 1, b: 2 };
    delete r.a;
    Object.defineProperty(r, "a", { get: function () { return 42; }, enumerable: true, configurable: true });
    checkEq("accessor/Object.keys", Object.keys(r), ["b", "a"]);
    check("accessor/get", r.a === 42);
    check("accessor/gtag", Object.getOwnPropertyDescriptor(r, "a").get instanceof Function);

    // Delete again after re-add, then readd once more.
    delete r.a;
    r.a = 7;
    checkEq("accessor/readd/Object.keys", Object.keys(r), ["b", "a"]);
    check("accessor/readd/value", r.a === 7);
}());

// ── 3. shape shared between objects ────────────────────────────────────────
// A shape is deduplicated across objects. Deleting on one object only marks
// its own slot, so re-adding must build a new shape and must not disturb the
// other object's def/slot.
(function test_shared_shape() {
    var a = { p: 1, q: 2 };
    var b = { p: 3, q: 4 };
    delete a.p;
    a.p = 5;
    checkEq("shared/a keys", Object.keys(a), ["q", "p"]);
    checkEq("shared/b keys", Object.keys(b), ["p", "q"]);
    checkEq("shared/b values", [b.p, b.q], [3, 4]);
    checkEq("shared/a values", [a.q, a.p], [2, 5]);

    // b still owns p, a's delete must not have affected it.
    check("shared/b has p", Object.prototype.hasOwnProperty.call(b, "p"));
    b.p = 30;
    checkEq("shared/b after set", Object.keys(b), ["p", "q"]);
}());

// ── 4. loop: shape/slot must not grow without bound ────────────────────────
(function test_loop() {
    var o = { a: 1, b: 2 };
    for (var i = 0; i < 10000; i++) {
        delete o.a;
        o.a = i;
        if (i % 997 === 0) {
            checkEq("loop/keys/" + i, Object.keys(o), ["b", "a"]);
        }
    }
    checkEq("loop/final keys", Object.keys(o), ["b", "a"]);
    check("loop/final value", o.a === 9999);

    // Same but reverting to the original order: delete `a` then delete `b`,
    // re-add `b` then `a` (order must follow the re-add sequence).
    var o2 = { a: 1, b: 2 };
    for (var j = 0; j < 1000; j++) {
        delete o2.a;
        delete o2.b;
        o2.b = j;
        o2.a = j + 1;
    }
    checkEq("loop2/keys", Object.keys(o2), ["b", "a"]);
}());

// ── 5. Proxy defineProperty / deleteProperty paths ─────────────────────────
(function test_proxy() {
    var target = { a: 1, b: 2 };
    var ops = [];
    var p = new Proxy(target, {
        deleteProperty: function (t, k) {
            ops.push("delete:" + k);
            return Reflect.deleteProperty(t, k);
        },
        defineProperty: function (t, k, d) {
            ops.push("define:" + k);
            return Reflect.defineProperty(t, k, d);
        }
    });

    delete p.a;
    p.a = 2;
    checkEq("proxy/ops", ops, ["delete:a", "define:a"]);
    checkEq("proxy/keys", Object.keys(target), ["b", "a"]);
    checkEq("proxy/value", target.a, 2);

    // Via Reflect on the proxy.
    ops.length = 0;
    Reflect.deleteProperty(p, "a");
    check("proxy/reflect-delete", Reflect.defineProperty(p, "a", {
        value: 3, enumerable: true, writable: true, configurable: true
    }));
    checkEq("proxy/ops2", ops, ["delete:a", "define:a"]);
    checkEq("proxy/keys2", Object.keys(target), ["b", "a"]);
    checkEq("proxy/value2", target.a, 3);
}());

if (failures !== 0) throw Error("test_shape_delete_readd: " + failures + " failure(s)");
console.log("test_shape_delete_readd: ok");
