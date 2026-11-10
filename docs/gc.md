# Garbage Collection

Zuqe uses tracing mark-sweep GC on top of Rust's ownership system: JS objects live on the GC
heap, while native code holds handles to them. The two requirements that follow from that split —
**report every edge** so a full mark can find the object, and **record every old→young write** so
a minor can find it — are the core invariants of the collector.

This page documents the **engine implementation**: object model, scopes, roots, generations, the
write-barrier primitive, guard-slot representation, the runtime validator, and zapi payload
handling. It is for changing the engine.

If you are writing a host crate that depends on `zuqe` — native payloads, `GcTrace`, guard slots,
`GcBind`, or the static GC audit — read the user-facing rules in
**[GC Safety Rules](../skills/gc-safety.md)** first. That file is the single source of truth
for rules 0-5; implementation examples live in
**[GC Tracing: Examples and Pitfalls](../skills/gc-trace.md)** and
**[Write Barriers: Patterns and Tools](../skills/gc-write-barrier.md)**. This page does not repeat them.

---

# Part 1 — Engine internals

## 1. Object model

A type is a **GC node** if it implements `gc_lite::GcNode` (it lives on the GC heap and is
markable/reclaimable) or if it wraps or contains a `GcRef<GcNode>`.

Tracing is split in two halves that must agree:

- `gc_lite::GcTrace` — reports every edge this value contributes. Used by a **full mark**.
- `zuqe::GcBind` — records the same edges into the **remembered set** for a **minor**.

`JSValue::trace` no-ops on non-node tags (Int/Bool/Double), so over-tracing is safe and
under-tracing is fatal. The collector only sees what these two report — it has no other way to
reach a node.

The runtime itself does not hold Rust references into the heap. Everything it owns is a handle
(`JSValue`, `Object`, `Promise`, a `def_native_object!` instance, …) that the GC can trace.

## 2. Scopes

`ScopedContext` is a **GC scope**: a lightweight, nestable sub-region inside a `Context`. Scopes
form a stack — each `with_new_scope` pushes a child on top of the current one. The public entry to
the root scope is `Context::with_root_scope()`; temporary work belongs in child scopes created
with `ScopedContext::with_new_scope()`.

### Why scopes

Scope-local allocations are **temporarily protected**: the GC treats them as roots for the
lifetime of the scope, so short-lived work needs neither Rust references nor `GcRef` handles:

```rust
ctx.with_root_scope(|mut root| {
    root.with_new_scope(|scope| {
        let mut arr = Array::create(scope)?; // protected by this scope
        arr.define_property_value_u32(scope, 0, JSValue::from(42), JS_PROP_C_W_E)?;
        // arr is released automatically when this scope closes
        Ok(JSValue::UNDEFINED)
    })
});
```

### Scope stack and promotion

When the top-most scope exits, its **return value** is promoted to the parent scope's protection —
any GC nodes embedded in it stay safe. All other scope-local nodes lose their temporary
protection and become eligible for collection (they are freed only if nothing else references
them):

```rust
fn compute_result(ctx: &mut Context) -> Result<JSValue, i32> {
    ctx.with_root_scope(|mut root| {
        root.with_new_scope(|scope| {
            let tmp1 = Array::create(scope)?;   // local, protected only in this scope
            let tmp2 = Array::create(scope)?;

            let result =
                Array::from_values(scope, vec![JSValue::from(1), JSValue::from(2)])?;
            Ok(result.into())
        }) // tmp1/tmp2 lose protection; `result` is promoted to the root scope
    })
}
```

### Nested scopes

```rust
ctx.with_root_scope(|mut root| {
    root.with_new_scope(|outer| {
        let outer_val = JSValue::from(1);

        outer.with_new_scope(|inner| {
            let inner_val = JSValue::from(2);
            // both are protected here
            let _ = inner_val;
        }); // inner_val loses protection; outer_val is still protected

        outer_val
    }) // outer_val promoted to the root scope
});
```

### `with_root_scope`

The root scope (depth 1) lives for the entire lifetime of the `Context`. Anything promoted into
it is **permanently reachable**: never collected until the `Context` is dropped.

> **Notice:** a value placed in the root scope **persists forever**, even if nothing references it —
> the root scope is a permanent root set. If a root-scope value is no longer needed you **must**
> call `scope.remove_local(node)` yourself; otherwise it leaks for the life of the `Context`.

For long-running hosts, avoid accumulating values in the root scope. Use `with_new_scope` for
temporary work so intermediates are released when the child scope exits.

### Returning values across a scope boundary

Return the value as the closure's final expression — no special call is needed. `with_new_scope`
invokes `promote()` on the returned value before destroying the child scope, so the GC nodes it
embeds move into the parent scope's cache:

