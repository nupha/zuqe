// SPDX-License-Identifier: Apache-2.0
//
// Write-barrier regressions for payloads and containers that can be promoted
// to the old generation and later receive a young GC value.
//
// Host: examples/zjs. Run with `--features gc_generations` (default features)
// so the debug build's `dbg_assert_no_stale_old_edges` validates every minor.
//
//   ./target/debug/examples/zjs tests/test_gc_barriers.mjs

import * as std from "std";

function check(cond, msg) {
    if (!cond) throw new Error(msg);
}

function spin() {
    // A major may be incremental: give it enough safepoints to finish so a
    // later minor request is not dropped behind a half-marked heap.
    for (let i = 0; i < 1024; i++) {
        if (i === -1) throw new Error("unreachable");
    }
}

function minor() {
    // A pending major wins over a minor request, so keep requesting until a
    // minor actually completes; otherwise a barrier test silently no-ops.
    const before = std.gcStats().minorCount;
    for (let i = 0; i < 16 && std.gcStats().minorCount === before; i++) {
        std.gcMinor();
        spin();
    }
    check(std.gcStats().minorCount > before, "minor did not run");
}

// ── async generator request queue ──────────────────────────────────────────
// A request queued while the generator is EXECUTING is only held by the
// generator payload; its resolving functions must be bound to the (possibly
// old) generator node.
{
    let release;
    const gate = new Promise((r) => { release = r; });
    async function* ag() {
        await gate;
    }
    const g = ag();
    minor();
    minor(); // promote the generator object

    const p1 = g.next(); // resumes, parks on `await`; req1 stays queued
    const p2 = g.next(); // EXECUTING: req2 lives only in the queue
    minor(); // old generator -> queued capability must survive

    release();
    const r1 = await p1;
    const r2 = await p2;
    check(r1.done === true && r2.done === true, "async generator queue lost a request");
}

// ── Iterator helper: flatMap inner iterator ────────────────────────────────
{
    const helper = [0].values().flatMap(() => [{ n: 1 }, { n: 2 }]);
    minor();
    minor(); // promote the helper object

    const a = helper.next(); // creates the inner iterator and keeps it
    minor(); // old helper -> inner iterator must survive
    const b = helper.next();
    minor();
    const c = helper.next();
    check(!a.done && !b.done && c.done, "iterator helper lost its inner iterator");
    check(a.value.n === 1 && b.value.n === 2, "iterator helper lost values");
}

// ── Iterator.concat: current source iterator ───────────────────────────────
{
    const c = Iterator.concat([1, 2], [3, 4]);
    minor();
    minor(); // promote the concat object

    const a = c.next(); // creates and caches the source iterator
    minor(); // old concat object -> cached iterator must survive
    const b = c.next();
    minor();
    const d = c.next();
    check(a.value === 1 && b.value === 2 && d.value === 3, "Iterator.concat lost its source iterator");
}

// ── Set operations: result built while/before keys arrive ──────────────────
// Integration coverage for the set-methods. The deterministic old-result /
// young-key check that fails without `upsert_bound` is the Rust unit test in
// `src/builtins/mapset/class_map.rs`.
{
    // `GetSetRecord` runs the `size` getter after the result Set exists, so a
    // minor there promotes the result without touching the keys yielded later.
    const otherWith = (count) => ({
        get size() {
            minor();
            return count;
        },
        has() { return false; },
        keys() {
            // A plain iterator: a generator would keep its suspended frame
            // rooted and mask the barrier by retaining every yielded key.
            let i = 0;
            return {
                next() {
                    return i < count
                        ? { done: false, value: { i: i++ } }
                        : { done: true };
                },
            };
        },
    });

    // union / symmetricDifference insert fresh keys after the result became old.
    std.gcPromote(1);
    try {
        for (const op of ["union", "symmetricDifference"]) {
            const result = new Set()[op](otherWith(8));
            minor();
            minor();
            check(result.size === 8, `Set.${op} lost keys after the result was promoted`);
        }

        // intersection / difference share the same bound write path; exercise
        // both sides with keys that stay reachable through `kept`.
        const kept = [{ k: 0 }, { k: 1 }, { k: 2 }];
        const base = new Set(kept);
        const other = {
            size: kept.length,
            has(k) { return base.has(k); },
            keys() { return kept.values(); },
        };
        const inter = base.intersection(other);
        check(inter.size === kept.length, "Set.intersection lost keys");

        const diff = base.difference(new Set());
        check(diff.size === kept.length, "Set.difference lost keys");
        minor();
        check(diff.size === kept.length, "Set.difference lost keys after a minor");
    } finally {
        std.gcPromote(2);
    }
}

// ── for-in: property-name list rebuilt late on an old iterator ─────────────
// Smoke coverage for the late `prop_enums` assignment. The atoms are usually
// kept alive by the source shape/scope, so this exercises the path but cannot
// reliably fail before the `bind_atom` call is present.
{
    let gen = 0;
    const proto = new Proxy({}, {
        ownKeys() {
            const out = [];
            for (let i = 0; i < 3; i++) {
                gen++;
                out.push("__wbt_" + gen);
            }
            return out;
        },
        getOwnPropertyDescriptor() {
            return { value: 1, enumerable: true, configurable: true };
        },
        getPrototypeOf() { return null; },
    });
    const child = [];
    child[0] = 1;
    child[1] = 1;
    Object.setPrototypeOf(child, proto);

    let seen = 0;
    for (const _k in child) {
        seen++;
        if (seen === 1) {
            minor();
            minor(); // promote the for-in iterator before the late name list
        }
        if (seen === 3) {
            minor(); // old iterator -> late property-name atoms must survive
        }
    }
    check(seen === 5, "for-in lost prototype-chain keys");
}

// ── closure variable: frame teardown into an old VarRef ────────────────────
// The captured `VarRef` node lives in the closure, the value only in the
// frame slot. Detaching at frame teardown copies the slot into the node, an
// old→young write when the node was already promoted.
{
    let closure = null;
    // `Array.prototype.map` is native, so the callback runs through
    // `BytecodeFunction::call`, whose teardown detaches the frame's VarRefs.
    [0].map(function () {
        let x;
        closure = () => x; // the closure allocates the VarRef for `x`
        minor(); // promote the closure object and its VarRef
        x = { v: 1 }; // young value, only in the frame slot
    });
    minor(); // old VarRef -> the detached value must survive
    check(closure().v === 1, "closure lost the value captured at frame teardown");
}

// ── generator suspension: frame snapshot into an old state node ────────────
// A suspended frame leaves the arena (it is no longer a root), so the values
// it carries are only reachable through the AsyncState payload from then on.
{
    function* gen() {
        const a = { v: 1 }; // young value in the frame
        yield a;
        yield a.v;
    }
    const g = gen();
    minor();
    minor(); // promote the generator's AsyncState node
    const r1 = g.next(); // suspends: young frame values -> old state node
    minor(); // the state is old; its snapshot values must survive
    const r2 = g.next();
    check(r1.value && r1.value.v === 1, "generator lost the yielded value");
    check(r2.value === 1, "generator lost its local after a minor");
}

// ── async function: the same frame snapshot on `await` ─────────────────────
{
    let release;
    const gate = new Promise((r) => {
        release = r;
    });
    async function work() {
        const a = { v: 2 };
        await gate;
        return a.v;
    }
    const p = work();
    minor();
    minor(); // promote the async state node while it is suspended on `gate`
    release();
    const v = await p;
    check(v === 2, "async function lost its local across the suspension");
}

std.puts("gc barriers: ok\n");
