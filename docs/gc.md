# Garbage Collection

Zuqe uses tracing mark-sweep GC on top of Rust's ownership system: JS objects live on the GC
heap, while native code holds handles to them. The two requirements that follow from that split —
**report every edge** so a full mark can find the object, and **record every old→young write** so
a minor can find it — are what this document is about.

It is organized for two audiences:

- **Part 1 — engine internals.** How the collector is built: object model, scopes, roots,
  generations, the write-barrier primitive, the guard-slot representation, and the runtime
  validator. Read this when changing the engine.
- **Part 2 — embedding zuqe.** What a crate that depends on `zuqe` must do: `GcTrace` on payloads,
  typed guard slots for late writes, `GcBind` on container elements, and the static GC audit that
  keeps the rules honest. Ends with a best-practice template you can copy.

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
- **user trace callbacks** registered with `gc_set_trace_handler` (Part 2, Rule 2);
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
traced slot (Part 2, Rule 4).

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
  `mem::replace(&mut p.slot, …)`). Rust cannot forbid it, so the static audit rejects it (Part 2).

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

# Part 2 — Embedding zuqe

## 9. The golden rule, and what counts as a GC node

> **Any GC node held on the Rust side must be part of the GC reachability graph.**
> The collector only sees nodes reachable from its roots. A `JSValue`/`Object` stashed in a native
> class payload or a `thread_local!` static is invisible to it. If it is reclaimed while you still
> use it, that is a **use-after-free** — a debug build panics with `gc: bad node`.
>
> Missing a `trace` is fatal; calling `trace` on a non-node is a harmless no-op. **When in doubt,
> trace it.**
>
> With `gc_generations` there is a second, separate requirement: `trace` decides what survives a
> *full* mark, and a write of a young node into an **old** container must also record the edge with
> a write barrier. `trace` without a barrier loses the child on the next minor; a barrier without
> `trace` loses it on the next full mark.

Common **GC node** types:

- `Object` and its transparent wrappers: `Promise`, `Array`, `TypedArray`/`Uint8Array`, `Map`,
  `Set`, `RegExp`, `Iterator`, `Function`, `Proxy`, `WeakRef`, `ArrayBuffer`, and native class
  instances created by `def_native_object!` (`JSFile`, `JSReadableStream`, `JSResponse`, …).
- `JSValue` — always trace it: it may hold a node (Object, GC-heap String, Symbol,
  FunctionBytecode, Module) or an inline value (Int/Bool/Double).
- Callback handles (JS functions / resolvers held as `Object`).

**Not** GC nodes (pure Rust data, no tracing needed): `String`, `Vec<u8>`, numbers, fds, channels,
`Arc<Mutex<…>>`, `serde_json::Value`, and FFI wrappers such as `LlamaModel`/`LlamaContext`.

Rules 1 and 2 cover **long-lived** Rust-held nodes. A value held only **during a native call** and
used after JS runs is covered by Rule 4. A node not covered by one of these rules is unsafe.

## 10. Rule 1 — payloads holding GC nodes implement `GcTrace`

When a `def_native_object!` payload stores GC nodes, the payload type **must** implement
`gc_lite::GcTrace`. The macro wires the payload's trace into the class's `on_gc_trace` at
registration, so the collector traces every field whenever an instance is marked. A payload type
that is not `GcTrace` fails to compile at the macro call — there is no way to register an untraced
payload.

The impl must report **every** GC field, unconditionally:

```rust
use zuqe::gc_lite::{GcTrace, GcTraceCtx};

impl GcTrace for FooPayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.field.trace(gcx);  // JSValue / Object / native wrappers all impl GcTrace
        for e in self.elements.iter() {
            e.cb.trace(gcx);
        }
    }
}

def_native_object!(
    "Foo",
    JSFoo,
    FooPayload,
    FOO_PROTO,
    Some(ClassCtorDef::new(js_foo_constructor, 1, true)),
);
```

