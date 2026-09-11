// Comprehensive regression test for all GC arena fixes
// Run: cargo run --example zjs --features debug_gc -- tests/test_all_fixes.js
// Or:  cargo run --release --example zjs -- tests/test_all_fixes.js

// ================ Bug 4: async generator yield value ================
async function test_bug4() {
    async function* gen() {
        yield { a: 1 };
        yield { b: 2 };
        yield { c: 3 };
    }
    var it = gen();
    var r1 = await it.next();
    if (r1.value.a !== 1) throw Error("Bug4: r1.a=" + r1.value.a);
    var r2 = await it.next({x:10});
    if (r2.value.b !== 2) throw Error("Bug4: r2.b=" + r2.value.b);
    var r3 = await it.next({y:20});
    if (r3.value.c !== 3) throw Error("Bug4: r3.c=" + r3.value.c);
    var r4 = await it.next();
    if (r4.done !== true) throw Error("Bug4: not done");
    print("Bug4 PASS");
}

// ================ Bug 4: async generator yield* ================
async function test_bug4_star() {
    async function* inner() { yield { x: 100 }; }
    async function* outer() { yield* inner(); }
    var it = outer();
    var r = await it.next();
    if (r.value.x !== 100) throw Error("yield*: x=" + r.value.x);
    print("Bug4_star PASS");
}

// ================ Bug 4: async function await ================
async function test_async_fn() {
    async function delay(x) { return x * 2; }
    var r = await delay(21);
    if (r !== 42) throw Error("async: r=" + r);
    print("AsyncFn PASS");
}

// ================ Bug 4: generator throw/return ================
async function test_throw_return() {
    async function* g() {
        try { yield 1; } catch(e) { yield "caught:" + e; }
    }
    var it = g();
    var r1 = await it.next();
    if (r1.value !== 1) throw Error("throw: r1=" + r1.value);
    var r2 = await it.throw("err");
    if (r2.value !== "caught:err") throw Error("throw: r2=" + r2.value);
    print("ThrowReturn PASS");
}

// ================ Bug 4: multi-yield with values ================
async function test_multi_yield() {
    async function* gen() {
        var a = yield 10;
        var b = yield 20;
        var c = yield 30;
        return a + b + c;
    }
    var it = gen();
    var r1 = await it.next();
    if (r1.value !== 10) throw Error("multi: r1=" + r1.value);
    var r2 = await it.next(1);
    if (r2.value !== 20) throw Error("multi: r2=" + r2.value);
    var r3 = await it.next(2);
    if (r3.value !== 30) throw Error("multi: r3=" + r3.value);
    var r4 = await it.next(3);
    if (r4.value !== 6 || r4.done !== true) throw Error("multi: r4=" + r4.value);
    print("MultiYield PASS");
}

// ================ Bug 5: Promise.allSettled ================
async function test_bug5() {
    var ps = [
        Promise.resolve(2).then(function(v) { return v + 1; }),
        Promise.reject(21).catch(function(v) { return v * 2; }),
        Promise.resolve('nope').then(function() { throw 'foo'; }),
        Promise.reject('yes').then(function() { throw 'nope'; }),
        Promise.resolve('here').finally(function() { return 'nope'; }),
        Promise.reject('here too').finally(function() { return 'nope'; }),
        Promise.resolve('nope').finally(function() { throw 'finally'; }),
        Promise.reject('nope').finally(function() { throw 'finally after rejected'; }),
        Promise.reject(1).then(function() { return 'nope'; }, function() { return 0; }),
    ];
    var settled = await Promise.allSettled(ps);
    if (settled.length !== 9) throw Error("Bug5: length=" + settled.length);
    var expected = [
        { status: 'fulfilled', value: 3 },
        { status: 'fulfilled', value: 42 },
        { status: 'rejected', reason: 'foo' },
        { status: 'rejected', reason: 'yes' },
        { status: 'fulfilled', value: 'here' },
        { status: 'rejected', reason: 'here too' },
        { status: 'rejected', reason: 'finally' },
        { status: 'rejected', reason: 'finally after rejected' },
        { status: 'fulfilled', value: 0 },
    ];
    for (var i = 0; i < 9; i++) {
        var s = settled[i];
        var e = expected[i];
        if (s.status !== e.status) throw Error("Bug5: " + i + " status");
        if (s.value !== undefined && s.value !== e.value) throw Error("Bug5: " + i + " value=" + s.value);
        if (s.reason !== undefined && s.reason !== e.reason) throw Error("Bug5: " + i + " reason=" + s.reason);
    }
    print("Bug5 PASS");
}

// ================ Promise.all (non allSettled) ================
async function test_promise_all() {
    var r = await Promise.all([Promise.resolve(1), Promise.resolve(2)]);
    if (r[0] !== 1 || r[1] !== 2) throw Error("all: bad");
    print("PromiseAll PASS");
}

// ================ Promise.race ================
async function test_promise_race() {
    var r = await Promise.race([Promise.resolve(42), Promise.resolve(0)]);
    if (r !== 42) throw Error("race: " + r);
    print("PromiseRace PASS");
}

// ================ Array.prototype.sort with objects ================
async function test_array_sort() {
    var arr = [{n:3}, {n:1}, {n:2}];
    arr.sort(function(a, b) { return a.n - b.n; });
    if (arr[0].n !== 1 || arr[1].n !== 2 || arr[2].n !== 3)
        throw Error("sort: bad order");
    print("ArraySort PASS");
}

// ================ Main ================
async function main() {
    var tests = [
        test_bug4, test_bug4_star, test_async_fn,
        test_throw_return, test_multi_yield,
        test_bug5, test_promise_all, test_promise_race,
        test_array_sort,
    ];
    for (var i = 0; i < tests.length; i++) {
        try {
            await tests[i]();
        } catch(e) {
            print("FAIL: " + e.message);
            return;
        }
    }
    print("ALL " + tests.length + " PASS");
}

main();
