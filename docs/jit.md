# Tiered JIT

Zuqe ships a tiered bytecode JIT behind the `jit` feature (enabled by default;
build with `--no-default-features` plus an explicit feature list to leave it
out). It compiles hot JavaScript functions to Cranelift-generated machine code
while the interpreter stays the reference for correctness.

## What it is

- A **tiered** JIT for *already parsed* bytecode: function entries and loop back
  edges feed a hotness counter; a hot function is verified, compiled, and
  installed on the function itself.
- **Semantics-preserving**: under the production default (`Automatic`) the
  observable result is bit-for-bit identical to the interpreter. Anything the
  backend is unsure about falls back to the interpreter.
- **Fail-closed**: a function containing an opcode the backend does not
  translate is *never* compiled; it simply keeps running interpreted.
- **Bounded and observable**: code memory, metadata, queue length and compile
  attempts are quota-limited; counters are exported through `JitMetrics`.
- **Portable in design, Unix-first in practice**: W^X code memory with an
  aarch64 i-cache flush. Linux x86_64/aarch64 (qemu) and **Intel macOS** work;
  Apple Silicon (`MAP_JIT`) and Windows (`VirtualAlloc`/`VirtualProtect`) are not
  implemented yet.

## What it is not

- Not a source-level engine: it does not touch the parser, the emitter, or the
  bytecode format. Bytecode is the only input.
- Not a second implementation of JavaScript semantics: complex operations
  (arithmetic coercion, property access, calls, throws) are executed by calling
  the interpreter's own Rust functions.
- Not a general inlining compiler: there is a monomorphic direct-call path and a
  small frame-inlining subset (pure integer bodies plus the loop/call/exception/
  allocation extensions), but no source-level inlining and no `DeoptMap`. The
  general call path is a helper that keeps the caller's frame authoritative.
- Not a full property-access compiler: object shapes stay in helpers by default.
  A few shapes — array and typed-array elements, own data properties, global
  variable slots — are inlined **behind guards that re-check on every access**;
  a miss falls back to the helper, so a shape change is just one miss.
- Not a tracing/GC-integrated JIT: values live in the same frame buffer the
  interpreter uses; there are no register-resident GC roots.
- Not a `with`/generator/`async`/`eval`/`import` compiler: those opcodes are
  rejected on purpose.

## Using it

```rust
use zuqe::{Jit, JitConfig, JitTierPolicy};

let mut rt = zuqe::Runtime::create()?;
let mut ctx = rt.create_context(zuqe::ScopedContext::ECMA_CONTEXT_SETUP)?;
let jit = Jit::attach_to(&rt, JitConfig::builder()
    .tier_policy(JitTierPolicy::Automatic)   // production default
    .call_threshold(32)                      // gate 1: 32 entries alone queue the function
    .loop_threshold(56)                      // …or 56 back edges; mixed counts share one budget
    .tier2_call_threshold(1024)              // gate 2: promote to the optimizing tier
    .tier2_loop_threshold(2048)              // gate 2: …or this many back edges
    .build())?;

ctx.eval("for (var i = 0, s = 0; i < 1e6; i++) s += i;".as_bytes(),
         &zuqe::EvalName::Name("app.js".into()),
         zuqe::EvalType::Global, zuqe::EvalFlags::empty())?;

// No poll call: the engine compiles queued functions automatically at
// trampoline frame boundaries, on every event-loop tick, and at a hot loop's
// back edge (this snippet is a single top-level loop, so it is the back edge
// that hands the frame to native code — back-edge OSR).
println!("{:?}", jit.metrics());
```

- `Jit::attach_to(&runtime, config)` — one JIT per `Runtime`; fails if the host
  has no Cranelift backend.
