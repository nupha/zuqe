# GC Safety Rules

> **Purpose:** native functions, modules, and classes may hold or write **GC
> nodes** on the Rust side. Those nodes must be safe under GC. This file gives
> the **rules (0-5)**, the failure modes, a **template**, and a pre-commit
> checklist.
>
> **This file is the only rule source.** Trace examples and pitfalls are in
> [GC Tracing](gc-trace.md). Barrier patterns and audit tools are in
> [Write Barriers](gc-write-barrier.md). This file keeps the rules and links.

## 1. GC safety has two parts

Both parts are required:

| Part | Question | Covers | Details |
|---|---|---|---|
| **GC node trace** | Can a full mark still find this value? | full mark / major | [GC Tracing](gc-trace.md) |
| **Node write barrier** | Can a minor see a young child written into an old container? | minor / remembered set (`gc_generations`) | [Write Barriers](gc-write-barrier.md) |

### 1.1 What a missing trace causes

The GC marks only from roots it knows. A `JSValue` or `Object` in a payload
field, a `thread_local!`, or an untraced Rust container is invisible.

- If it is the only reference, a full mark or major collects it.
- Rust keeps using it: **use-after-free**.
- Debug builds often show `gc: bad node`. Release builds may crash randomly,
  throw `TypeError`, corrupt callbacks, or crash at teardown.
- A short script may never run a full mark. "It ran" does not mean trace is
  correct.

### 1.2 What a missing write barrier causes

With `gc_generations`, a minor scans young nodes plus the remembered set. It
does **not descend old payloads**.

- An old owner writes a young child without recording the edge.
- The next minor does not see it and collects the young child.
- The owner still holds the reference: **dangling reference / UAF**.
- Debug builds panic after the minor: `— missing write barrier`. Release builds
  have a silent UAF.
- Common symptoms: promises or async work never settle, random `TypeError`,
  `gc: bad node`.

### 1.3 They cannot replace each other

- Trace only: a full mark sees the child, but a minor loses it.
- Barrier only: a minor sees it, but a full mark loses it.
- Incremental or concurrent marking needs barriers too. Every new edge created
  during marking must be visible to the collector.

## 2. GC rules (the only rule source)

> API usage, examples, pitfalls, and tools are in
> [GC Tracing](gc-trace.md) and [Write Barriers](gc-write-barrier.md). This
> section gives only the rules.

### Rule 0: every Rust-held node must be reachable

Every GC node held by Rust must be in the GC reachability graph. A payload
field, `thread_local!`, or untraced Rust container is unknown to the collector.

- A missing trace is fatal. When unsure, trace. Tracing a non-node is a
  harmless no-op.
- This covers host payloads, `thread_local!`, module records, and temporary
  values across JS calls.

Details: [GC Tracing](gc-trace.md).

### Rule 1: payloads that hold GC nodes implement `GcTrace`

- A payload or long-lived Rust struct with GC nodes must implement `GcTrace`
  and trace **every** GC field unconditionally.
- Use `#[derive(GcTrace)]` in most cases. For a crate that only depends on
  `zuqe`, add `#[gc(crate_path = zuqe::gc_lite)]`. Hand-write `impl GcTrace`
  only when derive cannot express the logic, and explain why in a comment.
- Do not default to an empty hand-written impl. Use `()` for zero-size
  payloads. Use `unsafe_empty_gc_trace!` for non-zero payloads whose fields do
  not implement `GcTrace`. Use derive when fields can be traced. See
  [GC Tracing](gc-trace.md).
- An `extends:` child does not share the parent payload or trace.
- Trace lazy caches (`RefCell<Option<Object>>`, `Vec<JSValue>`, cache maps) when
  they are reachable only from the payload.
- Use `#[gc(skip)]` for weak or untraced fields. Do not trace a weak target.
- **`GcTrace` is not a write barrier.** Late writes also follow Rule 5.