`#[derive(GcTrace)]` does the same for you, with `#[gc(skip)]` / `#[gc(with)]` for fine-grained
control — but it expands to absolute `::gc_lite::…` paths, so it needs `gc-lite` as a **direct**
dependency of your crate. A crate that depends only on `zuqe` writes the impl by hand; that is
what the template in section 17 does.

A payload that holds **no** GC nodes still needs an impl — write it empty:

```rust
impl GcTrace for Payload {
    fn trace(&self, _: &mut GcTraceCtx) {}
}
```

Pitfalls:

1. **`extends:` subclasses do not share the parent's payload type.** GC looks the trace callback up
   by the object's own `class_id`, and `new_instance` creates objects with `Self::class_id()`. Each
   class carries its own payload type and therefore its own `GcTrace` impl.
2. **Lazily-cached JS objects are GC nodes too.** A `RefCell<Option<Object>>` cached in the payload
   (a URL's `searchParams`, a parsed result, a namespace object) must be traced once it is only
   reachable from that payload.
3. **`GcTrace` is not a write barrier.** It keeps the payload's children alive across a *full*
   mark. With `gc_generations`, writing a young value into a payload whose instance may already be
   old also needs a barrier (Rule 5). Payloads have no automatic re-trace fallback, so route such
   writes through a setter that binds.

## 11. Rule 2 — `thread_local!` state: register a user trace

`thread_local!` statics are not in the GC's reachability graph. A GC node stored there must be
explicitly rooted by a trace callback, or it is collected while the thread-local still points at it.

```rust
thread_local! {
    static MY_STATE: RefCell<HashMap<u64, JSValue>> = const { RefCell::new(HashMap::new()) };
}

// Walks the state and traces every node it holds.
fn gc_trace_my_state(gcx: &mut GcTraceCtx) {
    MY_STATE.with_borrow(|map| {
        for v in map.values() {
            v.trace(gcx);
        }
    });
}
```

Register it on every runtime/thread that may touch the state:

```rust
fn main() -> ExitCode {
    let mut rt = Runtime::create().unwrap();

    rt.gc_set_trace_handler(|gcx| {
        gc_trace_my_state(gcx);
        // one entry per thread_local that holds GC nodes
    });

    let mut ctx = rt
        .create_context(|cx| ScopedContext::ECMA_CONTEXT_SETUP(cx))
        .unwrap();

    // …
}
```

Key points:

- one trace fn per thread-local, and it **must be registered**; a missing one means the nodes can be
  collected at any moment;
- the fn must cover **all** GC-node fields — trace every `JSValue` unconditionally;
- `gc_set_trace_handler` is per-thread / per-runtime;
- a registered user trace is a GC **root**, and a minor force-descends roots, so values reachable
  only from a registered thread-local need no write barrier. That exemption does **not** extend to
  ordinary old containers or to payloads.

## 12. Rule 3 — raw pointers into `Vec` storage

This is not a tracing problem, and it is rare: safe Rust's borrow checker rejects most of it. It
arises only where `unsafe` raw pointers bypass borrows.

A `Vec` relocates **all** its elements when it grows. A raw pointer into an element
(`&mut v[i].field as *mut _`, `v.as_ptr()`, a cached `NonNull`, …) dangles as soon as any `push`
grows the vector, even though the `Vec` itself is alive. The failure mode is not "a node was
collected" but live memory being stomped through the dangling pointer — which corrupts whatever
reuses that memory, often GC nodes, so the symptoms surface far away from the cause (bad-node
panics, recycled-callback `TypeError`s, teardown bounds aborts).

Prevention, only where raw pointers are involved:

- never cache an element pointer of a growable `Vec` across a call that may `push` to it — re-derive
  it from the index after every mutation;
- if element-address stability is genuinely required, store `Vec<Box<T>>`.

Distant, shape-shifting corruption is the signature of this class; a sanitizer build reports the
first offending access with both the use and free stacks.

## 13. Rule 4 — values taken out of a traced container, used across JS re-entry

The most common use-after-free in native code, and the one scope protection does **not** cover:

```rust
let chunk = queue.borrow_mut().remove(0);  // 1. leaves the traced queue
call_js_that_may_gc(ctx)?;                 // 2. runs bytecode, may GC
use(chunk);                                // 3. chunk may already be freed
```

Between steps 1 and 3 the only reference is a Rust local. Scope protection does not help: at a GC
safepoint the executor calls `clear_locals()` on the current scope **before** `gc_collect()`, so
scope-local protection is dropped exactly when the GC runs. `gc_protect_transient` is a no-op
unless the `gc_protect_transient` feature is on, and it still relies on scope protection — do not
use it as a fix.

"Calls JS" includes anything that can run bytecode: `call`, `invoke_*`, a `pull`/`cancel`/`then`
callback, a thenable check on a value you resolve with, and a getter that runs on `[[Get]]`.

**Fix:** keep the value in a **traced slot owned by the object** that is reachable while the native
call runs, for exactly the duration needed, then release it. Prefer a closure-scoped helper so the
hold cannot leak or be popped out of order:

```rust
cp.with_in_flight(owner, |held| -> Result<Promise, i32> {
    held.push(ctx, chunk);              // traced until the closure returns, and bound
    if cp.should_call_pull(ctx)? { cp.call_pull(ctx)?; }
    iterator_result_promise(ctx, chunk, false)
});
```

A traced slot only covers the **full** mark. Every value pushed into a payload whose instance may
already be old also needs its own barrier (Rule 5).

Do **not** fix this with a global roots `Vec` and manual push/pop: it is hidden discipline with no
compile-time check, and an early `?` leaks the root.

**Related hazard — holding a `RefCell` borrow across JS.** A `borrow_mut()` taken before a JS call
stays active during it. Any GC in that call traces the same cell with `borrow()` and panics with
"already borrowed"; it can also re-enter and panic on the next borrow. Take the borrow right before
the access and drop it before anything that can run JS. The same discipline applies to `try_borrow`
guards used by `gc_trace`, which silently skip a cell that is mutably borrowed.

## 14. Rule 5 — an old container receiving a young value

Only `gc_generations` builds record these edges. The shape this forces — private fields plus
setters — is right in every build; a feature-off build simply omits the generational bookkeeping.

A minor marks the young generation **plus the remembered set**; it does not descend into an old
object. Once a container has been promoted, assigning a young node into it must record the edge:

```rust
// BAD: `md` may already be old when evaluation starts; both writes are invisible to the
// next minor, which then frees the promise and its resolving functions.
let (promise, resolve) = Promise::create(ctx)?;
md.promise = Some(promise);
md.resolving_funcs = resolve;
```

Nothing panics: the module's top-level-await promise simply can never settle, so the script prints
its last line and the process never exits.

```rust
// GOOD: one setter owns the write and binds every edge it installs.
pub(crate) fn set_capability(
    &mut self,
    ctx: &ScopedContext,
    promise: Promise,
    funcs: [JSValue; 2],
) {
    let owner = self.gc_head_ptr();
    self.promise.set_node(ctx, owner, Some(promise));
    self.resolving_funcs[0].set_node(ctx, owner, funcs[0]);
    self.resolving_funcs[1].set_node(ctx, owner, funcs[1]);
}
```

Where this class of bug actually shows up:

- capabilities / promise resolvers installed on a long-lived record (module evaluation, async
  state, pending operations);
- lazily built caches: module namespace, `import.meta`, parsed or compiled results;
- resolved dependency edges, exported `VarRef`s, async-parent lists;
- an error/result slot written once, long after the owner was promoted;
- a payload field mutated after the instance was created (`RefCell<Option<Object>>`,
  `Vec<JSValue>`, a cached map). Payloads have no re-trace fallback; only zapi instances get the
  per-minor re-scan described in Part 1, section 8.

Already covered by the engine — do not hand-roll these:

- property slots, array elements, Map/Set entries, closure variables (`VarRef`) — the public
  mutators (`set_prop_slot`, `bind_value`, `VarRef::set_value`/`detach`) call `bind`; do not write
  those payloads directly;
- `CallContext::replace_data` — the store and the barrier to the owning function node happen in one
  call (`CallContext::new_owned` carries the owner); `NFuncPayload.data` is private to its module,
  so only `NFuncPayload::data(nth)` (a copy) is reachable;
- JIT-generated property/array stores — generated code calls `h_gc_bind` → `bind`;
- JIT call feedback — `note_call` binds the caller bytecode to the callee bytecode;
- root containers (contexts, shape registry, jobs, user state) — a minor force-descends them.

### 14.1 Typed slots

Private fields plus setters is the convention; the typed slots make a bare write a **compile
error** instead of a review item (`src/gc/guard.rs`, re-exported as
`zuqe::{WriteBarrier, WriteBarrierVec, WriteBarrierMap, GcBind}`). Their representation and the
deliberate omissions are described in Part 1, section 6.

```rust
#[derive(Debug, GcTrace)]
struct FooPayload {
    // scalar slot: `p.result = v` does not compile
    result: WriteBarrier<JSValue>,
    // container slot: `queue.push(v)` / `queue[i] = v` do not compile
    queue: WriteBarrierVec<Box<Req>>,
    // map slot: `map.insert(k, v)` / `map[i] = v` do not compile
    cache: WriteBarrierMap<u64, Object>,
}

impl FooPayload {
    // the payload exposes setters only, and the owner is the instance
    fn set_result(&mut self, ctx: &ScopedContext, owner: JSValue, v: JSValue) {
        self.result.set(ctx, owner, v); // store + barrier, one step
    }
}
```

- **owner**: pass the node that holds the payload — `this_obj.gc_node_ptr()` at the call site, or
  a `JSValue` owner through the public `set`/`push`/`insert`. Passing a foreign owner records the
  edge on the wrong node: silent corruption, catchable only by `GcHeap::bind`'s debug assert and by
  review. A cross-module `field.set(ctx, foreign_owner, v)` is a review blocker.
- **container invariant**: between the store and the bind there must be **no GC point** (nothing
  that can allocate a GC node or collect). The order is not a correctness condition — bind-then-store
  is equally safe — but the two statements must not be split by an edit.
- **removal is not free**: `remove`/`pop`/`clear`/`retain`/`take` need no barrier, but a removed
  element stops being traced. If it is still used after the next GC point, hold it in a traced slot
  first (Rule 4).
- **`new` versus `set`**: `WriteBarrier::new` / `WriteBarrierVec::from_vec` / `WriteBarrierMap::new`
  are initializing stores for payload construction only. After the instance exists every write goes
  through `set`/`push`/`insert`.
- **residual hole**: `p.result = WriteBarrier::new(v)` and `mem::replace(&mut p.result, …)` still
  compile — Rust cannot forbid whole-field replacement. The defence is the setter convention plus
  the static audit, which fails on any such assignment.

**Make the raw field impossible to write.** Keep GC fields private to the payload module and expose
setters that bind. A direct assignment from another module then fails to compile instead of
silently dangling. `ModuleDef` is the reference implementation: `set_capability`, `set_module_ns`,
`set_meta_obj`, `set_attributes`, `set_bytecode`, `set_stack_prev`, `set_cycle_root`,
`try_push_async_parent` are the only write paths, and `ModuleDependency::set_module` does the same
for the dependency edge.

Module entry leaves follow the same shape without a guard type. The entry `Vec`s stay plain (they
are written element-wise only at construction), and every post-construction write of a GC leaf goes
through exactly one method that stores and binds in the same function:

| leaf | single write path |
|---|---|
| `ModuleDependency.module_name` / `attributes` | `ModuleDef::add_dependency` |
| `ImportEntry.import_name` | `ModuleDefImpl::add_import_entry` |
| `ExportEntry.local_name` | `add_export_internal` / `set_export_local_name` |
| `ExportEntry.export_name` | `add_export_internal` |
| `ExportType.var_ref` | `add_export_internal` / `set_export_var_ref` |

The leaf fields are private or `pub(super)`, visible only inside the module, with read-only getters
elsewhere. Building the three entry `Vec`s and assigning them in one shot is legal **only** for a
young, still-unreachable module; any late write must use a setter.

`trace` must still report the field. A barrier keeps the child alive; `trace` is what makes it
reachable. Missing either one loses the node.

### 14.2 Container element types need `GcBind`

`WriteBarrierVec` and `WriteBarrierMap` bind the element they store, so the element type must say
how: implement `GcBind` by hand for every element type that is not a built-in wrapper
(`JSValue`, `Object`, `Promise`, `Atom`, `PropertyKey`, …).

`GcBind` is the value-level counterpart of `GcTrace`: it enumerates the edges one stored value
contributes.

```rust
impl GcBind for MyElement {
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        GcBind::bind_node(&self.strong_value, ctx, owner);
        // weak references MUST be skipped: binding them keeps dead targets alive
        // (`#[gc(skip)]` fields are already excluded from GcTrace — do the same here)
    }
}
```

Two practical notes:

- a tuple type cannot implement `GcBind` — `GcBind` is a foreign trait and a tuple is a foreign
  type, so the impl is rejected. Give the tuple a local struct: it documents the shape and gives
  the audit a place to hang the exemption.
- element structs are still reported by the static audit (their own GC leaves are bare fields of a
  reachable type). That is expected: the element's `GcBind` binds them on every push, so they need
  no slot of their own — record that as an exemption rather than wrapping them redundantly.

## 15. Review checklist

**Rule 1 (payload)**

- [ ] Does the payload contain GC nodes (`JSValue`/`Object`/native wrapper/callback handle)?
- [ ] If yes — does the payload type impl `GcTrace`, and does every GC field get traced?
- [ ] Is it an `extends:` subclass? Does the subclass payload type carry its own `GcTrace` impl?
- [ ] Can a GC field be written **after the instance may already be old**? If yes, the write must go
      through a setter that binds (Rule 5).

**Rule 2 (`thread_local`)**

- [ ] Any `thread_local!` holding GC nodes?
- [ ] Is there a matching trace fn that covers every GC field?
- [ ] Is it registered in `gc_set_trace_handler` on every thread/runtime that holds it?

**Rule 3 (raw pointers into `Vec`)**

- [ ] Is any raw pointer/reference into a `Vec` element cached across calls?
- [ ] Can those calls grow the `Vec` (`push`/`insert` → realloc)?
- [ ] If element-address stability is needed — is the storage `Vec<Box<T>>`, or are the pointers
      re-derived after every mutation?

**Rule 4 (values across JS re-entry)**

- [ ] Does any value leave a traced container (`remove`, `pop`, `take`, `drain`, a dropped
      descriptor) before a call that can run JS?
- [ ] If yes — is it held in a traced slot of an owner until it is handed off, and is that hold
      closure-scoped so it is released on every exit path including `?`?
- [ ] Is the value still referenced after JS runs? Then it must still be traced at that point.
- [ ] Does any `RefCell` borrow stay alive across a call that can run JS? Narrow it or drop it.
- [ ] Did you avoid a global roots `Vec` with manual push/pop, and avoid relying on
      `gc_protect_transient` or scope protection across JS?
- [ ] If the hold is a payload slot, is every value **also** bound to the owner? The slot covers the
      full mark only; the next minor does not descend into an old owner.

**Rule 5 (write barrier)**

- [ ] Property slots / array elements / Map/Set entries / closure variables are routed through
      `bind` already — no action.
- [ ] `cc.replace_data`: the store and the barrier happen inside the call.
- [ ] `def_native_object!` payload fields have no automatic re-trace fallback: a write after the
      instance may be old must bind (prefer a setter).
- [ ] New payload writes go through a setter on the payload type, not a direct field assignment.
- [ ] New payload GC field is a `WriteBarrier`/`WriteBarrierVec`/`WriteBarrierMap` slot, or a
      `Type.field` line in the exemption file (a bare field fails the audit at `file:line`).
- [ ] New container element type has an `impl GcBind` with its weak/`#[gc(skip)]` fields left out.
- [ ] No `field = WriteBarrier::new(...)` / `mem::replace(&mut field, …)` whole-field replacement.
- [ ] The owner's `GcTrace` reports every GC field. A bound but untraced field is still lost.
- [ ] New old→young path exercised under debug + `gc_generations`: promote the owner, write the
      value, request a minor, let execution reach a safepoint. A missing barrier must panic in
      `dbg_assert_no_stale_old_edges`, i.e. `… — missing write barrier`.