- **Compilation is automatic.** The runtime polls at three engine safe points:
  the interpreter's trampoline (frame boundaries), the event-loop tick, and a
  taken loop back edge. No host call is needed. The poll is a no-op unless work
  is queued or compiling and no native frame is in flight. Verification and
  lowering run on worker threads; a poll only dispatches, drains and installs.
  The back-edge point is also where a *running* frame switches to native code:
  a top-level loop that never calls anything (so it never crosses a frame
  boundary) queues itself at its own back edge and enters the compiled loop
  header once the worker's result is installed — back-edge OSR.
- `Jit::poll()` — optional explicit poll for hosts/tests that want to compile at
  an exact moment; pair it with `JitConfig::auto_poll(false)` when the timing
  must be fully manual.
- `JitMetrics` — function entries, back edges, queue length/drops,
  verified/rejected/compiled counts, native entries, interrupts, compile
  failures/timeouts, speculative fast-path counts, code bytes, and the tiering
  counters (`tier2_promotions`, `functions_recompiled`, `tier_downs`).
- Tiers: `Interpreter` (hooks inert), `Tier1` (baseline only), `Tier2` (always
  specialize what feedback recorded), `Automatic` (default; two gates — the
  first compiles the baseline tier, the second promotes the function to the
  optimizing tier, and the specialization is gated on evidence). Each gate is a
  single weighted budget over entries and back edges: `L×calls + C×loops ≥ C×L`
  for a configured `(C, L)` pair, so the corners `C` calls and `L` back edges
  fire as before and a mix of both fires earlier.

## How it works

### Pipeline

```
Bytecode (unchanged parser output)
   │  interpreter: note_enter / note_backedge  →  hotness counter
   │  interpreter: note_binary / note_value / note_local_update → Type feedback (Tier2)
   ▼  engine safe point (trampoline frame boundary / event-loop tick /
      loop back edge; explicit Jit::poll optional)
1. CompileSnapshot        pointer-free copy of the function (Send, no GC refs)
2. verify                 decode → opcode policy → index bounds → CFG → abstract stack
                          (fail-closed: any failure means "never compile")
3. lowering               VerifiedFunction → Cranelift IR → machine code
4. publish                mmap(RW) → write the code → mprotect(RX) → flush i-cache
                          (relocations are rejected, fail-closed)
5. install                NativeEntry stored in the function's `jit` slot
   ▼
next call → native entry runs; on exit the interpreter continues
   (a running frame can also enter native mid-flight: the conveyed `pc` may be
    an OSR resume point — a loop header reported by a back edge, or the pc after
    a call the trampoline performed)
```

### Design principles

1. **The interpreter stays authoritative.** It records hotness and feedback, and
   it owns every operation that is not a pure frame/code operation. Native code
   never has to re-implement JavaScript.
2. **The frame buffer is the single source of truth.** Live values live in the
   `StackFrame` buffer (`[args][locals][operands]`). Inside a basic block a value
   may live only in a register, but every observation point (helper call, exit,
   branch, safepoint, block end) flushes it back first. GC and exception
   unwinding therefore always see a complete frame.
3. **No deopt map.** Because flushing *is* materialization and a slot's stack
   height is the interpreter's own, there is no "reconstruct state by pc"
   moment. A guard miss simply calls the helper on the same frame. (Decision
   D-25; cross-block values travel as block parameters.)
4. **Helpers are interpreter semantics.** The generated code calls a trampoline
   table of `extern "C"` functions in `abi.rs`; each one reads operands straight
   out of the frame, calls the interpreter's existing Rust function, and writes
   the result back.
5. **Calls keep the caller in native code.** The `call` helpers run the callee
   through `call_internal` on the same frame: a native callee is entered
   natively, and the caller never exits to the interpreter (D-6, route A). A
   monomorphic site whose recorded callee matches enters that callee's machine
   code directly, without `call_internal` or the trampoline (route B, step 1);
   a small eligible subset is lowered into the caller's own code with no second
   activation (route B, step 2). `f.apply(thisArg, args)` with a packed array has
   its own direct path.
