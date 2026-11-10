---
name: zuqe-embedding
description: >
  REQUIRED when embedding the zuqe JavaScript engine in a Rust host, writing
  host/builtin native bindings, loading ES modules, bridging Promises and Rust
  futures, configuring the tiered JIT, or developing zapi extensions. Use when
  adding native functions, defining a native class with `def_native_object!`,
  creating a native module, implementing `GcTrace`/write barriers on payloads,
  running `zuqe-gc-audit`, `--gc-stress`, or reviewing GC safety. Triggers:
  zuqe, ScopedContext, CallContext, JSResult, CallbackFn, def_native_object,
  NativeObject, ClassDef, ClassCtorDef, class_init, init_class_proto_extends,
  link_class_extends, create_module, FieldDef, GcTrace, GcBind, WriteBarrier,
  unsafe_empty_gc_trace, no-GC payload, bind_value, gc_audit, gc-stress,
  write barrier, zapi, native class, native module, payload, eval_wait,
  EvalName, run_async, Promise::into_future, Jit::attach_to. Excludes pure
  JS/TS application logic that does not touch the embedding API.
---

# zuqe Embedding Skill

This directory is the user guide for embedding and extending **zuqe**. It shows
how a Rust host calls, embeds, and extends the engine. It does not cover engine
internals such as the GC heap, tick loop, or JIT lowering. Those live in
`docs/`.

> The API can change. Check the source and the checklists here when writing or
> reviewing code.

## Read this first: three boundaries

1. **Host/builtin Rust vs zapi extensions are different paths.**
   - Host/builtin code uses `ScopedContext`, `CallContext`, `JSResult`,
     `CallbackFn`, `def_native_object!`, `FieldDef`, and
     `ScopedContext::create_module`.
   - zapi extensions use `zuqe-zapi`, `ZqApi`, `ZqBodyFn`, `ZqRawValue`,
     `ZqFieldDef`, and `#[zq_module]` (which generates
     `zq_module_register`). The host loads them through the zapi host.
   - Never mix the two type systems or registration paths.
2. **GC safety needs two parts.** `GcTrace` covers full marks. Old-to-young
   writes also need a write barrier (`gc_generations`). You need both. Do not
   suggest `gc_protect_transient` as a fix. Read
   [GC Safety Rules](gc-safety.md) first, then the trace and barrier details.
3. **Modules have two paths.** JS/ESM modules come from a host loader and
   normalizer. Native modules are registered with `create_module`/`FieldDef`.
   zapi extensions use the cdylib ABI. See
   [Modules: ESM Loading and Native Modules](native-module.md).

## Topic index

Always start with the global workflow and checklist. If GC nodes are involved,
read the GC safety rules first.

### Global

| Topic | File | When to read |
|---|---|---|
| **Workflow and checklist** | [workflow-checklist.md](workflow-checklist.md) | Every zuqe embedding task |
| **Basics** | [Basics: Runtime, Context, and Execution](basics.md) | Cargo features, Runtime/Context, `eval`/`eval_wait`, `EvalName`, limits, module hooks |
| **Promise and Future** | [promise.md](promise.md) | `Promise::create*`, channels/slots, `into_future`, `create_from_future`, `run_async` |

### JIT

| Topic | File | When to read |
|---|---|---|
| **Tiered JIT Usage** | [Tiered JIT Usage](jit.md) | Enable `jit`, `Jit::attach_to`, tiers, thresholds, metrics, compile timing |

### GC

| Topic | File | When to read |
|---|---|---|
| **GC safety rules (read first)** | [gc-safety.md](gc-safety.md) | Rules 0-5, the two parts, failure modes, template |
| ↳ Trace details | [gc-trace.md](gc-trace.md) | `GcTrace`, `thread_local!`, keeping values alive across JS calls |
| ↳ Write barriers and tools | [gc-write-barrier.md](gc-write-barrier.md) | Barrier patterns, failure modes, static audit, minor GC and stress tests |

### Bindings and extensions

| Topic | File | When to read |
|---|---|---|
| Native functions | [native-function.md](native-function.md) | Register host functions, callbacks, arguments, errors |
| Native classes | [native-class.md](native-class.md) | `def_native_object!`, payload, `GcTrace`, inheritance |
| Modules (ESM and native) | [native-module.md](native-module.md) | `create_module`/`FieldDef`, loader/normalizer, import attributes, `import.meta` |
| zapi extensions | [zapi-extension.md](zapi-extension.md) | cdylib setup, `#[zq_*]` macros, classes, async work, GC trace |

## Key rules

- **GC rules:** [GC Safety Rules](gc-safety.md) is the only rule source. Trace
  examples are in [gc-trace.md](gc-trace.md). Barrier and audit tools are in
  [gc-write-barrier.md](gc-write-barrier.md). Other files link to these rules
  and do not repeat them.
- **Payload shapes:** use `()` for zero-size or stateless payloads. Use
  `unsafe_empty_gc_trace!` when a non-zero payload has fields that do not
  implement `GcTrace`. Use `#[derive(GcTrace)]` when fields can be traced. Do
  not use an empty hand-written impl by default. Do not hide GC fields behind
  an empty trace macro; the audit checks this.
- **Project rules:** GC node safe. Clear zapi boundary. **Class inheritance has
  one path only.** Build root classes first. Link host classes with
  `def_native_object!(..., extends: X)` or with the engine primitives
  `ScopedContext::link_class_extends` and `init_class_proto_extends`. Do not
  write `set_prototype` by hand. Do not expose InitFn to third parties. Use the
  right path for the audience.
- **Entry point:** with default `async_support`, `Context::eval_wait(...)` is
  async and does **not** take `&mut Runtime`. Module entry points must use
  `EvalName::BaseUrl`.
- **Source examples:** `src/class/native.rs`, `src/class/using.rs`,
  `src/module/mdef.rs`, `src/function/*`, `src/gc/guard.rs`,
  `src/builtins/*`, `src/zapi/host.rs`, `examples/`, and `tests/gc_audit.rs`.

## How to use this skill

1. Decide the audience and path: host/builtin Rust or a zapi cdylib extension.
   Do not mix them.
2. Read the [workflow and checklist](workflow-checklist.md).
3. Read the topic file. Copy the template and adjust it to the real types and
   API. Do not guess field names.
4. If GC nodes are involved, read [GC Safety Rules](gc-safety.md) and follow
   rules 0-5.
5. Before you commit, run the static audit
   (`cargo test --test gc_audit`). Run focused dynamic checks with
   debug + `gc_generations`, and use `--gc-stress` when needed. Then recheck
   [workflow-checklist.md](workflow-checklist.md).
