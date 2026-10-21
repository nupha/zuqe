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
  aarch64 i-cache flush; macOS/Windows executable memory is not implemented yet.

## What it is not

- Not a source-level engine: it does not touch the parser, the emitter, or the
  bytecode format. Bytecode is the only input.
- Not a second implementation of JavaScript semantics: complex operations
  (arithmetic coercion, property access, calls, throws) are executed by calling
  the interpreter's own Rust functions.
- Not an inlining compiler: no frame inlining, no source-level deopt. Function
  calls run through a helper that keeps the caller's frame authoritative
  (single-shape inline calls are future work).
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
    .call_threshold(32)                      // gate 1: queue the function
    .loop_threshold(56)                      // gate 1: a back edge queues it
    .tier2_call_threshold(1024)              // gate 2: promote to the optimizing tier
    .tier2_loop_threshold(2048)              // gate 2: …or this many back edges
    .build())?;

ctx.eval("for (var i = 0, s = 0; i < 1e6; i++) s += i;".as_bytes(),
         &zuqe::EvalName::Name("app.js".into()),
         zuqe::EvalType::Global, zuqe::EvalFlags::empty())?;

// No poll call: the engine compiles queued functions automatically at
// trampoline frame boundaries, on every event-loop tick, and at a hot loop's
// back edge (this snippet is a single top-level loop, so it is the back edge
// that hands the frame to native code — inline OSR).
println!("{:?}", jit.metrics());
```

- `Jit::attach_to(&runtime, config)` — one JIT per `Runtime`; fails if the host
  has no Cranelift backend.
- **Compilation is automatic.** The runtime polls at three engine safe points:
  the interpreter's trampoline (frame boundaries), the event-loop tick, and a
  taken loop back edge. No host call is needed. The poll is a no-op unless work
  is queued and no native frame is in flight.
  The back-edge point is also where a *running* frame switches to native code:
  a top-level loop that never calls anything (so it never crosses a frame
  boundary) compiles itself at its own back edge and continues natively inside
  the same `eval` — inline OSR into the compiled loop header.
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
  optimizing tier, and the specialization is gated on evidence).

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
   natively, and the caller never exits to the interpreter (D-6, route A).
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

### Limits and interrupts

Loop back edges run a safepoint helper that delivers host interrupts and
triggered collections. Interrupts are control flow, not exceptions: no JS
`catch` runs. `compile_timeout_ms` classifies a *failed* compile as timed out; it
is not a preemptive deadline (compilation is inline).

## Document map

| Document | Contents |
|---|---|
| [`memo/jit_plan.md`](../memo/jit_plan.md) | Plan, design, decisions (§9), enhancement directions (§13), background thread (§12). |
| [`memo/jit_handover.md`](../memo/jit_handover.md) | Per-item progress log (§6.x): what was built, measurements, acceptance. |
| [`memo/jit_todo.md`](../memo/jit_todo.md) | Status only: implemented / not implemented / next steps. |

## Code map

| Area | Files |
|---|---|
| Host API, per-runtime state, interpreter hooks | `src/jit/mod.rs`, `config.rs`, `metrics.rs`, `error.rs` |
| Opcode strategy (generated from the opcode table) | `build_opcodes.rs` → `src/jit/policy.rs` |
| Pointer-free function snapshot | `src/jit/snapshot.rs` |
| Frontend gate (decode, CFG, abstract stack, verify) | `src/jit/bytecode/{mod,decode,cfg,stack,verify}.rs` |
| Baseline + Tier-2 lowering to Cranelift | `src/jit/compiler.rs` |
| Native ABI, frame layout, helper trampolines | `src/jit/abi.rs` |
| Function-scoped analysis (data-flow facts: liveness, dominators, loops, value numbering) | `src/jit/analysis/{mod,values}.rs` |
| Tier-2 type feedback table | `src/jit/feedback.rs` |
| Hotness counters, thresholds, tier policy | `src/jit/runtime/hotness.rs`, `src/jit/config.rs` |
| Compilation queue and function lifecycle | `src/jit/runtime/coordinator.rs` |
| Executable memory (W^X, i-cache) and code cache | `src/jit/runtime/install.rs`, `src/jit/platform.rs`, `src/jit/code_cache.rs` |
| Interpreter integration points | `src/bytecode/exec_stackless.rs` (entry/back-edge hooks, `invoke_native`), `src/runtime/tick.rs` |
| Per-function native entry slot | `src/function/bytecode.rs` (`FunctionSlot`) |

## Tests and benchmarking

| Suite | Path | Command |
|---|---|---|
| Differential (4 modes, bit-exact) | `tests/jit/jit_differential.rs` | `cargo test --features jit --test jit jit_differential` |
| Per-opcode end-to-end + claim audit | `tests/jit/jit_opcodes.rs`, `tests/jit/opcode-claims.py` | `cargo test --features jit --test jit_opcodes` |
| Semantic stress | `tests/jit/jit_semantics.rs` | `cargo test --features jit --test jit jit_semantics` |
| Native vs interpreter, exceptions, interrupts | `tests/jit/jit_verify.rs`, `jit_interrupt.rs` | `cargo test --features jit --test jit` |
| Tier feedback / speculation | `tests/jit/jit_feedback.rs` | `cargo test --features jit --test jit jit_feedback` |
| Bench matrix (35 kernels × 4 modes) | `examples/jit-bench.rs`, `tests/jit/jit-bench-matrix.sh` | `./tests/jit/jit-bench-matrix.sh --protocol process` |
| Compile-threshold sweep | `tests/jit/jit-threshold-grid.sh` | `./tests/jit/jit-threshold-grid.sh` |
| Feature-gating / hardening gates | `tests/jit/ci-gate.sh`, `tests/jit/ci-sanitize.sh`, `.github/workflows/ci.yml` | `./tests/jit/ci-gate.sh` |

## Current status

Implemented: baseline + Tier-2 lowering, type feedback and guarded
int/float fast paths, tier-down, property/array access helpers with inline
caches, global-variable helpers, cross-block residency / DCE / CSE, native calls
(route A), inline back-edge OSR, code-cache retirement, metrics/quotas, the
background compilation thread (`jit_background`, in `default`; `Jit::poll` only
dispatches, drains and installs), and the two-gate tiering (a first gate
compiles the baseline tier, a second gate promotes the function to the
optimizing tier; the native tier reports its own activations and back edges
back into the hotness counters, and a demoted function waits out a cooldown
before it is promoted again).

Not implemented: single-shape inline calls (D-6 route B), macOS/Windows
executable memory, and real-hardware aarch64 validation. ASan is blocked by the
allocator under ASan, not by the JIT (see `tests/jit/ci-sanitize.sh`); TSan runs
clean. `with`/generator/`async`/`eval`/`import` opcodes stay rejected by design.
See [`memo/jit_todo.md`](../memo/jit_todo.md) for the current list.
