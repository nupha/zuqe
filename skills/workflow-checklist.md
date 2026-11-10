# zuqe Embedding Workflow and Review Checklist

> This is the global workflow for zuqe embedding work. It covers function
> binding, native classes, native modules, GC, audit, and review. It is not a GC
> only document. The only GC rule source is [GC Safety Rules](gc-safety.md).
> Trace examples are in [GC Tracing](gc-trace.md). Barrier, audit, minor, and
> stress tools are in [Write Barriers](gc-write-barrier.md).

## Standard workflow

1. **Clarify the requirement**
   - What ability do you expose (function, class, module, event, lifecycle)?
     What is the JS API shape? Sync or async? What are the error semantics?
   - Does it hold GC nodes, call JS, or use long-lived caches or
     `thread_local!`?
   - Does it need an ESM loader, a native module, or a zapi extension? Is the
     entry point `EvalName::BaseUrl`?
2. **Choose the path first**
   - **Host or engine-builtin Rust** uses `ScopedContext`, `CallContext`,
     `JSResult`, `CallbackFn`, `def_native_object!`, `FieldDef`, and
     `create_module`. This is for embedded hosts and builtin features.
   - **zapi cdylib extensions** use `zuqe-zapi`, `ZqApi`, `ZqBodyFn`,
     `ZqFieldDef`, and `#[zq_module]` (which generates `zq_module_register`).
     This is for loadable plugins.
   - **ESM modules** are the host loader and normalizer's job, not a native
     module. See [Modules: ESM Loading and Native Modules](native-module.md).
   - **Do not mix** the two styles, types, or registration paths. Record the
     choice in the implementation notes or PR.
3. **Implement**
   - Entry, Runtime, Promise: [Embedding Basics](embedding-basics.md) and
     [Promise and Rust Future Bridge](promise.md)
   - Function binding: [Native Function Binding](native-function.md)
   - Native class: [Native Classes](native-class.md)
   - Modules (ESM and native):
     [Modules: ESM Loading and Native Modules](native-module.md)
   - zapi extension: [Writing a zapi Extension](zapi-extension.md)
   - JIT setup: [Tiered JIT Usage](jit.md)
   - Think about GC early: payload fields, thread-local, cross-JS holds, and
     late-write paths.
4. **GC self-check**
   - Read [GC Safety Rules](gc-safety.md) first. Trace examples are in
     [GC Tracing](gc-trace.md). Barriers are in
     [Write Barriers](gc-write-barrier.md).
   - Add `GcTrace`. With no GC nodes, use `()` for zero-size payloads and
     `unsafe_empty_gc_trace!` for non-zero payloads.
   - Handle Rule 4 across JS. Add guard slots and setters. Check the owner and
     avoid whole-field replacement.
5. **Static GC audit**
   - Standard layout: `tests/gc_audit.rs` plus `gc_audit_exemption.txt`, using
     `zuqe::gc_audit_test!(min_roots)`. See
     [Write Barriers](gc-write-barrier.md).
   - Run `cargo test --test gc_audit` in the target crate. Use
     `zuqe-gc-audit <src>` when you need the report. See
     [Write Barriers](gc-write-barrier.md).
   - New exemptions must be short, honest, and correctly categorized. Fix stale
     entries. See [Write Barriers](gc-write-barrier.md).
6. **Dynamic GC check**
   - In debug with `gc_generations`, promote the owner, write, then call
     `gc_minor_now()`. A missing barrier should report `missing write barrier`
     from `dbg_assert_no_stale_old_edges`.
   - Run a focused case with `--gc-stress` (or `gc_set_stress(true)` on a small
     script). See [Write Barriers](gc-write-barrier.md).
   - Add or run the relevant unit tests, integration tests, and examples.
