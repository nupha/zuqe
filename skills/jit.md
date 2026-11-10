# Tiered JIT Usage

> For host configuration. JIT verification, lowering, code cache, and test
> details are in `docs/jit.md`. This page covers the public API and host-visible
> behavior.

## 1. Feature and semantics

```toml
[dependencies]
zuqe = { version = "0.11", features = ["jit"] } # jit is on by default
```

- `jit` compiles **already parsed bytecode**. It does not touch the parser,
  emitter, or bytecode format.
- With the default `JitTierPolicy::Automatic`, results match the interpreter
  bit for bit. An opcode the backend cannot translate keeps the function
  interpreted. This is fail-closed.
- It is not a tracing or GC-integrated JIT. Values stay in the interpreter
  frame buffer. There are no register-resident GC roots.
- `with`, generator, `async`, `eval`, and `import` opcodes are rejected by
  design.

## 2. Enable and attach the JIT

`jit` is on by default. For a custom build, include it explicitly:

```bash
cargo build --no-default-features --features "jit"
```

Then attach one JIT before running code:

```rust
use zuqe::{Jit, JitConfig, JitTierPolicy};

let mut rt = zuqe::Runtime::create()?;
let mut ctx = rt.create_context(zuqe::ScopedContext::ECMA_CONTEXT_SETUP)?;

let jit = Jit::attach_to(
    &rt,
    JitConfig::builder()
        .tier_policy(JitTierPolicy::Automatic)
        .call_threshold(32)
        .loop_threshold(56)
        .tier2_call_threshold(1024)
        .tier2_loop_threshold(2048)
        .build(),
)?;

ctx.eval(
    b"for (var i = 0, s = 0; i < 1e6; i++) s += i;",
    &zuqe::EvalName::Name("app.js".into()),
    zuqe::EvalType::Global,
    zuqe::EvalFlags::empty(),
)?;

println!("{:?}", jit.metrics());
```

`Jit::attach_to(&runtime, config)`:

- One JIT per `Runtime`. It returns `Err(JitError)` if there is no Cranelift
  backend.
- `Jit` holds a strong reference to `Runtime`. The runtime stays alive while
  the handle lives. You do not need to control drop order. A runtime can be
  attached only once.

## 3. When compilation runs

**Compilation is automatic. The host does not call `poll()` every round.** The
engine polls at three safe points:

1. interpreter trampoline frame boundaries;
2. event-loop ticks;
3. executed loop back edges. This also supports back-edge OSR: a running loop
   can switch to installed compiled code.

Verification and lowering run on worker threads. The runtime thread only
dispatches, drains, and installs. As a result:

- A single hot top-level loop queues itself at its own back edge, even if it
  never crosses a frame boundary. It then enters compiled code.
- `Jit::poll()` is for exact control only. Use
  `JitConfig::builder().auto_poll(false)` to drive it fully by hand.

## 4. Tiers and thresholds

| Policy | Behavior |
|---|---|
| `Interpreter` | Install no native code. Hooks stay idle. |
| `Tier1` | Compile the baseline tier only. |
| `Tier2` | Always specialize parts with recorded feedback. |
| `Automatic` | Default. Two gates: baseline compile, then promote to the optimizing tier. |

Each gate is a weighted budget over entries and back edges:
`L * calls + C * loops >= C * L`. `call_threshold` and `loop_threshold` are the
first gate. `tier2_call_threshold` and `tier2_loop_threshold` are the second.

## 5. Metrics

`jit.metrics()` returns `JitMetrics`, with:

- function entries, back edges, queue length and drops;
- verified, rejected, and compiled counts, plus native entries;
- interrupts, compile failures, and timeouts;
- speculative fast-path counts and code bytes;
- tiering counters: `tier2_promotions`, `functions_recompiled`, and
  `tier_downs`.

## 6. Host notes

- A function that fails to compile keeps running interpreted. No host fallback
  is needed.
- Do not rely on compile timing for semantics. The interpreter is the
  reference.
- Apple Silicon `MAP_JIT` is not implemented. Current targets are Linux
  x86_64/aarch64 (qemu), Intel macOS, and Windows x86_64.
- Do not enable `jit_dump` in production. It writes `.clif` and `.bin` files
  and is a compile-time feature.
