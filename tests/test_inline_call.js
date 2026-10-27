// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: Apache-2.0
// VM-P7b regression: same-realm bytecode calls run inline in the dispatch
// loop (caller parked in the interpreter, no trampoline round trip). The
// observable contract that must survive the fast path:
//   - strict/sloppy `this` and strict-mode semantics per callee
//   - `arguments` (mapped and unmapped) and parameter aliasing
//   - closure captures, recursion and the stack-depth limit
//   - try/catch/finally, exceptions crossing inline frames
//   - constructors, derived classes and `super`
//   - tail-call reuse (true TCO) and method calls
var failures = 0;
function check(name, cond) {
    if (!cond) { failures++; console.log("FAIL: " + name); }
}

// 1. strict mode is per callee frame
function sloppyThis() { return this; }
function strictThis() { "use strict"; return this; }
check("sloppy this boxes", sloppyThis.call(1) instanceof Number);
check("strict this stays primitive", strictThis.call(1) === 1);
check("strict this undefined", strictThis() === undefined);

var strictDelete = false;
try {
    (function () { "use strict"; delete Object.prototype; })();
} catch (e) { strictDelete = e instanceof TypeError; }
check("strict delete throws inside inline callee", strictDelete === true);

// 2. arguments: mapped aliasing and unmapped strict arguments
function mapped(a, b) {
    arguments[0] = 10;
    var before = a;
    a = 20;
    return [before, arguments[0], b];
}
var m = mapped(1, 2);
check("mapped arguments alias parameter", m[0] === 10 && m[1] === 20 && m[2] === 2);

function unmapped(a) {
    "use strict";
    arguments[0] = 10;
    return a;
}
check("strict arguments do not alias", unmapped(1) === 1);

// 3. closure captures point into the live frame
function counter() {
    var n = 0;
    return function () { return ++n; };
}
var c1 = counter();
c1(); c1();
var c2 = counter();
check("captured variable shared per closure", c1() === 3 && c2() === 1);

function nestedCapture() {
    var a = 1;
    function mid() { return function () { return ++a; }; }
    var f = mid();
    return [f(), f()];
}
var nc = nestedCapture();
check("two-level capture", nc[0] === 2 && nc[1] === 3);

// 4. recursion and the depth limit
function sum(n) { return n === 0 ? 0 : n + sum(n - 1); }
check("recursion", sum(1000) === 500500);
function deep(n) { return deep(n + 1); }
var depthError = false;
try { deep(0); } catch (e) { depthError = e.name === "InternalError" && /stack/i.test(e.message); }
check("runaway recursion is a catchable stack error", depthError === true);

// 5. exceptions cross inline frames
function thrower() { throw new Error("boom"); }
function catchMiddle() {
    try { thrower(); return "no"; } catch (e) { return e.message; }
}
check("exception caught in inline caller", catchMiddle() === "boom");

var order = [];
function fin() {
    try {
        try { throw 1; } finally { order.push("inner"); }
    } finally { order.push("outer"); }
}
try { fin(); } catch (e) { order.push("caught:" + e); }
check("finally order across inline frames", order.join(",") === "inner,outer,caught:1");

function thrower2() { throw 7; }
function propagate() { return thrower2(); }
var propagated = 0;
try { propagate(); } catch (e) { propagated = e; }
check("uncaught exception reaches outer handler", propagated === 7);

// A GC object allocated in a Full-scope callee must survive the callee scope
// being torn down while the exception crosses back into the caller.
function throwAlloc() { var o = { tag: "kept" }; throw o; }
function catchAlloc() {
    try { throwAlloc(); return "no"; } catch (e) { return e.tag; }
}
check("thrown object survives callee scope teardown", catchAlloc() === "kept");

// 6. constructors, methods, derived classes
function Point(x, y) { this.x = x; this.y = y; }
Point.prototype.sum = function () { return this.x + this.y; };
var p = new Point(2, 3);
check("constructor + method", p.sum() === 5 && p instanceof Point);

function returnsObject() { this.x = 1; return { y: 2 }; }
var ro = new returnsObject();
check("constructor object return wins", ro.y === 2 && ro.x === undefined);

class Base {
    constructor(v) { this.v = v; }
    get() { return this.v; }
}
class Derived extends Base {
    constructor(v) { super(v + 1); }
    get() { return super.get() * 2; }
}
var d = new Derived(4);
check("derived class and super", d.get() === 10 && d instanceof Base);

// 7. tail calls reuse the frame (constant depth). Proper tail calls are only
// emitted for strict-mode code, as ES requires.
function tco(n) { "use strict"; return n === 0 ? "done" : tco(n - 1); }
check("tail recursion at constant depth", tco(200000) === "done");
var tcoObj = {
    m(n) { "use strict"; return n === 0 ? "done" : tcoObj.m(n - 1); },
};
check("method tail call at constant depth", tcoObj.m(200000) === "done");

// 8. method receiver and call/apply
var receiver = {
    v: 41,
    add(n) { return this.v + n; },
};
check("method receiver", receiver.add(1) === 42);
check("call with receiver", receiver.add.call({ v: 1 }, 2) === 3);
check("apply", receiver.add.apply({ v: 2 }, [3]) === 5);

// 9. callee reads the caller's integer/float result immediately
function id(x) { return x; }
check("int result", id(1) + id(2) === 3);
check("float result", id(1.5) + id(2.25) === 3.75);
check("string result", id("a") + id("b") === "ab");

if (failures) {
    console.log(failures + " failures");
} else {
    console.log("ALL PASS");
}
