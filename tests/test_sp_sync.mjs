// SPDX-License-Identifier: Apache-2.0
//
// VM-P4 regression: the interpreter keeps `sp` in a register and writes
// `cur_sp_offset` back only at the points that read it (GC safepoint, unwind
// scan, call boundaries, JIT entry). A missing sync frees live temporaries
// that sit on the operand stack above the stale offset.
//
// Host: `examples/zjs`. `std.gc()` only *requests* a collection; the safepoint
// after the next opcode runs it, which is exactly the window under test.
//
// The values here are inline literals, not locals: after allocation they are
// protected only by the operand-stack trace (`VmStack::trace` scans up to
// `cur_sp_offset`), so a stale offset collects them.

import * as std from "std";

function assert(cond, msg) {
  if (!cond) throw new Error("assertion failed: " + msg);
}

// 1. Collection at an opcode boundary while earlier array elements are still
//    on the operand stack. `{n: round}` is a temporary, never a local.
for (let round = 0; round < 50; round++) {
  const arr = [{ n: round }, (std.gc(), 0), { n: round + 1 }];
  assert(
    arr[0].n === round && arr[2].n === round + 1,
    "operand-stack temporary corrupted at round " + round,
  );
}

// 2. Collection inside a native callee (`Array.prototype.map` re-enters JS).
//    `{n: 40}` is a pending argument on the caller's operand stack while the
//    native frame runs, and the callback requests the collection.
function consume(a, b) {
  return a.n + b;
}
for (let round = 0; round < 50; round++) {
  const r = consume({ n: 40 }, [1, 2, 3].map(function (v) {
    std.gc();
    return v;
  }).length);
  assert(r === 43, "native-call argument corrupted at round " + round);
}

// 3. Many live temporaries above the saved offset.
function many(...vals) {
  return vals;
}
for (let round = 0; round < 50; round++) {
  const got = many(
    { i: 1 },
    { i: 2 },
    { i: 3 },
    { i: 4 },
    (std.gc(), { i: 5 }),
  );
  assert(got.length === 5, "arity at round " + round);
  for (let i = 0; i < 5; i++) {
    assert(got[i].i === i + 1, "deep temporary " + i + " at round " + round);
  }
}

// 4. Safepoint triggered by the allocator itself (no explicit `std.gc()`): the
//    collection runs right after the allocating opcode, while earlier elements
//    are still on the operand stack above the last sync.
let bad = 0;
for (let i = 0; i < 200000; i++) {
  const arr = [{ n: i }, { n: i + 1 }, { n: i + 2 }];
  if (arr[0].n !== i || arr[1].n !== i + 1 || arr[2].n !== i + 2) bad++;
}
assert(bad === 0, "allocation-triggered safepoint corrupted " + bad + " arrays");

// 5. Callouts that re-enter JS (getter, valueOf) while pending operands sit on
//    the caller's operand stack: the nested frame's safepoint must not lose
//    the caller's live values.
const withGetter = {
  get g() {
    std.gc();
    return 1;
  },
};
function pick(obj, v) {
  return obj.n + v;
}
for (let i = 0; i < 200; i++) {
  assert(pick({ n: i }, withGetter.g) === i + 1, "getter callout " + i);
}

const withValueOf = {
  valueOf() {
    std.gc();
    return 7;
  },
};
for (let i = 0; i < 200; i++) {
  const arr = [{ n: i }, withValueOf + 1];
  assert(arr[0].n === i && arr[1] === 8, "valueOf callout " + i);
}

console.log("test_sp_sync: passed");