- [ ] Barrier regression reproduced under `--gc-stress` at least once on targeted cases.
- [ ] Remember the validator's limits: traced edges only, only paths that run a minor, debug only.
      Release correctness rests on the private fields plus setters, not on the validator.

## 16. The GC audit

`zuqe::gc_audit` is the static half of these rules: it scans a **source directory you pass in** —
not zuqe's checkout — and reports every GC-leaf payload field that is neither a guard slot nor
allowlisted, every container element type without an `impl GcBind`, and every whole-field guard
replacement, each with `file:line`. It ships inside the library, so any crate that depends on
`zuqe` runs it against its own tree, whatever the dependency source is. No cargo feature, no extra
dependency, no `debug_assertions` gate: it is ordinary library code that is never called at
runtime.

What it checks:

| rule | finding |
|---|---|
| 1 | a reachable GC-leaf payload field that is neither a guard slot nor allowlisted |
| 2 | a container element type with no `impl GcBind` and no exemption |
| 3 | a whole-field guard replacement (`field = WriteBarrier::new(…)`, `mem::replace`) |
| — | stale exemption entries (the field was renamed, wrapped or removed) |
| — | a root-set floor, so scanning the wrong directory cannot pass |

The scanner is a deliberate text heuristic (no `syn`): unknown field types are skipped rather than
guessed, only *direct* GC-leaf fields are enumerated, and a payload type unreachable from the root
set is invisible. It reports shape, never write timing.