```rust
let obj = ctx.with_root_scope(|mut root| {
    root.with_new_scope(|scope| {
        let obj = JSValue::from(scope.create_base_object()?);
        Ok(obj) // promoted to the root scope automatically
    })
})?;
```

Promotion works through the `ScopeResult` trait, implemented for `JSValue`, `Result<T, E>`,
`Option<T>` and the common scalar/no-op types (`()`, `i32`, `bool`, `String`, …). `with_root_scope`
itself does not promote (its result stays where the caller puts it), so it can return any type.

## 3. What the collector treats as roots

A mark starts from:

- **scopes** (see above) — but note that the executor calls `clear_locals()` on the current scope
  *before* `gc_collect()`, so scope protection is dropped exactly when the GC runs;
- **runtime roots**: pending jobs, the shape registry, prototype/constructor slots, module and
  bytecode tables;
- **user trace callbacks** registered with `gc_set_trace_handler` (Rule 2 in
  [GC Safety Rules](../skills/gc-safety.md));
- **root containers** (the context itself, the shape registry, jobs, user state): a minor
  force-descends these, so writes into them need no barrier.

Ordinary old containers are **not** descended by a minor — that is the whole point of the write
barrier.

## 4. Generations

`gc_generations` is a build feature (on by default in zuqe's default feature set). Without it the
collector keeps the single-generation path; with it, each partition is split into a young chain
and an old chain.

- A fresh allocation is **young** (age 0) and every minor sweeps it unless marked.
- A young node that survives `promote_after` minors is promoted to **old**. The default is **1 in
  debug builds** (the shorter young phase surfaces a missing old→young barrier sooner) and **2 in
  release builds**. Promotion is **transitive along young references**: when a node becomes old,
  every young node it reaches is promoted too, recursively. Those edges were created while the
  parent was young, so no write barrier recorded them; leaving the children young would create an
  unremembered old→young edge that the next minor cannot see.
- A **major** merges the whole young chain first: every live node is old afterwards.
- A minor never sweeps old nodes; old garbage waits for a major.
- A minor marks the young chain (age>0 survivors are roots) plus the **remembered set** of old
  nodes that recorded an old→young write. It does not descend into an old node's payload.

Minors run automatically when young allocations exceed the young budget. `gc_minor_request()`
only *requests* one: it runs at the next safepoint, so a caller must execute a few more bytecodes.
`gc_minor_now()` runs one immediately (debug builds only; exposed to host scripts as
`Aike.gcMinor`). `gc_set_promote_after(0)` disables minors (major-only) while keeping the
bookkeeping; `gc_set_promote_after` accepts 0/1/2 (larger values clamp to 2).

Consequence: a quick script passing proves nothing about barriers. The owner is often already old
(the entry module, a long-lived stream controller, a cache created at startup) and a major makes
everything old, so assume any mutated container can be old and record the edge.

## 5. The write-barrier primitive

There is exactly one primitive: `GcHeap::bind`. It updates the generational remembered set and the
tri-color / cross-partition bookkeeping, and it **never allocates a GC node and never collects**.
That is why a store and its `bind` may not be separated by a GC point — the pair is meant to be one
indivisible step.

| entry point | visibility | use |
|---|---|---|
| `ScopedContext::bind_value(owner, child)` | public | host code / external crates |
| `ContextImpl::bind_node_value` / `bind_node` | crate-internal | code that already holds `NonNull<GcHead>` |
| `RuntimeImpl::bind_node`, `RuntimePtr::bind_node` | crate-internal | JIT / interpreter paths |
| `GcHeap::bind` | crate-internal | GC finalize paths, where no context exists |

The guard slots described next all funnel into `bind`; they exist so that forgetting the call is a
compile error rather than a review item.

**Owner.** `owner` must be the node that *holds* the payload — for a `def_native_object!`
instance that is the instance itself. Passing a foreign owner records the edge on the wrong node:
silent corruption, caught only by the debug assert in `GcHeap::bind` and by review.

**Removal is free.** `remove`/`pop`/`clear`/`retain`/`take` need no barrier — but the removed
value loses `trace` coverage, so if it is still used after the next GC point it must be held in a
traced slot (Rule 4 in [GC Safety Rules](../skills/gc-safety.md)).

## 6. Guard slots and their representation

Three payload-slot types make a bare write a compile error:

| type | for | mutating API |
|---|---|---|
| `WriteBarrier<T>` | one GC leaf | `set(ctx, owner, v)` |
| `WriteBarrierVec<T>` | a `Vec` of elements | `push(ctx, owner, v)`, `insert(ctx, owner, i, v)` |
| `WriteBarrierMap<K, V>` | a `HashMap` | `insert(ctx, owner, k, v)` |

They are `#[repr(transparent)]` with **no runtime state**: layout, size and `offset_of` are
identical to the bare type, so JIT/host code that reads payload fields by offset is unaffected.
Never add a field-level flag to them — that would break transparency.

Construction versus mutation:

- `WriteBarrier::new(v)`, `WriteBarrierVec::new()` / `with_capacity(n)` / `from_vec(v)`,
  `WriteBarrierMap::new()` are **initializing stores**. They are legal only while the owning
  instance does not exist yet (payload construction): the instance is born young, so there is no
  edge to record.
- After the instance exists, every write goes through `set` / `push` / `insert`, which store and
  bind in one call.

Deliberately withheld:

- **No `Clone`** on any guard — copying would fabricate unbarriered edges. `WriteBarrierVec` and
  `WriteBarrierMap` do implement `Default` (empty container), `WriteBarrier` does not.
- **No `DerefMut`, no index assignment, no `iter_mut`** — `queue.push(v)` and `queue[i] = v` do not
  compile. Reads go through `Deref<Target = [T]>` / `Deref<Target = HashMap<K, V>>` or `get()`.
- **`WriteBarrierVec::first_mut()`** is the single hole in the API. It hands out `&mut T`, so it is
  sound only when every GC leaf inside `T` is itself a guard slot; otherwise a caller can assign a
  bare edge. Prefer rebuilding the element through `remove` + `push` where that is natural.
- **Whole-field replacement still compiles** (`p.slot = WriteBarrier::new(v)`,
  `mem::replace(&mut p.slot, …)`). Rust cannot forbid it, so the static audit rejects it
  (see [Write Barriers: Patterns and Tools](../skills/gc-write-barrier.md)).

Guard slots cover the **minor**: `set`/`push`/`insert` call `bind` on the stored value, and a
container element supplies its own edges through `GcBind`. They do **not** replace `GcTrace`: a
bound but untraced field is still lost on the next full mark.

## 7. The generational validator

With `debug_assertions` + `gc_generations`, every minor is followed by
`GcHeap::dbg_assert_no_stale_old_edges`. It walks **every old node** — reachable or not, all
partitions — and traces its current edges. An edge pointing at a node the minor just collected
panics with:

```
a live object still points at a node freed by the last minor:
child 0x… (freed dtype …), owner 0x… (dtype …) — missing write barrier
```

`gc_set_stress(true)` (debug + `gc_generations` only) makes every GC-node allocation request a
minor, so the validator sees nearly every old→young write. Its cost is allocations × heap size;
use it on small, targeted scripts.

Known limits — do not read a green run as proof:

- it only sees edges that `trace` reports;
- it only sees paths that actually run a minor;
- it only exists in debug builds; release correctness rests on the guard slots and review;
- a minor still executes only at a safepoint, so native-only paths must request one themselves.

Separately, weak-reference-heavy workloads can currently trip a pre-existing double-free in
`ObjectImpl::finalize` → `JSMapState::remove_weak_obj`. It is unrelated to the barrier rules and
is not caught by the validator.

## 8. zapi payloads

A zapi extension has no ABI entry point for a write barrier. The engine compensates by re-tracing
the payload of every **old** zapi instance on each minor, through the class `gc_trace` callback.
That is a fallback, not a substitute: a `gc_trace` callback must still report every GC field it
holds, because the re-scan can only see what it reports.

---

# Embedding boundary

The public rules are in the skill docs, not here:

- **[GC Safety Rules](../skills/gc-safety.md)** — what a host
  payload must do: trace every GC node, register `thread_local!` traces, keep values alive across
  JS re-entry, and record old→young writes.
- **[GC Tracing: Examples and Pitfalls](../skills/gc-trace.md)** — `#[derive(GcTrace)]`, `#[gc(skip)]`,
  `#[gc(with = ...)]`, `unsafe_empty_gc_trace!`, and counter-examples.
- **[Write Barriers: Patterns and Tools](../skills/gc-write-barrier.md)** — `WriteBarrier`,
  `WriteBarrierVec`, `WriteBarrierMap`, `GcBind`, `zuqe-gc-audit`, dynamic minor verification,
  and `--gc-stress`.
- **[Workflow and Review Checklist](../skills/workflow-checklist.md)** — the end-to-end review list.

Two invariants connect the two halves:

1. **Full mark** needs `GcTrace` on every Rust-held GC node (payload fields, `thread_local!`,
   module records, cross-JS temporaries).
2. **Minor** needs a write barrier on every old→young write. Guard slots (`WriteBarrier*`) and
   setter conventions exist so forgetting the barrier is a compile error where possible.

The engine keeps the guard-slot primitives, the static audit, and the generational validator in
`src/gc/`; hosts should consume them through the public types and the `zuqe_audit_test!` macro
described in the skill docs.