7. **Review**
   - Focus on mixed boundaries, inheritance linking, GC shape and timing,
     errors, `this` checks, and exemption reasons. Walk the
     [checklist](#review-checklist).

## Review checklist

### A. Path and boundary

- [ ] The audience is clear: host/builtin Rust **or** a zapi cdylib. No mixing
      of `ScopedContext`/`FieldDef`/`def_native_object!` with `Zq*` ABI types.
- [ ] Native module, function, and class registration matches the chosen path
      (`create_module`/`FieldDef` vs zapi list/host loading).
- [ ] ESM modules and native modules are not mixed. ESM uses a loader and
      normalizer. Native uses `create_module`.
- [ ] Builtin-only mechanisms (InitFn, `pub(crate)` primitives) are not exposed
      as third-party APIs.

### A2. Runtime and entry

- [ ] `Runtime` and `Context` are created in the right order. A Context is not
      sent across threads or runtimes.
- [ ] With default `async_support`, use `Context::eval_wait(...).await`. Do not
      pass `&mut Runtime`.
- [ ] Module entries use `EvalName::BaseUrl(path_to_url(...)?)`. Plain scripts
      may use `EvalName::Name`.
- [ ] Promise results use `Promise::try_from` and `into_future`, driven with
      `run_async` or `eval_wait`.
- [ ] Temporary values use `with_new_scope`. Root-scope values use
      `remove_local` when no longer needed.
- [ ] `set_vm_stack_arena` errors are handled. `set_vm_stack_limit >= arena`.

### A3. Modules and Promise

- [ ] Without a loader, imports throw `ReferenceError`. The host either sets a
      loader or keeps this default on purpose.
- [ ] The normalizer and loader are plain `fn` pointers. Host state goes in
      `RuntimeImpl::user_state`.
- [ ] Import attributes (for example JSON) are handled in the loader.
      `compile_module` gets the correct URL.
- [ ] Promise channel/slot `convert` runs on the JS thread. Cross-thread data is
      plain Rust data with no GC handles.
- [ ] Unobserved Promises use `*_hold_when_awaited` only when they must keep the
      process alive.

### B. Native function binding

- [ ] Callback signature is `fn(&mut ScopedContext, CallContext) -> JSResult`.
      `length` and `magic` are correct.
- [ ] Argument reads check bounds and types. On failure, use `throw_*` and
      `Err(-1)`. Do not return a fake success.
- [ ] `this` is checked when needed, especially in native class methods.
- [ ] Property and export visibility flags match the intended semantics.

### C. Native classes and inheritance

- [ ] Uses the standard `def_native_object!` shape. The
      `#[repr(transparent)]` wrapper does not hand-implement `NativeObject`.
- [ ] Payload access uses generated safe methods. The class id is checked.
- [ ] Zero-size or stateless payloads use `()`; non-zero no-GC payloads use
      `unsafe_empty_gc_trace!`; there is no hand-written empty `impl GcTrace`.
      A macro type must not contain GC nodes (the audit checks this).
- [ ] `use_class` or the init path is correct before use in a context.
- [ ] **Inheritance follows the one allowed path**:
  - [ ] Host classes use `def_native_object!(..., extends: Parent::class_id)` as
        supported by the engine.
  - [ ] No hand-written `set_prototype` for class prototype or constructor
        inheritance.
  - [ ] Builtins reuse `link_class_extends`, `init_class_proto_extends`,
        `bind_ctor_proto*`, and related primitives. Do not invent another link
        path.
  - [ ] InitFn is not exposed to third parties.
- [ ] An `extends:` child has its own payload and `GcTrace`. It does not share
      the parent payload type.

### D. Native modules and ESM

- [ ] `create_module(name, &'static [FieldDef])` is used. The export table is
      `'static`. New code does not use deprecated `register_module`.
- [ ] The loader and normalizer are plain `fn` pointers. Host state goes through
      `user_state` or another explicit mechanism.
- [ ] Module names and resolution match the host integration. The zapi loader
      checks the extension suffix and calls `load_zapi_extension`.
- [ ] Module entries use `EvalName::BaseUrl`. `import.meta.url` and relative
      `import()` resolve as expected.
- [ ] Import attributes are handled in the loader. JSON wrapping is correct.

### E. GC rules

> All GC rules are in [GC Safety Rules](gc-safety.md), rules 0-5. This file does
> not repeat them. Trace examples and pitfalls are in
> [GC Tracing](gc-trace.md). Barrier tools are in
> [Write Barriers](gc-write-barrier.md).

- [ ] Walk rules 0-5 in `gc-safety.md`. See `gc-trace.md` and
      `gc-write-barrier.md` for examples and pitfalls.
- [ ] Trace side: check the examples and pitfalls in `gc-trace.md`. No-GC
      payloads use `()` or `unsafe_empty_gc_trace!`, and the macro type has no
      GC fields.
- [ ] Barrier side: run the static audit, dynamic minor, and gc stress flow in
      `gc-write-barrier.md`.
- [ ] Review finds no pitfall from `gc-trace.md` or `gc-write-barrier.md`, and
      does not suggest their wrong fixes.

### F. GC audit and checks

- [ ] `tests/gc_audit.rs` and `gc_audit_exemption.txt` (if needed) follow the
      standard layout in [Write Barriers](gc-write-barrier.md).
      `zuqe::gc_audit_test!(min_roots)` is reasonable.
- [ ] `cargo test --test gc_audit` passes. No stale exemptions. No
      `unsafe_empty_gc_trace!` type contains GC nodes. See
      [Write Barriers](gc-write-barrier.md).
- [ ] Exemption categories and reasons follow
      [Write Barriers](gc-write-barrier.md).
- [ ] New old-to-young paths have a focused debug + `gc_generations` check.
      Focused cases ran under [gc stress](gc-write-barrier.md) when possible.
- [ ] Limits are understood: audit and validator are not a complete timing
      proof. Release correctness relies on guards, setters, and review. See
      [Write Barriers](gc-write-barrier.md).

## Deliverable self-check

- [ ] Covers function binding, native classes, native modules, GC trace,
      `WriteBarrier`, static audit, dynamic stress, workflow, and checklist.
- [ ] Templates are actionable. API and macro names match current zuqe patterns.
      Unclear points cite source paths.
- [ ] Key source paths are cited. GC rules and topics are complete. Host/builtin
      and zapi third-party constraints are separated.
- [ ] GC semantics are correct, with no wrong advice.