### 16.1 Standard layout

| path (in your crate) | purpose |
|---|---|
| `tests/gc_audit.rs` | the audit test: one `zuqe::gc_audit_test!(<root floor>);` call |
| `gc_audit_exemption.txt` | the only place exemptions live; read by the test and by the CLI |
| `<crate>/src` | the scanned tree |

`gc_audit_exemption.txt` sits next to `Cargo.toml` (a sibling of `src/`) and is the single source of
truth: never repeat exemptions inline in the test, never keep a second list for the CLI. A crate
with no exemptions may omit the file (the test then runs with an empty list); once the first entry
exists it must go in this file.

Format — one entry per line, `#` comments and blank lines skipped:

```text
# Type.field — reason
MyPayload.cache — construction only
MyPayload.handler — weak target; not traced
```

Every entry is a reviewed claim that the field needs no write barrier of its own, and it should be
one of the legitimate kinds:

1. **construction only** — no late write, so the instance is young when the value is stored;
2. **root container** — a minor force-descends it, so a barrier is pointless;
3. **bound elsewhere** — the single write path binds (a setter, or the element's `GcBind` on push);
4. **structural non-goal** — a raw-pointer mirror, a weak target, a `Cell` slot with one setter.

The test fails on **stale** entries (field renamed, wrapped or gone), so the list cannot rot. Keep
it short: a field that simply "isn't migrated yet" belongs in the migration, not here.

### 16.2 Boilerplate

```rust
// tests/gc_audit.rs
zuqe::gc_audit_test!(30);
```

Run it with `cargo test --test gc_audit`. The macro reads `gc_audit_exemption.txt`, expands to a
single test named `gc_audit_payload_fields`, and runs the whole rule set. The two-argument form
`zuqe::gc_audit_test!(LIST, 30)` takes an inline slice — an escape hatch for one-off tests, not the
standard layout.

`min_roots` is a floor on how many root types your `src` must yield; it stops a scan of a wrong or
empty directory from passing. Start with a rough guess: if it is too high the failure prints your
actual count (`root set suspiciously small: N entries`), so set the floor somewhat below that.

### 16.3 Workflow: red → green

1. Run `cargo test --test gc_audit`. Every reported line is a field to either fix in code (give it a
   guard slot, or add an `impl GcBind` for the element type) or justify with an entry in the
   exemption file.
2. Keep that file short and honest — each entry is a visible diff a reviewer accepts, and stale
   entries fail the test.
3. Green means **shape only**: an entry is a claim you verified against the Rule 5 checklist, not
   proof that no late write exists.

### 16.4 Command-line tool

`zuqe-gc-audit` prints the same inventory as a report instead of a test: the payload root set as a
type list with `file:line` and, per field, its verdict and `bare`/`wrapped`/`container` status —
`✔️` passes, `🔵` allowlisted, `❌` violation. Only diagnostics that cannot be seen in that list are
printed below it: stale exemption entries and whole-field guard replacements. The last line counts
the failures. It reads the **same** exemption file as the test (default: the nearest directory at or
above the scanned directory that contains a `Cargo.toml`), so the report and `cargo test` always
agree.

```text
cargo build --release --bin zuqe-gc-audit
# → target/release/zuqe-gc-audit — standalone; point it at any crate:
./target/release/zuqe-gc-audit path/to/crate/src
./target/release/zuqe-gc-audit --allowlist other.txt path/to/crate/src

# in-tree, with the dev profile:
cargo run --bin zuqe-gc-audit -- [SRC_DIR]
```

`SRC_DIR` defaults to `./src`. Exit status: `0` clean (with the crate's exemption file applied),
`1` violations (so it can gate scripts), `2` usage error.

### 16.5 Discovery helpers

- `gc_leaf_types(dir)` — every struct/enum that *directly* declares a GC-leaf field, with
  `file:line` and the field's `wrapped`/`container` status: the discovery inventory;
- `field_verdicts(dir, allowlist)` — the verdict per field, keyed by `Type.field`:
  `Pass` / `Allowlisted` / `Violation(reason)`;
- `stale_exemptions(dir, allowlist)` — entries that no longer match an allowlisted field;
- `payload_root_types(dir)` — the audit's root set;
- `read_exemptions(path)` — read and parse an exemption file;
- `test_failures(dir, allowlist, min_roots)` — exactly what the macro runs;
- `all_violations`, `gc_field_violations`, `guard_construction_violations` — the pieces, if you
  prefer your own shape.

### 16.6 Limits

The scanner reports shape, not timing: it cannot see whether a field is written after the instance
was promoted, whether a `RefCell` borrow spans a JS call, or whether a value escapes a traced
container between a `remove` and a `push`. Those remain review items and runtime-validator items.
A green audit plus a green `--gc-stress` run on targeted paths is the strongest signal available.

## 17. Best-practice template

A minimal crate that does this right. Adapt the names; keep the shape.

```text
my-crate/
├── Cargo.toml
├── gc_audit_exemption.txt
├── src/
│   ├── lib.rs
│   └── foo.rs          # payload, guard slots, setters, element types
└── tests/
    └── gc_audit.rs
```

`Cargo.toml` — the audit ships inside `zuqe`, so there is nothing extra to depend on:

```toml
[dependencies]
zuqe = { version = "0.11", features = ["gc_generations"] }
```

`src/foo.rs` — payload with guard slots, one setter per slot, and an element type for the container:

```rust
use std::ptr::NonNull;

use zuqe::{
    GcBind, JSValue, Object, ScopedContext, WriteBarrier, WriteBarrierMap, WriteBarrierVec,
    def_native_object,
    gc_lite::{GcHead, GcTrace, GcTraceCtx},
};

/// One queued request. A local struct (not a tuple) so it can implement
/// `GcBind`: a tuple type cannot, `GcBind` being a foreign trait.
#[derive(Debug)]
struct Req {
    once: bool,
    cb: Object,
}

impl GcBind for Req {
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        // only the GC leaf is bound; `once` contributes no edge
        GcBind::bind_node(&self.cb, ctx, owner);
    }
}

#[derive(Debug)]
pub(crate) struct FooPayload {
    // full-mark coverage
    result: WriteBarrier<JSValue>,
    handler: WriteBarrier<Option<Object>>,
    // minor coverage for containers; every element type impls GcBind
    queue: WriteBarrierVec<Req>,
    cache: WriteBarrierMap<u64, Object>,
}

// Hand-written: the derive expands to `::gc_lite::…` and would need `gc-lite`
// as a direct dependency. Only `zuqe` is required here.
impl GcTrace for FooPayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.result.trace(gcx);
        self.handler.trace(gcx);
        for r in self.queue.iter() {
            r.cb.trace(gcx);
        }
        for v in self.cache.values() {
            v.trace(gcx);
        }
    }
}

impl FooPayload {
    /// The payload exposes setters only, so a bare write cannot compile.
    /// `owner` is the instance that holds this payload.
    pub(crate) fn set_result(&mut self, ctx: &ScopedContext, owner: JSValue, v: JSValue) {
        self.result.set(ctx, owner, v);
    }

    pub(crate) fn set_handler(&mut self, ctx: &ScopedContext, owner: JSValue, v: Option<Object>) {
        self.handler.set(ctx, owner, v);
    }

    pub(crate) fn push_req(&mut self, ctx: &ScopedContext, owner: JSValue, r: Req) {
        self.queue.push(ctx, owner, r); // store + barrier, one step
    }

    pub(crate) fn cache_put(&mut self, ctx: &ScopedContext, owner: JSValue, k: u64, v: Object) {
        self.cache.insert(ctx, owner, k, v);
    }
}

def_native_object!(
    "Foo",
    JSFoo,
    FooPayload,
    FOO_PROTO, // protocol + constructor elided
    Some(ClassCtorDef::new(js_foo_constructor, 1, false)),
);
```

At the call site, capture the owner **before** taking `&mut self` for the payload borrow (`JSValue`
is `Copy`), and let the setter do the store and the barrier together:

```rust
let owner = this;                       // captured before the mutable borrow
let p = get_foo_payload_mut(ctx, &mut this)?;
p.set_handler(ctx, owner, Some(cb))?;   // no separate bind call, none to forget
```

`tests/gc_audit.rs`:

```rust
//! Static GC audit: bare payload fields, missing `GcBind` impls, whole-field
//! guard replacement, stale exemptions.
zuqe::gc_audit_test!(10);
```

`gc_audit_exemption.txt` — start empty; add a line only for a structural case you reviewed:

```text
# Type.field — reason
# FooPayload.cache — construction only
```

Run both halves:

```text
cargo test --test gc_audit                 # the test
zuqe-gc-audit src                          # the same report, for humans
zuqe-gc-audit src && cargo test --test gc_audit   # the CI gate
```

Do:

- give every mutable GC field a guard slot, and keep the field private to the payload module;
- give every container element type an `impl GcBind` that skips weak and `#[gc(skip)]` fields;
- route post-construction writes through one setter that stores and binds in the same function;
- build the payload with `new`/`from_vec`/`new()` and never whole-field-replace a slot afterwards;
- read `Deref`/`get()`; there is no `iter_mut`, no index assignment, no `DerefMut`;
- run the audit and the validator on targeted paths before calling a change done.

Do not:

- leave a manual `ctx.bind_value(...)` next to a guard-slot write — the slot already binds;
- store a GC node in a `thread_local!` without a registered trace callback;
- hold a `RefCell` borrow across a call that can run JS;
- take a value out of a traced container and use it after JS without re-parking it in a slot;
- add an exemption for a field you simply did not migrate.