Details and examples: [GC Tracing](gc-trace.md).

### Rule 2: `thread_local!` holding GC nodes needs a user trace

- For each `thread_local!` that holds GC nodes, write a trace function that
  covers every GC field.
- Register it on each runtime or thread with `gc_set_trace_handler`.
- The registered user trace is a root. The root exemption does not extend to
  normal old containers or payloads.

Details and examples: [GC Tracing](gc-trace.md).

### Rule 3: raw pointers into `Vec` storage

- Do not cache pointers to elements of a growable `Vec`. Re-index after each
  change.
- If an element address must stay stable, use `Vec<Box<T>>`.

Details and pitfalls: [GC Tracing](gc-trace.md).

### Rule 4: values leaving a traced container before JS re-entry

- After `remove`, `pop`, or `take` from a traced container, put the value into
  an object-owned traced slot before any API that may run JS. Cover all exit
  paths, including `?`.
- Scope protection does not survive a safepoint. **Do not** use
  `gc_protect_transient` as a fix.
- Do not hold a `RefCell` borrow across JS. Do not use a global roots `Vec` with
  manual push and pop.
- Root-scope values live until the Context is dropped. Use `with_new_scope` for
  temporary work. Call `remove_local` when a root value is no longer needed.

Details and examples: [GC Tracing](gc-trace.md).

### Rule 5: an old container receiving a young value

- With `gc_generations`, an old node that receives a young value must record
  the old-to-young edge. Assume any mutable container may be old.
- Prefer **private fields, setters, and guard slots**
  (`WriteBarrier`/`WriteBarrierVec`/`WriteBarrierMap`). `set`, `push`, and
  `insert` store and bind in one step.
- The owner must be the node that holds the payload. There must be no GC point
  between store and bind.
- Container elements must implement `GcBind`. Skip weak and `#[gc(skip)]`
  fields.
- Do not replace a whole guard field (`field = WriteBarrier::new(...)` or
  `mem::replace`).
- Removal needs no barrier. The removed value loses trace coverage. If it is
  used across a GC point, follow Rule 4.
- Writes already covered by the engine (property slots, arrays, Map/Set,
  closure VarRef, `replace_data`, JIT, root containers) need no duplicate
  barrier. **Payload fields are not covered.**
- Incremental or concurrent marking is stricter. Record every new edge during
  marking.

Details, API, and tools: [Write Barriers](gc-write-barrier.md).

> The GC check flow (static audit, dynamic minor, gc stress) is in
> [Write Barriers](gc-write-barrier.md). The full embedding workflow is in
> [Workflow and Checklist](workflow-checklist.md).

## 3. Best-practice template

> Start from this shape, then adjust to the real types and API. More trace and
> barrier examples are in [GC Tracing](gc-trace.md) and
> [Write Barriers](gc-write-barrier.md).

```text
my-crate/
├── Cargo.toml
├── gc_audit_exemption.txt      # only exemption source; optional if empty
├── src/
│   └── foo.rs                  # payload, GcTrace, guard slots, setters
└── tests/
    └── gc_audit.rs             # zuqe::gc_audit_test!(<root floor>);
```

`src/foo.rs`:

