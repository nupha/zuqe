// SPDX-License-Identifier: Apache-2.0
//
// GC regression: constructor instances must not accumulate in the caller's GC
// scope while the constructor body runs (VM-B3, memo/vm_todo.md §0), in every
// call path that reaches `do_call_internal` — not only the inline
// `CALL_CONSTRUCTOR` fast path used by `new C(i)`.
//
// ── Environment ──────────────────────────────────────────────────────────
// - Host: `examples/zjs`. The test needs `std.gcMemory()`, a zjs-only host
//   extension backed by `RuntimeImpl::gc_memory_used()`; it is NOT part of the
//   engine's JS API, so this script does not run on other hosts.
// - Build: release (`cargo build --release --example zjs`). Debug builds are
//   valid but ~10x slower; the bound is unaffected. Debug builds additionally
//   exercise the native-caller UAF canary (`Promise.all.call(UserCtor, ...)`).
// - Run: `./target/release/examples/zjs tests/test_gc_ctor_scope.mjs`
// - Exit code: 0 = pass; 1 = failed assertion (the Error is not caught).
//
// ── What is measured ─────────────────────────────────────────────────────
// `std.gcMemory()` returns the bytes currently accounted live by the GC heap.
// Sampling it *inside* the allocation loop is the point: once the loop's
// function returns, its scope is popped and the leak drains, so a post-hoc
// measurement would miss the bug.
//
// Each retention phase must stay within `MEM_LIMIT` of its own baseline. A
// release run after the fix measures ~34 KB per phase; before it, the
// bound/proxy phases grew ~20 MB per 200k iterations (one retained instance
// each). The native-caller Promise phase is a UAF canary, not a retention
// bound (see below).
//
// ── Why a baseline-relative bound ────────────────────────────────────────
// `peak - baseline` isolates the loop's own retention from the runtime's
// builtin baseline (which varies with enabled features). Before the fix the
// growth scaled with the iteration count (~62 MB for 400k instances); after
// the fix it stays around the automatic-GC threshold (tens of KB above the
// baseline). The 4 MiB bound is two orders of magnitude above the fixed value
// and one order below the buggy one.
//
// ── Method ───────────────────────────────────────────────────────────────
// 1. Force a collection and read the resting live set as `baseline`.
// 2. Run each allocation shape (inline ctor, bound ctor, proxy ctor, object
//    literal, then a generator-frame bound ctor), sampling every 16k
//    iterations.
// 3. Run `Promise.all.call(UserCtor, ...)`: the capability object is created
//    inside a native builtin, which holds it in a Rust local while running
//    re-entrant JS. Its LOCAL protection must survive that (debug builds
//    assert it in `js_promise_all`); the loop also checks the result.
// 4. Assert `peak - baseline <= 4 MiB` for every phase.
import * as std from "std";

const N = 200000;
const N_NATIVE = 20000;
const MEM_LIMIT = 4 * 1024 * 1024;

function assert(cond, msg) {
  if (!cond) throw new Error("assertion failed: " + msg);
}

function C(i) {
  this.a = i;
  this.b = i + 1;
  this.c = i + 2;
}

const B = C.bind(null);
const P = new Proxy(C, {});

let peak = 0;

function sample() {
  const m = std.gcMemory();
  if (m > peak) peak = m;
}

// `std.gc()` only *requests* a collection; the safepoint after the next opcode
// actually runs it. Spin a few no-op iterations so the reading is post-sweep.
function restingMemory() {
  std.gc();
  for (let i = 0; i < 64; i++) {}
  return std.gcMemory();
}

function phase(label, fn) {
  const baseline = restingMemory();
  peak = baseline;
  fn();
  const growth = peak - baseline;
  console.log(
    label + ": baseline " + baseline + " peak " + peak + " growth " + growth,
  );
  assert(
    growth <= MEM_LIMIT,
    label +
      " live GC memory grew by " +
      growth +
      " bytes (baseline " +
      baseline +
      ", peak " +
      peak +
      ", limit " +
      MEM_LIMIT +
      "): caller-scope LOCAL nodes are leaking",
  );
}

let sink = null;

