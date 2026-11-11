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
how a Rust host calls, embeds, and extends the engine. The topic pages live in
[`references/`](references/) — start from the topic index below. It does not
cover engine internals such as the GC heap, tick loop, or JIT lowering. Those
live in `docs/`.

> The API can change. Check the source and the checklists here when writing or
> reviewing code.

## Read this first: three boundaries

1. **Host/builtin Rust vs zapi extensions are different paths.**
   - Host/builtin code uses `ScopedContext`, `CallContext`, `JSResult`,
     `CallbackFn`, `def_native_object!`, `FieldDef`, and
     `ScopedContext::create_module`.
   - zapi extensions are a separate crate's topic: authoring one needs only
     `zuqe_zapi` / `zuqe_zapi_macros`, no engine code. See
     `zuqe-zapi/skills/SKILL.md` (in a seperate crate).
   - Never mix the two type systems or registration paths.
2. **GC safety needs two parts.** `GcTrace` covers full marks. Old-to-young
   writes also need a write barrier (`gc_generations`). You need both. Do not
   suggest `gc_protect_transient` as a fix. Read
   [GC Safety Rules](references/gc-safety.md) first, then the trace and barrier details.
3. **Modules have two paths.** JS/ESM modules come from a host loader and
   normalizer. Native modules are registered with `create_module`/`FieldDef`.
   Loadable zapi cdylibs are a third path, written against `zuqe-zapi` alone.
   See [Modules: ESM Loading and Native Modules](references/native-module.md).

## Topic index

Always start with the global workflow and checklist. If GC nodes are involved,
read the GC safety rules first.

### Global

| Topic                      | File                                                            | When to read                                                                                                                                                                                               |
| -------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Workflow and checklist** | [workflow-checklist.md](references/workflow-checklist.md)       | Start every task here: requirement-to-path workflow, then the per-area review checklist to run before commit                                                                                               |
| **Basics**                 | [Basics: Runtime, Context, and Execution](references/basics.md) | First page for a new host: choose Cargo features, create a Runtime and Context, run a script or module and read the ExitStatus, name the entry correctly, cap memory and stack, install the module loader  |
| **Promise and Future**     | [promise.md](references/promise.md)                             | Bridging JS and Rust async: return a Promise that settles later, `await` a JS Promise in Rust, hand results over from threads or blocking IO, decide whether an unobserved Promise keeps the process alive |

### JIT

| Topic                | File                                  | When to read                                                                                                                                 |
| -------------------- | ------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| **Tiered JIT Usage** | [Tiered JIT Usage](references/jit.md) | Turn `jit` on and attach it to a Runtime, choose a tier policy and thresholds, read `jit.metrics()`, and know which opcodes stay interpreted |

### GC

| Topic                            | File                                                  | When to read                                                                                                                                                                                      |
| -------------------------------- | ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **GC safety rules (read first)** | [gc-safety.md](references/gc-safety.md)               | The only rule source: why a node needs both a trace and a write barrier, rules 0-5 with their failure modes, and the standard safe template                                                       |
| ↳ Trace details                  | [gc-trace.md](references/gc-trace.md)                 | Trace examples rule by rule, then the recurring pitfalls (a missed field, a cached JS object, a borrow or `remove` held across JS, a weak target, a hidden GC field) and how to debug a lost node |
| ↳ Write barriers and tools       | [gc-write-barrier.md](references/gc-write-barrier.md) | Barrier patterns (guard slot, setter, `GcBind`), running `zuqe-gc-audit` to green, a focused old-to-young check under minor GC, and `--gc-stress`                                                 |

### Bindings and extensions

| Topic                    | File                                                | When to read                                                                                                                                                                                                                   |
| ------------------------ | --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Native functions         | [native-function.md](references/native-function.md) | Expose a Rust function or callback to JS: signature, argument checks, throw handling, and the ways to install it                                                                                                               |
| Native classes           | [native-class.md](references/native-class.md)       | Define a host class with `def_native_object!`: payload and trace, constructor, methods and properties, creating instances, `extends`                                                                                           |
| Modules (ESM and native) | [native-module.md](references/native-module.md)     | Two module kinds: make `import` work in a host (loader/normalizer, import attributes, `import.meta`), and expose Rust values as a native module                                                                                |
| zapi extensions          | `zuqe-zapi/skills/SKILL.md`                         | Not a page in this skill: writing a zapi extension library needs only the `zuqe-zapi` crate (no engine code), so its authoring guide lives there — project setup, `#[zq_*]` macros, classes, async and thread jobs, GC tracing |

## Key rules

- **GC rules:** [GC Safety Rules](references/gc-safety.md) is the only rule source. Trace
  examples are in [gc-trace.md](references/gc-trace.md). Barrier and audit tools are in
  [gc-write-barrier.md](references/gc-write-barrier.md). Other files link to these rules
  and do not repeat them.
- **Payload shapes:** use `()` for zero-size or stateless payloads. Use
  `unsafe_empty_gc_trace!` when a non-zero payload has fields that do not
  implement `GcTrace`. Use `#[derive(GcTrace)]` when fields can be traced. Do
  not use an empty hand-written impl by default. Do not hide GC fields behind
  an empty trace macro; the audit checks this.
- **Project rules:** GC node safe. Clear zapi boundary. **Class inheritance has
  one path only.** Build root classes first. Host crates link a parent only with
  `def_native_object!(..., extends: Parent::class_id)` plus `use_class`.
  `ScopedContext::link_class_extends` and `init_class_proto_extends` are
  `pub(crate)` engine primitives for builtins — hosts cannot call them. Do not
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
2. Read the [workflow and checklist](references/workflow-checklist.md).
3. Read the topic file. Copy the template and adjust it to the real types and
   API. Do not guess field names.
4. If GC nodes are involved, read [GC Safety Rules](references/gc-safety.md) and follow
   rules 0-5.
5. Before you commit, run the static audit
   (`cargo test --test gc_audit`). Run focused dynamic checks with
   debug + `gc_generations`, and use `--gc-stress` when needed. Then recheck
   [workflow-checklist.md](references/workflow-checklist.md).