```rust
use std::ptr::NonNull;

use zuqe::{
    GcBind, JSValue, Object, ScopedContext, WriteBarrier, WriteBarrierMap, WriteBarrierVec,
    def_native_object,
    gc_lite::{GcHead, GcTrace},
};

/// Container element type: a local struct. Tuples cannot impl external GcBind.
#[derive(Debug, GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct Req {
    once: bool,
    cb: Object,
}

impl GcBind for Req {
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        GcBind::bind_node(&self.cb, ctx, owner);
        // Skip weak references. Match #[gc(skip)] and GcTrace behavior.
    }
}

#[derive(Debug, GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
pub(crate) struct FooPayload {
    // full mark: derive expands trace by field
    result: WriteBarrier<JSValue>,
    handler: WriteBarrier<Option<Object>>,
    // minor: the guard slot stores and binds; element type impls GcBind
    queue: WriteBarrierVec<Req>,
    cache: WriteBarrierMap<u64, Object>,
}

// GcTrace comes from derive, so new fields are covered. Hand-write impl only
// for special trace logic. If FooPayload has no GC nodes and fields do not
// implement GcTrace, use unsafe_empty_gc_trace!(FooPayload). If it has no
// state, use the () no-payload form. Do not write an empty impl GcTrace.

impl FooPayload {
    // Expose setters only. The caller passes the owner (the instance).
    pub(crate) fn set_result(&mut self, ctx: &ScopedContext, owner: JSValue, v: JSValue) {
        self.result.set(ctx, owner, v);
    }

    pub(crate) fn set_handler(&mut self, ctx: &ScopedContext, owner: JSValue, v: Option<Object>) {
        self.handler.set(ctx, owner, v);
    }

    pub(crate) fn push_req(&mut self, ctx: &ScopedContext, owner: JSValue, r: Req) {
        self.queue.push(ctx, owner, r);
    }

    pub(crate) fn cache_put(&mut self, ctx: &ScopedContext, owner: JSValue, k: u64, v: Object) {
        self.cache.insert(ctx, owner, k, v);
    }
}

def_native_object!(
    "Foo",
    JSFoo,
    FooPayload,
    FOO_PROTO, // proto/ctor in native-class.md
    Some(ClassCtorDef::new(js_foo_constructor, 1, false)),
);
```

Call site:

```rust
let owner = this; // capture owner before borrowing the payload
let p = JSFoo::payload_mut_of(&mut this).ok_or(-1)?;
p.set_handler(ctx, owner, Some(cb)); // setter stores and binds
```

`tests/gc_audit.rs`:

```rust
//! Static GC audit: bare payload fields, missing GcBind impls,
//! whole-field guard replacement, stale exemptions.
zuqe::gc_audit_test!(10);
```

Order of work:

1. Design fields per Rules 0-5: GC fields need trace; possible late writes need
   guard slots and setters.
2. Implement `GcTrace` and setters. Container elements implement `GcBind`.
3. Add `tests/gc_audit.rs` and `gc_audit_exemption.txt`. Run
   `cargo test --test gc_audit` until it passes.
4. In debug + `gc_generations`, run a focused old-to-young check. Use
   `--gc-stress` when needed.
5. Recheck [Workflow and Checklist](workflow-checklist.md).

## 4. Pre-commit checklist

> Check each Rule 0-5 item. These entries point to the details and do not repeat
> the rules.

- [ ] Rule 0-2: every GC field has a reachability plan (payload, thread-local,
      or cross-JS). Examples are in [GC Tracing](gc-trace.md).
- [ ] Payload shape is correct when there are no GC nodes: use `()` for
      zero-size; use `unsafe_empty_gc_trace!` for non-zero fields without
      `GcTrace`. Do not hide fields behind a hand-written empty impl. The audit
      checks the macro type. See [GC Tracing](gc-trace.md).
- [ ] Rule 4: values across JS have a traced slot; no `gc_protect_transient` or
      scope dependency; no `RefCell` borrow across JS. See
      [GC Tracing](gc-trace.md).
- [ ] Rule 5: late-write fields use guard slots and setters; owner is correct;
      no whole-field replacement; elements implement `GcBind`. See
      [Write Barriers](gc-write-barrier.md).
- [ ] Static audit: `cargo test --test gc_audit` passes. Exemptions have
      reasons and none are stale. See [Write Barriers](gc-write-barrier.md).
- [ ] Dynamic check: a focused old-to-young path runs a minor, and the focused
      case runs with `--gc-stress`. See [Write Barriers](gc-write-barrier.md).
- [ ] Review: recheck [Workflow and Checklist](workflow-checklist.md).
