// Bug 5: Promise.allSettled use-after-free with debug_gc
// Run: cargo run --example zjs --features debug_gc -- tests/test_promise_allSettled_bug.js
// Expected: print "done" without panic
// Actual (bug): [O.o] bad node at gc-lite-v2/src/node.rs:667
//
// NOTE: Reproduction is intermittent. The original crash was observed
// via the test262 runner (which provides async harness + $DONE).
// Standalone reproduction depends on arena GC timing (64KB arena)
// and may not trigger on every run. If the test passes, try running
// under the test262 runner or increase GC pressure:
//   cargo run --example test262 -- built-ins/Promise/allSettled/resolved-then-catch-finally.js

// Build 9 promises with various .then/.catch/.finally chains
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

Promise.allSettled(ps).then(function(settled) {
    if (settled.length !== 9) { throw new Error("bad length"); }
    for (var i = 0; i < settled.length; i++) {
        var s = settled[i];
        if (s.status !== 'fulfilled' && s.status !== 'rejected') {
            throw new Error("bad status at " + i);
        }
    }
    print("done");
});