6. **Speculation is guarded and reversible.** Tier 2 emits an inline `Int`
   (and `Float64`) fast path only when the feedback table saw those types, with a
   runtime guard; a miss falls back to the helper. A function whose guards miss
   too often is recompiled without speculation ("tier-down").

### Exit kinds

Native code returns a small status to the interpreter:

| Kind | Meaning |
|---|---|
| `Done` | returned a value |
| `Resume` | hand this pc back to the interpreter (deliberate slow paths, e.g. a TDZ read) |
| `Exception` | a JS exception was raised; the interpreter scans this frame for a handler |
| `Interrupt` | the host interrupt fired at a back-edge safepoint; **bypasses JS `catch`** |
| `Retry` | re-enter the interpreter at this pc |

(`JitExitKind` also declares `Unwind`, `CallBytecode` and `TailCall`; the
baseline lowering does not emit them.)

Two different kinds of "failure" must not be confused: a **compile-time reject**
(policy/verify) is permanent — the function is never compiled; a **run-time
fallback** (exception, interrupt, guard miss, a deliberate slow path) returns to
the interpreter for this activation only.

### Code memory and lifetime

Published mappings are bounded by a byte quota. A function's slot holds a strong
reference to its mapping; the cache only keeps a weak one, so a mapping is freed
when the last function that could call it is collected. When the quota is tight,
the least-recently-used live mapping is retired (the function falls back to the
interpreter and recompiles if it becomes hot again). Retirement runs only inside
`poll`, between native activations, so no in-flight frame can hold a stale entry.

### Function-scoped analysis

A minimal function-scoped analysis (`analysis/`) provides the data-flow view the
bytecode does not: which block reads/writes which slot, dominance, natural
loops, and liveness. It powers cross-block local residency, dead-store
elimination and block-local CSE. It is deliberately **not** a new instruction
representation, an SSA form, or a deopt map — the value model stays the frame.

Terminology: in this project **"IR" always means the Cranelift IR** (clif)
that the lowering produces. The analysis layer produces *facts*, not
instructions, so it is called analysis, not IR.

### Diagnostics

- **Site counters are always compiled in** (they are a few increments in the
  generated code, and they are the production input of the guard feedback):
  `JitMetrics` carries the inline guards' hits and misses
  (`property_guard_*`, `global_guard_*`), the monomorphic call path's
  (`inline_call_*`, `frame_inline_hits`), and the speculative int guards'
  (`specialized_guard_misses`). `examples/jit-bench.rs` prints them per run,
  under both protocols, with no feature flag:

  ```sh
  cargo run --release --example jit-bench -- --kernel ctor-heavy
  # property_guard (hits/misses): interpreter=0/0 tier1=… automatic=…
  ```

- **The IR/machine-code dump** is behind the **`jit_dump`** cargo feature,
  which is off by default and never in `default`. With the feature, setting
  `JIT_DUMP=1` when the JIT attaches writes each compile's Cranelift IR and
  machine code to `<function-id>-<tier>.clif` (header line included: code byte
  count) and `<function-id>-<tier>.bin` in `JIT_DUMP_DIR` (default `.`) — no
  disassembler. `JIT_DUMP=0`/unset keeps it off; without the feature the
  variable is never read at all, and `ci-gate.sh` asserts the strings are
  absent from the release artifact.

  ```sh
  JIT_DUMP=1 cargo run --release --features jit_dump --example jit-bench -- --kernel ctor-heavy
  cargo test --features jit_dump --lib jit::dump      # the dump smoke test
  ```

### Limits and interrupts

Loop back edges run a safepoint helper that delivers host interrupts and
triggered collections. Interrupts are control flow, not exceptions: no JS
`catch` runs. `compile_timeout_ms` is a real deadline: an attempt past it is
abandoned and its late completion is discarded.

## Document map