function runCtor(n) {
  for (let i = 0; i < n; i++) {
    sink = new C(i);
    if ((i & 0x3fff) === 0) sample();
  }
  assert(sink !== null, "ctor loop did not run");
  assert(
    sink.a === n - 1 && sink.b === n && sink.c === n + 1,
    "constructor instance corrupted: " + sink.a + "," + sink.b + "," + sink.c,
  );
}

function runBoundCtor(n) {
  for (let i = 0; i < n; i++) {
    sink = new B(i);
    if ((i & 0x3fff) === 0) sample();
  }
  assert(sink !== null, "bound ctor loop did not run");
  assert(
    sink.a === n - 1 && sink.b === n && sink.c === n + 1,
    "bound constructor instance corrupted: " +
      sink.a +
      "," +
      sink.b +
      "," +
      sink.c,
  );
}

function runProxyCtor(n) {
  for (let i = 0; i < n; i++) {
    sink = new P(i);
    if ((i & 0x3fff) === 0) sample();
  }
  assert(sink !== null, "proxy ctor loop did not run");
  assert(
    sink.a === n - 1 && sink.b === n && sink.c === n + 1,
    "proxy constructor instance corrupted: " +
      sink.a +
      "," +
      sink.b +
      "," +
      sink.c,
  );
}

// Generator frames are bytecode carriers (`BCWrapper` but not the narrow
// `BytecodeFunction` cast). Classifying them as native would keep every bound
// instance LOCAL in the generator's suspended scope.
function runGenBoundCtor(n) {
  function* body() {
    for (let i = 0; i < n; i++) {
      sink = new B(i);
      if ((i & 0x3fff) === 0) sample();
    }
  }
  const g = body();
  while (!g.next().done) {}
  assert(sink !== null, "generator bound ctor loop did not run");
  assert(
    sink.a === n - 1 && sink.b === n && sink.c === n + 1,
    "generator bound constructor instance corrupted: " +
      sink.a +
      "," +
      sink.b +
      "," +
      sink.c,
  );
}

function runObjlit(n) {
  for (let i = 0; i < n; i++) {
    sink = { a: i, b: i + 1, c: i + 2 };
    if ((i & 0x3fff) === 0) sample();
  }
  assert(sink !== null, "objlit loop did not run");
  assert(
    sink.a === n - 1 && sink.b === n && sink.c === n + 1,
    "object literal corrupted: " + sink.a + "," + sink.b + "," + sink.c,
  );
}

// Native caller path: `js_promise_all` is a Rust builtin that holds the
// capability object in a local while it runs element callbacks (re-entrant
// JS). That object must stay LOCAL in the native call scope; if it loses
// protection, a safepoint can collect it (debug builds assert in
// `js_promise_all`). This is a UAF canary, not a retention bound: the promise
// machinery's first-run warm-up alone exceeds the 4 MiB phase limit, so only
// functional correctness is checked here.
function runNativeCtor(n) {
  for (let i = 0; i < n; i++) {
    let seen = null;

    function Constructor(executor) {
      executor(
        function (values) {
          seen = values;
        },
        function (err) {
          throw err;
        },
      );
    }
    Constructor.resolve = function (v) {
      return v;
    };

    const p1 = {
      then(onFulfilled) {
        onFulfilled(i);
      },
    };
    Promise.all.call(Constructor, [p1]);
    assert(seen !== null, "native ctor path did not resolve at " + i);
    assert(seen[0] === i, "native ctor path corrupted at " + i);
  }
}

phase("ctor", () => runCtor(N));
phase("bound-ctor", () => runBoundCtor(N));
phase("proxy-ctor", () => runProxyCtor(N));
phase("objlit", () => runObjlit(N));
runNativeCtor(N_NATIVE);
console.log("native-ctor-promise-all: functional check passed");
// Kept last on purpose: a collection that runs *after* a generator frame is
// completed currently trips a pre-existing debug-only gc-lite `bad node`
// panic (it reproduces with the old unconditional `remove_local` as well, so
// it is unrelated to the caller-scope policy). Running this phase last still
// samples memory inside the loop without triggering it.
phase("generator-bound-ctor", () => runGenBoundCtor(N));

console.log("test_gc_ctor_scope: passed");