| Document | Contents |
|---|---|
| [`memo/jit_impl.md`](../memo/jit_impl.md) | The single implementation guide: design, pipeline, optimizations, thresholds, traps, decisions, platform notes, and a compressed history. Start here. |
| [`memo/jit_todo.md`](../memo/jit_todo.md) | Status only: implemented / not implemented / next steps. |

## Code map

| Area | Files |
|---|---|
| Host API, per-runtime state, interpreter hooks | `src/jit/mod.rs`, `config.rs`, `metrics.rs`, `error.rs` |
| Opcode strategy (generated from the opcode table) | `build_opcodes.rs` → `src/jit/policy.rs` |
| Pointer-free function snapshot | `src/jit/snapshot.rs` |
| Frontend gate (decode, CFG, abstract stack, verify) | `src/jit/bytecode/{mod,decode,cfg,stack,verify}.rs` |
| Baseline + Tier-2 lowering, inline guards | `src/jit/compiler.rs` |
| Monomorphic direct call / frame inlining | `src/jit/direct_call.rs`, `src/jit/inline_call.rs` |
| Native ABI, frame layout, helper trampolines | `src/jit/abi.rs` |
| Function-scoped analysis (data-flow facts: liveness, dominators, loops, value numbering) | `src/jit/analysis/{mod,values}.rs` |
| Tier-2 type feedback table | `src/jit/feedback.rs` |
| Hotness counters, thresholds, tier policy | `src/jit/runtime/hotness.rs`, `src/jit/config.rs` |
| Compilation queue and function lifecycle | `src/jit/runtime/coordinator.rs` |
| Executable memory (W^X, i-cache) and code cache | `src/jit/runtime/install.rs`, `src/jit/platform.rs`, `src/jit/code_cache.rs` |
| Measured compile cost (the cost gate's input) | `src/jit/rate.rs` |
| IR / machine-code dump (feature `jit_dump`, off by default) | `src/jit/dump.rs` |
| Interpreter integration points | `src/bytecode/exec_stackless.rs` (entry/back-edge hooks, `invoke_native`), `src/runtime/tick.rs` |
| Per-function native entry slot | `src/function/bytecode.rs` (`FunctionSlot`) |

## JIT testing framework

A JIT can be wrong in two different ways, and one kind of test cannot see both:

1. **Semantic divergence** — the compiled code computes a different result from
   the interpreter (wrong coercion, missing guard, broken GC interaction).
2. **Codegen/path divergence** — the result happens to match, but the machine
   code does something else than intended (falls back to the helper, writes the
   wrong element width, emits an extra body, skips an inline path).

The suite therefore has **two paths**: a **black box** (differential testing at
the JS level) and a **white box** (the hand-written bytecode harness). They
answer different questions, and every new test belongs to exactly one of them.
This split is part of the contribution contract: do not put a test on the wrong
side because "it is easier there".

### Black box: differential testing (the default)

Run the same script in `Interpreter`, `Tier1`, `Tier2` and `Automatic`, and
compare tag-exact results, exceptions and observed side effects. The interpreter
is the oracle; no expected value is written by hand. Located in `tests/jit/*`
(`jit_differential.rs`, `jit_semantics.rs`, `jit_opcodes.rs`, `jit_field.rs`,
`jit_array.rs`, …), run with `cargo test --features jit --test jit`.

This path reproduces the real pipeline: guards, helper fallbacks, GC,
`detach`/`resize`/shared buffers, prototypes, user `valueOf`, inline calls and
exceptions.

**Strengths:** real engine state, broad coverage, expectations never go stale
(the interpreter is the reference), and it finds *unknown* engine bugs — e.g. an
`ArrayBuffer.resize()` that left the JIT's byte mirror dangling was caught here.

**Weaknesses:** it cannot tell "inline path" from "helper fallback" (both give
the same result); it cannot see raw bytes, codegen-only tags, internal layouts
or emission counts; it cannot reach opcodes/shapes the JS frontend never emits;
and it cannot catch a bug shared by the interpreter and the JIT.

### White box: the hand-written bytecode harness

Assemble a bytecode body with `Asm`, compile it with the real compiler, and run
it against a synthetic `JitExecFrame` (`test_harness::run_function`) or a copied
inline body (`run_inline`). Located in `src/jit/test_harness.rs`,
`src/jit/inline_call.rs` (`#[test]`) and `src/jit/compiler.rs` (`#[test]`), run
with `cargo test --features jit --lib`.

A real heap object may be passed in as an argument (e.g. a typed array for the
inline element paths), but the body must not call a helper: the synthetic frame
has no scope/arena/bytecode behind it. A guard miss therefore **faults** instead
of silently passing — which is what makes the harness the only proof that a
given inline path ran.

**Strengths:** proves the path executed; bit-level (element width, raw bytes,
exact `JSValue` tags); internal protocol (argument-window layout, emission
counts); reaches frontend-unreachable opcodes and limit boundaries; and can run
exhaustive tables with hard-coded expected values that are independent of the
interpreter.

**Weaknesses:** no helpers, so no GC/detach/resize/prototype/user-code paths; a
synthetic frame is not a real activation; expectations are hand-written and must
be kept in sync with the engine; a regression fails as a fault, not an
assertion; and it cannot discover unknown engine bugs.

### Ownership rules

**Default: black box.** If JS can reach the behaviour and observe the result, it
goes to `tests/jit/*`. Every guard hit or miss, helper fallback, GC interaction,
buffer state and exception path is a black-box test.

**White box is only for these three categories:**

1. **Unreachable from the frontend** — opcodes or shapes the parser/optimizer
   never emits (`set_arg`, `set_loc`, `dup`, `drop`, `nop`, the 96/97-instruction
   inline-eligibility boundary, …).
2. **Not observable from JS** — raw element bytes and store width, exact tags
   produced by codegen, the argument-window layout, emission counts, and the
   **inline-vs-helper path** itself.
3. **Exhaustive tables with hard-coded expectations** — per-opcode tag tables,
   per-conversion byte tables. Differential only catches "the two sides
   disagree"; a table also catches "both are wrong". Such a table must stay
   limited to lowering facts.

**Anti-rules** (reject in review):

- Do not re-test user-visible semantics in the harness when a black-box case
  already covers them.
- A green differential suite does **not** prove an inline path ran. If that
  matters, add a harness case.
- Do not use the harness for anything that needs a helper, GC, an exception,
  `detach`/`resize`, or user code — the synthetic frame cannot support it.
- Do not derive a harness expectation from "whatever the interpreter does";
  hard-code the spec value, otherwise it is just a slower differential test.
- A white-box test must name its differential blind spot in one line of its doc
  comment ("differential cannot see …"), so the next maintainer knows why it
  exists and does not delete it.

### Decision checklist

When adding a JIT test, walk this list and stop at the first match:

1. Can JS produce the input and observe the result? → **black box**.
2. Does the frontend never emit the shape, or can JS not observe the fact
   (bytes, tags, layout, path, counts)? → **white box**.
3. Is it a per-opcode / per-conversion exhaustive table whose expected values
   can be written down independently of the interpreter? → **white box** (and
   add one representative black-box case if the behaviour is user-visible).
4. None of the above? It is a unit test of a Rust helper, not a JIT test.

Note that neither path can detect "the interpreter itself is wrong" — the JIT
matches it by design. Test262 and the pinned corpora cover that.

### Worked examples

| Question | Path | Why |
|---|---|---|
| Did a typed array store really go inline, and did it write 1/2/4/8 bytes? | White box (raw view bytes + neighbour element) | Not observable: the helper fallback gives the same result |
| Do `Float64 → Int32` truncation/wrap and `Uint8ClampedArray` rounding match `ToInt32` / `to_u8_clamped` at every boundary? | Both: black-box differential for semantics, white-box table for exhaustiveness | Two independent oracles |
| Does `ArrayBuffer.prototype.resize()` leave the JIT's byte mirror dangling? | Black box | Needs real engine state (`realloc`, live views) |
| Does the `set_loc` opcode lower? | White box | The JS frontend never emits it |
| Do polymorphic property sites still fall back correctly? | Black box | Real shapes, helper path |
| Is an inlined callee body emitted twice for the post-call OSR tail? | White box (`inline_emissions`) | Internal emission count |

## Tests and benchmarking

| Suite | Path | Command |
|---|---|---|
| White-box lowering harness | `src/jit/test_harness.rs`, `src/jit/inline_call.rs`, `src/jit/compiler.rs` | `cargo test --features jit --lib` |
| Differential (4 modes, bit-exact) | `tests/jit/jit_differential.rs` | `cargo test --features jit --test jit jit_differential` |
| Per-opcode end-to-end + claim audit | `tests/jit/jit_opcodes.rs`, `tests/jit/opcode-claims.py` | `cargo test --features jit --test jit jit_opcodes` |
| Semantic stress | `tests/jit/jit_semantics.rs` | `cargo test --features jit --test jit jit_semantics` |
| Native vs interpreter, exceptions, interrupts | `tests/jit/jit_verify.rs`, `jit_interrupt.rs` | `cargo test --features jit --test jit` |
| Tier feedback / speculation | `tests/jit/jit_feedback.rs` | `cargo test --features jit --test jit jit_feedback` |
| Inline guards (property/array/typed array/global) | `tests/jit/jit_field.rs`, `jit_array.rs`, `jit_global.rs` | `cargo test --features jit --test jit jit_field` |
| Calls: direct path, frame inlining, `apply` | `tests/jit/jit_inline_call.rs`, `jit_frame_inline.rs` | `cargo test --features jit --test jit jit_inline_call` |
| Bench matrix (51 kernels × 4 modes) | `examples/jit-bench.rs`, `tests/jit/jit-bench-matrix.sh` | `./tests/jit/jit-bench-matrix.sh --protocol process` |
| Compile-threshold sweep | `tests/jit/jit-threshold-grid.sh` | `./tests/jit/jit-threshold-grid.sh` |
| Feature-gating / hardening gates | `tests/jit/ci-gate.sh`, `tests/jit/ci-sanitize.sh`, `.github/workflows/ci.yml` | `./tests/jit/ci-gate.sh` |

## Current status

Implemented: baseline + Tier-2 lowering, type feedback and guarded
int/float fast paths, tier-down, cross-block residency / DCE / CSE, back-edge
OSR (including post-call resume entries), code-cache retirement, metrics/quotas,
worker-thread compilation (`Jit::poll` only dispatches, drains and installs), and
the two-gate tiering (a first gate compiles the baseline tier, a second gate
promotes the function to the optimizing tier; the native tier reports its own
activations and back edges back into the hotness counters, and a demoted
function waits out a cooldown before it is promoted again).

Also implemented, on the access side: inline guards for array and typed-array
element reads/writes, own data property reads/writes (including `get_field2` /
`get_array_el2` and a typed array's `length`), and global variable reads/writes
— each re-checked per access. On the call side: the monomorphic direct path
(route B step 1), a frame-inlining subset (route B step 2), and the
`Function.prototype.apply` method-call direct path.

Not implemented: a broader frame-inlining subset, `call_constructor` direct
entry, cross-eval content identity (the code cache is keyed by bytecode node),
Apple Silicon/Windows executable memory, and real-hardware aarch64 validation.
ASan is blocked by the allocator under ASan, not by the JIT (see
`tests/jit/ci-sanitize.sh`); TSan runs clean. `with`/generator/`async`/`eval`/
`import` opcodes stay rejected by design. Kernel-level performance is uneven
after the recent interpreter speedups: see [`memo/jit_todo.md`](../memo/jit_todo.md)
for the measured matrix, the kernels where native code currently loses, and the
task list.
