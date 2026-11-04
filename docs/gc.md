# Garbage Collection

Zuqe uses a tracing and mark-sweep memory management approach that combines Rust's ownership system with JavaScript's garbage collection.

## GC Scopes

`ScopedContext` represents a **GC scope** — a lightweight, nestable sub-region within a `Context`. Scopes form a stack: each call to `with_new_scope` pushes a child scope on top of the current one.

The public entry point to the root ScopedContext is `Context::with_root_scope()`, which hands you the context's root scope. Temporary work should run inside child scopes created with `ScopedContext::with_new_scope()`.

### Why scopes?

Scope-local GC allocations are **temporarily protected** — the GC treats them as reachable roots for the lifetime of that scope, so they won't be freed even if nothing else references them. This avoids the need for Rust references or `GcRef` handles in short-lived operations:

```rust
ctx.with_root_scope(|mut root| {
    root.with_new_scope(|scope| {
        let mut arr = Array::create(scope)?; // protected by this scope
        arr.define_property_value_u32(scope, 0, JSValue::from(42), JS_PROP_C_W_E)?;
        // arr is automatically released when this scope closes
        Ok(JSValue::UNDEFINED)
    })
});
```

### Scope stack and promotion

Scopes are nested like a stack. When the top-most scope exits, its **return value** is promoted to the parent scope's protection — any GC nodes embedded in that value remain safe. All other scope-local nodes lose their temporary protection and become eligible for GC (freed only if no other references exist):

```rust
fn compute_result(ctx: &mut Context) -> Result<JSValue, i32> {
    ctx.with_root_scope(|mut root| {
        root.with_new_scope(|scope| {
            // These arrays are local — protected only inside this scope
            let tmp1 = Array::create(scope)?;
            let tmp2 = Array::create(scope)?;
            // ... do work with tmp1, tmp2 ...

            // Build a JS array carrying the result.
            let result = Array::from_values(scope, vec![JSValue::from(1), JSValue::from(2)])?;
            Ok(result.into())
        }) // tmp1, tmp2 lose protection here; the result value is promoted to the root scope
    })
}
```

### Nested scopes

You can nest scopes to fine-tune lifetime boundaries:

```rust
ctx.with_root_scope(|mut root| {
    root.with_new_scope(|outer| {
        let outer_val = JSValue::from(1);

        outer.with_new_scope(|inner| {
            let inner_val = JSValue::from(2);
            // Both outer_val and inner_val are protected here
            let _ = inner_val;
        }); // inner_val loses protection here; outer_val still protected

        outer_val
    }) // outer_val promoted to the root scope
});
```

### `with_root_scope`

The root scope (depth = 1) lives for the **entire lifetime** of the `Context`. Any GC node promoted into it is **permanently reachable** — never collected until the `Context` itself is dropped:

```rust
ctx.with_root_scope(|mut scope| {
    let val = JSValue::from(42);
    // val is now in the root scope — it will never be GC'd
});
```

For long-running applications, avoid accumulating values in the root scope. Use `with_new_scope` for temporary work so that intermediate values are released when the child scope exits.

> **Notice**: Any value placed into the root scope will **persist forever** — even if nothing references it, the GC will not reclaim it, because the root scope acts as a permanent GC root set. If you know a value in the root scope is no longer needed, you **must** manually call `scope.remove_local(node)` to remove it from the scope's protection. Otherwise it will leak until the `Context` itself is dropped.

### Returning values across a scope boundary

You return a value from `with_new_scope` simply as the closure's final expression — **no special call is required**. `with_new_scope` automatically invokes `promote()` on the returned value before the child scope is destroyed, so the GC nodes embedded in it are moved into the parent scope's cache and stay protected across the boundary:

```rust
let obj = ctx.with_root_scope(|mut root| {
    root.with_new_scope(|scope| {
        let obj = JSValue::from(scope.create_base_object()?);
        // `obj` is promoted to the root scope automatically on return
        Ok(obj)
    })
})?;
```

Promotion works through the `ScopeResult` trait, which is implemented for `JSValue`, `Result<T, E>`, `Option<T>`, and the common scalar/no-op types (`()`, `i32`, `bool`, `String`, …). Because promotion happens for whatever you return, returning `Ok(obj)` is all you need — any GC nodes reachable from the returned value survive the scope exit. `Context::with_root_scope` itself does not promote (its result stays where the caller puts it), so it can return any type.

## ⚠️ GC safety for native code — class payloads & `thread_local` (MUST READ)

> **Golden rule: any GC node held on the Rust side must be part of the GC reachability graph.**
> Zuqe's GC only sees nodes reachable from its roots. A `JSValue`/`Object` you stash in a native
> class payload or in a `thread_local!` static is **invisible to the collector**. If it gets
> reclaimed while you still use it, that is a **use-after-free** — and in debug builds the engine
> panics (`gc: bad node`). Missing a `trace` is fatal; calling `trace` on a non-node value is a
> harmless no-op. **When in doubt, trace it.**
>
> **With the opt-in `gc_generations` feature there is a second, separate requirement:** `trace`
> decides what survives a *full* mark; a write of a young node into an **old** container must also
> record the edge with a write barrier (`bind`). `trace` without a barrier loses the child on the
> next minor; a barrier without `trace` loses it on the next full mark. See Rule 5.

### What counts as a GC node

A type is a **GC node** if it implements `gc_lite::GcNode` (allocated on the GC heap, markable /
reclaimable) or if it wraps/contains a `GcRef<GcNode>`. Common node types:

- `Object`, and its transparent wrappers: `Promise`, `Array`, `TypedArray`/`Uint8Array`,
  `Map`/`Set`, `RegExp`, `Iterator`, `Function`, `Proxy`, `WeakRef`, `ArrayBuffer`, and native
  class instances defined by `def_native_object!` (`JSFile`, `JSReadableStream`, `JSResponse`, …).
- `JSValue` — **always trace it**: it may hold a node (Object / GC-heap String / Symbol /
  FunctionBytecode / Module) or an inline value (Int/Bool/Double). `JSValue::trace` no-ops on
  non-node tags, so over-tracing is safe and under-tracing is fatal.
- Callback handles (JS functions / resolvers held as `Object`).

**Not** GC nodes (pure Rust data, no tracing needed): `String`, `Vec<u8>`, numbers, fds, channels,
`Arc<Mutex<…>>`, `serde_json::Value`, and FFI wrappers such as `LlamaModel`/`LlamaContext`.

Rules 1 and 2 are the two entry points for **long-lived** Rust-held nodes. A value that is only
held **during a native call** and is used after JS runs is covered by Rule 4. If a node isn't
covered by one of these rules, it is unsafe:

### Generations: young, old, and what a minor does

`gc_generations` is an opt-in build feature; enable it on a host build that needs generational
collection (`zuqe = { features = ["gc_generations"] }`). Builds without it keep the
single-generation path. With it on, each partition is split into a young chain and an old chain:

- A fresh allocation is **young** (age 0) and is swept by every minor unless marked.
- A young node that survives `promote_after` minors is promoted to **old**.
  The default is **1 in debug builds** (the shorter young phase surfaces a
  missing old→young barrier sooner) and **2 in release builds**.
  (Black).
- A **major** merges the whole young chain first: every live node is old afterwards.
- A minor never sweeps old nodes; old garbage waits for a major.
- A minor marks the young chain (age>0 survivors are roots) plus the **remembered set** of old
  nodes that recorded an old→young write. It does not descend into an old node's payload.

Minors run automatically when young allocations exceed the young budget. `gc_minor_request()`
only *requests* one; it runs at the next safepoint, so a stress caller must execute a few more
bytecodes. `gc_set_promote_after(0)` disables minors (major-only) but keeps the bookkeeping;
building without the feature removes it entirely. `gc_set_promote_after` accepts 0/1/2 (larger
values clamp to 2).

Consequence: a quick script passing proves nothing about barriers. The owner is often already old
(the entry module, a long-lived stream controller, a cache created at startup), and a major makes
everything old. Assume any mutated container can be old and bind the write.

### Rule 1 — class payloads holding GC nodes: implement `GcTrace`

When a `def_native_object!` payload stores GC nodes, the payload type **MUST** implement
`gc_lite::GcTrace` — typically via `#[derive(GcTrace)]` (or `#[gc(skip)]`/`#[gc(with)]` for
fine-grained control). The macro wires the payload's trace into the class's `on_gc_trace`
automatically at registration, so the collector traces every field whenever an instance of that
class is marked. A payload type that is not `GcTrace` fails to compile right at the macro call —
there is no way to register an untraced payload:

```rust
use zuqe::gc_lite::GcTrace;

#[derive(GcTrace)]
struct FooPayload {
    field: RefCell<Option<JSValue>>, // JSValue / Object / native wrappers all impl GcTrace
}

def_native_object!(
    "Foo",
    JSFoo,
    FooPayload,
    FOO_PROTO,
    Some(ClassCtorDef::new(js_foo_constructor, 1, true)),
);
```

For a payload that holds **no** GC nodes, write an empty manual `impl GcTrace for Payload { fn
trace(&self, _: &mut GcTraceCtx) {} }` (a foreign type such as `RefCell<T>` cannot take a derive).

**Pitfalls:**

1. **`extends:` subclasses do NOT share the parent's payload type.** GC looks the trace callback up
   by the object's **own** `class_id`, and `new_instance` creates objects with
   `Self::class_id()`. Each class's payload type carries its own `GcTrace` impl — a subclass
   reusing a payload that holds GC nodes gets its trace wired from its own payload type
   automatically; there is no inherit-to-silently-drop-trace hazard anymore.
2. **Lazily-cached JS objects are GC nodes too.** A `RefCell<Option<Object>>` cached in the payload
   (e.g. `URL.searchParams`) must be traced once it is only reachable from that payload — with the
   derive this is automatic as long as the field is a traced type.
3. **`GcTrace` is not a write barrier.** It keeps the payload's children alive across a *full*
   mark. With `gc_generations`, writing a young value into a payload whose instance may already be
   old also needs `bind` (Rule 5): the next minor does not descend into the old instance and frees
   the value. `def_native_object!` payloads have **no** automatic re-trace fallback (only zapi
   instances do), so route such writes through a setter that binds:

   ```rust
   payload.cache = Some(obj);
   ctx.bind_node_value(instance_node, obj);
   ```

   `ScopedContext::bind_value(owner, child)` is the public write-barrier entry point; the
   node-based forms (`bind_node_value` / `bind_node`) and `GcHeap::bind` are crate-internal for
   code that already holds node pointers. Use `bind_value` in every path that has a
   `ScopedContext`.

   The same applies to a `RefCell<Vec<JSValue>>` or map inside the payload: after appending a
   young value, bind the **instance** again (`bind` records the owner once per minor and dedupes,
   so repeated binds are cheap). Remember the remembered set is cleared at each minor, so a write
   in a later minor needs its own bind; binding once at creation is not a permanent license.

### Rule 2 — `thread_local!` stores holding GC nodes: register a user GC trace

`thread_local!` statics are **not** in GC's reachability graph. The root scan (`src/gc/gc.rs`)
only traces runtime roots (jobs, shapes, and the `gc_user_trace` callbacks). A GC node stored in a
thread-local global must be explicitly rooted, or it is collected while the thread-local still
points at it → UAF → panic:

```rust
// 1) a thread_local holding GC nodes (JSValue / Object / native wrapper / callback)
thread_local! {
    static MY_STATE: RefCell<HashMap<u64, JSValue>> = const { RefCell::new(HashMap::new()) };
}

// 2) a matching trace fn: walk the variable and trace every node
fn gc_trace_my_state(gcx: &mut GcTraceCtx) {
    MY_STATE.with_borrow(|map| {
        for v in map.values() {
            v.trace(gcx);
        }
    });
}
```

Register every such function on the runtime (this is exactly the `mod_os_gc_trace` pattern in
`examples/zjs/mod_os.rs`, see the "Customized GC tracer" section below):

```rust
rt.gc_set_trace_handler(|gcx| {
    gc_trace_my_state(gcx);
    gc_trace_other_state(gcx);
    // ... one entry per thread_local that holds GC nodes
});
```

**Key points:**

- Every `thread_local!` that holds GC nodes needs its own trace fn, and that fn **must** be
  registered — missing one means the nodes inside it can be collected at any time.
- `gc_set_trace_handler` is **per-thread / per-runtime**: register on every runtime/thread that
  may touch the thread-local.
- The trace fn must cover **all** GC-node fields of the variable; trace every `JSValue`
  unconditionally.
- The user trace is a GC **root** and a minor force-descends roots, so a value reachable only from
  a registered thread-local needs no write barrier — the handler must still trace every field, or a
  full mark loses it. This root treatment does **not** extend to ordinary old containers or to
  `def_native_object!` payloads (Rule 5).

### Rule 3 — raw pointers into `Vec` storage: element addresses move on growth (rare)

Unlike Rules 1–2 this is **not** a tracing problem, and it is **rare**: safe Rust's borrow
checker already rejects most of it. It only arises when `unsafe` raw pointers bypass borrows —
so guard against it exclusively where raw pointers are involved.

A `Vec` relocates **all** of its elements to a fresh allocation when it grows. A raw pointer
into an element (`&mut v[i].field as *mut _`, `v.as_ptr()`, a cached `NonNull`, …) dangles as
soon as any `push` grows the vector — even though the `Vec` itself is still alive. Unlike
Rules 1–2 the failure mode is not "a node was collected while referenced" but **live memory
being stomped through the dangling pointer**, which corrupts whatever reuses that memory —
often GC nodes — so the symptoms surface far away from the cause.

Real-world case: `VmStack::frames` was a plain `Vec<StackFrame>` and grew via `alloc_frame → push`
whenever a native call allocated a callee frame, while `execute_with_arena` cached
`&mut frame.cur_sp_offset` / `cur_pc_offset` as raw pointers across its whole interpretation
loop. The stale writes corrupted freed memory later reused by GC nodes — surfacing far from
the cause as gc-lite `bad node` panics, recycled-callback TypeErrors (`not a function`), and
index-out-of-bounds aborts during runtime teardown.

**Prevention** (only relevant when raw pointers are involved):

- Never cache an element pointer of a growable `Vec` across a call that may `push` to it;
  re-derive it from the index after every mutation.
- If element-address stability is actually required, store `Vec<Box<T>>` — `Box` gives
  per-element stability independent of vector growth (see `VmStack::frames`).
- Distant, shape-shifting corruption (bad nodes, wrong-callback TypeErrors, teardown bounds
  aborts) is the signature of this class. Run the reproducer under ASAN
  (`RUSTFLAGS="-Zsanitizer=address"` + nightly); the very first offending access is reported
  with both the use stack and the free stack.

### Rule 4 — values taken out of a traced container, used across JS re-entry

This is the most common way to get a use-after-free in native code, and the one scope protection
does **not** cover. The pattern:

```rust
let chunk = queue.borrow_mut().remove(0);  // 1. leaves the traced queue
call_js_that_may_gc(ctx)?;                  // 2. runs bytecode, may GC
use(chunk);                                 // 3. chunk may already be freed
```

Between steps 1 and 3 the only reference to `chunk` is a Rust local, and no traced structure
holds it. Scope protection does not save it: at a GC safepoint the executor calls
`clear_locals()` on the current scope **before** `gc_collect()`, so scope-local protection is
dropped exactly when the GC runs. `ScopedContext::gc_protect_transient` is a no-op unless the
`gc_protect_transient` feature is on, and it still relies on scope protection, so do not use it
as a fix.

"Calls JS" includes any call that can run bytecode: `call`, `invoke_*`, a `pull`/`cancel`/`then`
callback, a thenable check on a value you resolve with, and a getter that runs on `[[Get]]`.

**Fix:** keep the value in a **traced slot owned by the object** that is reachable while the
native call runs (for example the stream controller). Hold it for the exact duration needed and
release it right after. Prefer a closure-scoped helper so the hold cannot leak or be popped out of
order. The `aike` case uses `ReadableControllerPayload::with_in_flight`
(`aike/src/whatwg/stream/readable_controller.rs`); with `gc_generations` its guard binds every
pushed value to the controller node:

```rust
cp.with_in_flight(owner, |held| -> Result<Promise, i32> {
    held.push(ctx, chunk);              // traced until the closure returns, and bound
    if cp.should_call_pull(ctx)? { cp.call_pull(ctx)?; }
    iterator_result_promise(ctx, chunk, false)
});
```

With `gc_generations`, a traced slot only covers the **full** mark. Every value pushed into a
payload of an instance that may already be old also needs its own write barrier; the next minor
does not descend into that instance. See Rule 5.

Do **not** fix this by pushing to a global roots `Vec` with manual push/pop. That is a hidden
discipline with no compile-time check, and an early `?` can leak the root.

**Related hazard: holding a `RefCell` borrow across JS.** A `borrow_mut()` taken before a JS call
stays active during that call. Any GC in the call traces the same cell with `borrow()` and panics
with "already borrowed". It can also re-enter and panic on the next borrow. Take the borrow
right before the access and drop it before any call that can run JS. The `aike` fix was in
`enqueue`: the queue was borrowed for the whole `fill_pending_pull_intos`, which resolves
promises. It is now borrowed only for the final leftover insert.

### Rule 5 — an old container receiving a young value: write barrier

Only builds with the opt-in `gc_generations` feature record these edges. The shape it forces
(private fields + setters) is right in every build; a feature-off build simply omits the
generational bookkeeping.

A minor marks the young generation **plus the remembered set**; it does not descend into an old
(Black) object. Once a container has been promoted, assigning a young node into it must record the edge:

```rust
// BAD: `md` may already be old when evaluation starts; both writes are invisible
// to the next minor, which then frees the promise and its resolving functions.
let (promise, resolve) = Promise::create(ctx)?;
md.promise = Some(promise);
md.resolving_funcs = resolve;
```

(The current `ModuleDef` makes those fields private, so this snippet no longer compiles — that
is the point. The historical version did direct writes and produced the failure below.)

Nothing panics: the module's top-level-await promise simply can never settle, so the script
prints its last line and the process never exits. That was the real `distro verify` hang: after
importing every extension with dynamic `import()` and printing all `ok …` lines, the runtime
parked forever.

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

Places this class of bug actually shows up:

- capabilities / promise resolvers installed on a long-lived record (module evaluation, async
  state, pending operations);
- lazily built caches: module namespace, `import.meta`, parsed or compiled results;
- resolved dependency edges, exported `VarRef`s, async-parent lists;
- an error/result slot written once, long after the owner was promoted;
- a **native payload** field mutated after the instance was created (`RefCell<Option<Object>>`,
  `Vec<JSValue>`, a cached map). `def_native_object!` payloads have no re-trace fallback: only
  zapi instances are re-scanned every minor.

Already covered by the engine (do not hand-roll these):

- property slots, array elements, Map/Set entries, closure variables (`VarRef`) — the public
  mutators (`set_prop_slot`, `bind_value`, `VarRef::set_value`/`detach`) call `bind`; do not write
  those payloads directly;
- `CallContext::replace_data` — the store and the barrier to the owning function node happen in
  the one call (`CallContext::new_owned` carries the owner); there is no post-callback rebind.
  `NFuncPayload.data` itself is **private to `src/function/nfun.rs`**, so outside that file the
  field cannot be written at all — only `NFuncPayload::data(nth)` (a copy) is reachable;
- JIT-generated property/array stores — generated code calls `h_gc_bind` → `bind`;
- JIT call feedback — `note_call` binds the caller bytecode to the callee bytecode;
- root containers (contexts, shape registry, jobs, user state) — a minor force-descends them.

The barrier has one primitive — `GcHeap::bind`, the generational remembered set plus tri-color and
cross-partition bookkeeping — and one public entry point: `ScopedContext::bind_value(owner, child)`.
The node-based forms (`ContextImpl::bind_node_value` / `bind_node`, `RuntimeImpl::bind_node`,
`RuntimePtr::bind_node`) are crate-internal for paths that already hold GC node pointers. Values
that reference no GC node are a no-op. The module setters (`ModuleDef::set_capability`, …) and
their `bind_child` helper delegate to this API; `GcHeap::bind` is used directly only where no
context exists (GC finalize paths).

Make the raw field impossible to write: keep GC fields **private** to the payload module and
expose setters that bind (`set_capability`, `set_module_ns`, `set_attributes`, …). A direct
assignment from another module then fails to compile instead of silently dangling. `ModuleDef`
(`src/module/mdef.rs`) is the reference implementation: `bind_value` / `bind_node` /
`bind_child` are the only write paths, and `ModuleDependency::set_module` does the same for the
dependency edge.

**Module entries** (`memo/gc_bind_guard.md` 附录 C / U6, route B) follow the same shape without a
guard type: the entry `Vec`s stay plain (they are written element-wise only at construction), and
every post-construction write of a GC leaf goes through one `ModuleDefImpl` method that stores and
binds in the same function —

| leaf | single write path |
|---|---|
| `ModuleDependency.module_name` / `attributes` | `ModuleDef::add_dependency` |
| `ImportEntry.import_name` | `ModuleDefImpl::add_import_entry` |
| `ExportEntry.local_name` | `add_export_internal` / `set_export_local_name` |
| `ExportEntry.export_name` | `add_export_internal` |
| `ExportType.var_ref` | `add_export_internal` / `set_export_var_ref` |

The leaf fields are private or `pub(super)` — visible only inside `crate::module` — with
read-only getters for the rest of the crate, so code outside `crate::module` cannot assign them
at all. `persistent/module.rs` builds the three entry
Vecs and assigns them in one shot — that is legal **only** because it is construction of a young,
unreachable module (see the comment there); any late write must use a setter.

`trace` must still report the field. A barrier keeps the child alive; `trace` is what makes it
reachable. Missing either one loses the node (see Rule 1) — the barrier cannot rescue an
unreported edge.

#### Typed slots: `WriteBarrier<T>` / `WriteBarrierVec<T>`

Design and audit record: `memo/gc_bind_guard.md` (three-layer defence, pilot list, allowlist).

Private fields + setters are the convention; the typed slots make a bare write a **compile
error** instead of a review item (`src/gc/guard.rs`, re-exported as `zuqe::{WriteBarrier,
WriteBarrierVec, GcBind}`). Both are `#[repr(transparent)]` with no runtime state — layout,
size and `offset_of` are identical to the bare type, so JIT/host code that reads payload
fields by offset is unaffected. Never add a field-level flag to them: that would break
transparency.

```rust
#[derive(Debug, GcTrace)]
struct FooPayload {
    // scalar slot: `p.result = v` does not compile
    result: WriteBarrier<JSValue>,
    // container slot: `queue.push(v)` / `queue[i] = v` do not compile
    queue: WriteBarrierVec<Box<Req>>,
}

impl FooPayload {
    // Rule①: the payload exposes setters only, and the owner is `self`
    fn set_result(&mut self, ctx: &ScopedContext, owner: NonNull<GcHead>, v: JSValue) {
        self.result.set_node(ctx, owner, v); // store + barrier, one step
    }
}
```

- **`new` vs `set`**: `WriteBarrier::new(v)` / `WriteBarrierVec::from_vec(v)` are for building
  the payload — the instance does not exist yet, so it is born young and there is no edge to
  record (V8's initializing-store rule). After the instance exists, every write goes through
  `set`/`set_node` (scalar) or `push`/`push_node` (container).
- **owner**: pass the node that *holds the payload* — `self.gc_head_ptr()` inside the payload's
  own setter, `this_obj.gc_node_ptr()` at the call site. Passing a foreign owner records the
  edge on the wrong node and is silent corruption: only `GcHeap::bind`'s debug assert and review
  can catch it. Cross-module `field.set(ctx, foreign_owner, v)` is a review blocker.
- **container invariant**: between the store and the bind there must be **no GC point** (no
  call that can allocate a GC node or collect). The *order* is not a correctness condition —
  bind-then-store is equally safe (`settle.rs` does it) — but `GcHeap::bind` never allocates or
  collects, and the value is still rooted by the stack/scope across the two statements.
- **removal is not free**: `remove`/`pop`/`clear`/`retain` need no barrier, but a removed
  element stops being `trace`d. If it is still used after the next GC point it needs
  `gc_protect_transient` first (Rule 4). `AGenPayload::dequeue` does exactly this.
- **no `Clone`, no `Default`** on either guard: copying or resetting fabricates unbarriered
  edges. Read with `get()` (needs `T: Copy`); the write path does not need `Copy`.
- **residual hole**: `p.result = WriteBarrier::new(v)` and `mem::replace(&mut p.result, …)`
  still compile — Rust cannot forbid whole-field replacement. The defence is the setter
  convention above plus `tests/gc_audit.rs`, which fails on any such assignment and on
  any new unwrapped GC payload field (allowlisted entries are printed as a visible diff).

The scanner is a library API, so an embedding crate audits **its own** `def_native_object!`
payloads with the same code and its own allowlist (`zuqe::gc_audit`):

```rust
// EXEMPT = structural exemptions only (root containers, construction-only
// fields, composite shapes). Everything else must be wrapped.
const EXEMPT: &[&str] = &["MyPayload.cache — construction only"];

#[test]
fn gc_payload_fields() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let v = zuqe::gc_audit::gc_field_violations(&src, EXEMPT);
    assert!(v.is_empty(), "{}", v.join("\n"));

    let v = zuqe::gc_audit::guard_construction_violations(&src);
    assert!(v.is_empty(), "{}", v.join("\n"));
}
```

Exemptions are `Type.field — reason`; an entry whose field disappeared, was renamed or got
wrapped is reported as stale, so the list cannot rot. Keep it to the structural cases — a field
that simply "isn't migrated yet" belongs in the migration, not here. `payload_root_types(&src)` lets a
test assert that the scan actually found the payloads it was meant to see.

`memo/gc_bind_guard.md` 附录 C (U1–U6) changed the list like this: **removed**
`ModuleDefImpl.resolving_funcs` (now `[WriteBarrier<JSValue>; 2]`), **reworded** the seven module
entry lines to name the single write path (U6 route B), `NFuncPayload.data` (U1 binds at the write
site) and the four `Iterator{Helper,Concat}Data` `Cell` slots (U5 `set_*` methods). An entry is
never deleted just because the code moved — only because the field is now guarded or the write is
provably inside one method.

`GcBind` is the value-level counterpart of `GcTrace`: it enumerates the edges one stored value
contributes. `JSValue`, `Atom`, `PropertyKey`, `Option`/`Box` and every object wrapper have
impls; element types (structs pushed into a `WriteBarrierVec`) need a hand-written one:

```rust
impl GcBind for MyElement {
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        ctx.bind_node_value(owner, self.strong_value);
        // weak references MUST be skipped: binding them keeps dead targets alive
        // (`#[gc(skip)]` fields are already excluded from GcTrace — do the same here)
    }
}
```

One-step primitives for foreign slots (`&mut JSValue` whose type you do not own) used to be
`ScopedContext::bind_set` / `bind_push`; D11 made them test-only (`#[cfg(test)]`) because
production code now holds a typed slot or calls the owner's foreign-slot API. The strong
guarantee comes from the typed slots, not from an ergonomic helper.

**Differential debugging recipe** (all switches already exist in zhuque):

1. `gc_set_promote_after(0)` — minors off, major only. Bug disappears ⇒ suspected missing
   barrier;
2. build without the `gc_generations` feature — same signal, no generational bookkeeping at
   all;
3. debug build + `zjs --gc-stress <script>` (or `test262 --gc-stress --tests '<glob>'`) —
   every allocation requests a minor, so `dbg_assert_no_stale_old_edges` fires on the first
   stale edge; its panic text names the child, the owner and the dtype.
   The flag exists **only** in `debug` + `gc_generations` builds: anywhere else it is rejected
   as an unknown argument instead of silently doing nothing. Stress costs allocations × heap
   size (each minor runs a full O(heap) verification), so use it on small scripts and targeted
   cases — never as a general regression switch. It is a bug-finder, and it does find bugs: on
   weak-reference-heavy scripts it currently trips a **pre-existing** double-free in
   `ObjectImpl::finalize` → `JSMapState::remove_weak_obj` (repro and read-out in
   `memo/gc_bind_guard.md` §4.6), which is not covered by this rule.


**Detection:** with debug + `gc_generations`, every minor is followed by
`GcHeap::dbg_assert_no_stale_old_edges` (gc-lite), called from `RuntimeImpl::gc_minor`. It walks
**every old node** (reachable or not, all partitions) and traces its current edges; one pointing
at a node the minor just collected panics with:

```
a live object still points at a node freed by the last minor:
child 0x… (freed dtype …), owner 0x… (dtype …) — missing write barrier
```

Known limits, so do not treat a green run as proof: it only sees edges `trace` reports, only
paths that actually trigger a minor, and only debug builds. Release builds have no runtime check;
there the private fields + setters (or the typed slots) are the guarantee. Stress multiplies
what a single run sees — see the recipe above.

### Review checklist

**Rule 1 (class payload)**

- [ ] Does the payload contain GC nodes (`JSValue`/`Object`/native wrapper/callback handle)?
- [ ] If yes — does the payload type impl `GcTrace` (derive), and does every GC field get traced?
- [ ] Is it an `extends:` subclass? If so, does the subclass payload type carry its own `GcTrace` impl?
- [ ] Can a GC field be **written after the instance may already be old**? If yes — the write must go
      through a setter that calls `bind` (Rule 5). `GcTrace` alone only covers a full mark.

**Rule 2 (`thread_local`)**

- [ ] Any `thread_local!` holding GC nodes?
- [ ] If yes — is there a matching trace fn that covers every GC field?
- [ ] Is it registered in `gc_set_trace_handler` on every thread/runtime that holds it?

**Rule 3 (raw pointers into `Vec` storage)**

- [ ] Is any raw pointer / reference into a `Vec` element cached across calls?
- [ ] Can those calls grow the `Vec` (`push`/`insert` → realloc)?
- [ ] If element-address stability is needed — is the storage `Vec<Box<T>>`, or are the
      pointers re-derived after every mutation?

**Rule 4 (values across JS re-entry)**

- [ ] Does any value leave a traced container (`remove`, `pop`, `take`, `drain`, a `Vec` that was
      the only holder, a descriptor that is dropped) before a call that can run JS?
- [ ] If yes — is the value held in a traced slot of an owner until it is handed off, and is that
      hold closure-scoped or otherwise guaranteed to be released on every exit path, including
      `?` errors?
- [ ] Is the value still referenced after JS runs? If it is, it must still be traced at that point.
- [ ] Does any `RefCell` borrow (`borrow`/`borrow_mut`) stay alive across a call that can run JS?
      Narrow it to the access, or drop it first.
- [ ] Did you avoid a global roots `Vec` with manual push/pop, and avoid relying on
      `gc_protect_transient` or scope protection across JS?
- [ ] If the hold is a payload slot (`Vec<JSValue>`, cache, pending map), is every value **also**
      bound to the owner (`bind`)? The slot being traced only covers the full mark; the next minor
      does not descend into an old owner (Rule 5).

**Rule 5 (generational write barrier, only with `gc_generations`)**

A minor only marks the young generation plus the remembered set, so an **old container that starts
referencing a young node** must record the edge:

- [ ] Property slots / array elements / Map/Set entries / closure variables: already routed through
      `bind` (`set_prop_slot`, `bind_value`, `VarRef::set_value`/`detach`).
- [ ] Native payload (`cc.replace_data`): the store and the barrier to the owning function node
      happen inside `replace_data` — `CallContext::new_owned` carries the owner, and the old
      post-callback full rebind (`bind_data_to_self`) is gone. `NFuncPayload.data` is private to
      `src/function/nfun.rs`: a write from any other module must not compile (E0616).
- [ ] JIT-generated property/array stores: `h_gc_bind` → `bind`; no action needed.
- [ ] JIT call feedback (`note_call`) binds the caller bytecode to the recorded callee bytecode.
- [ ] `def_native_object!` payload fields (`RefCell<Option<Object>>`, `Vec<JSValue>`, caches) have
      **no** automatic re-trace fallback. A write after the instance may be old must call `bind`
      (prefer a setter). Only zapi instances get the per-minor re-scan below.
- [ ] New payload writes go through a **setter on the payload type**, not a direct field
      assignment. Host code calls `ctx.bind_value(owner, child)`; engine-internal setters may call
      the crate-private `bind_node_value(owner_node, child)`. Both are no-ops for non-GC values.
- [ ] The GC fields that can be written after promotion are **private** to the payload module, so a
      bypass does not compile. Follow `ModuleDef` (`src/module/mdef.rs`): `set_capability`,
      `set_module_ns`, `set_meta_obj`, `set_attributes`, `set_bytecode`, `set_stack_prev`,
      `set_cycle_root`, `try_push_async_parent`; `ModuleDependency::set_module` for dependency edges.
- [ ] Module entry leaves (`import_name`, `local_name`/`export_name`, `ExportType.var_ref`,
      `ModuleDependency.module_name`/`attributes`): written only by `add_import_entry` /
      `add_export_internal` / `set_export_local_name` / `set_export_var_ref` / `add_dependency`,
      each of which binds in the same function. From outside `crate::module` a direct assignment
      does not compile (private fields + read-only getters).
- [ ] The owner's `GcTrace` reports every GC field. A bound but untraced field is still lost; an
      edge the validator cannot see is an edge nobody can save.
- [ ] Root containers (contexts, shape registry, jobs, user state) are force-descended by a minor,
      so writes into them are covered; ordinary old containers are **not** descended.
- [ ] zapi extensions have no ABI write barrier entry point. The engine compensates by re-tracing the
      payload of every **old** zapi instance on each minor (through the class `gc_trace` callback),
      so a young value written into an opaque payload is still found. This is a fallback: a
      value-carrying barrier (planned for a future `zuqe_zapi` ABI) avoids re-scanning every old
      instance. Extension `gc_trace` callbacks must report every GC field they hold — the fallback
      can only see what `gc_trace` reports.
- [ ] New old→young path: exercised under debug + `gc_generations`, promoting the owner, writing the
      value, then requesting a minor (`gc_minor_request()`) and letting execution reach the next
      safepoint. A missing barrier must panic in `dbg_assert_no_stale_old_edges`,
      i.e. `child … (freed dtype …), owner … (dtype …) — missing write barrier`.
- [ ] New payload GC field is a `WriteBarrier`/`WriteBarrierVec` slot, or a `Type.field` line in
      `tests/gc_audit.rs`'s allowlist (a bare field fails that test at `file:line`)?
- [ ] New container element type has an `impl GcBind` with its weak/`#[gc(skip)]` fields left out?
- [ ] New `bind` call site still has a bare store next to it? Merge the pair into a guard
      `set`/`push` (the payload's single setter) — two statements can be split by a later edit.
- [ ] Any `field = WriteBarrier::new(...)` / `mem::replace(&mut field, …)` whole-field
      replacement? That bypasses the barrier and is rejected by the same audit test.
- [ ] Barrier regression reproduced under `--gc-stress` at least once? Stress is what makes the
      validator see a write that a normal run would never collect between.
- [ ] Remember the validator's limits: only traced edges, only paths that actually run a minor, only
      debug builds. Release correctness rests on the private fields + setters above, not on the
      validator.
- [ ] Runtime switch: `gc_set_promote_after(0)` disables minors (major-only) without rebuilding.
      It keeps the young chain and remembered-set bookkeeping, so it is **not** the same as building
      without `gc_generations`; use the feature-off build when a host needs the zero-bookkeeping
      path.

## Auditing GC payload fields from your own crate

`zuqe::gc_audit` is the static half of the rules above: it scans a **source directory you pass
in** — not zuqe's checkout — and reports every GC-leaf payload field that is neither a
`WriteBarrier`/`WriteBarrierVec` slot nor allowlisted, every container element type without an
`impl GcBind`, and every whole-field guard replacement, each with `file:line`. It ships inside
the library, so any crate that depends on `zuqe` runs it against its own tree, whether `zuqe`
comes from crates.io, git, or a path. No cargo feature, no extra dependency, no
`debug_assertions` gate — it is ordinary library code, compiled like the rest (it is never called
at runtime).

### Standard layout (required)

Adopting the audit means adopting its three fixed pieces — the test, the exemption file and the
CLI agree on them:

| Path (in your crate) | Purpose |
|---|---|
| `tests/gc_audit.rs` | the audit test: one `zuqe::gc_audit_test!(<root floor>);` call |
| `gc_audit_exemption.txt` | **the only place exemptions live**; read by the test and by `zuqe-gc-audit` |
| `<crate>/src` | the scanned tree |

`gc_audit_exemption.txt` sits next to `Cargo.toml` (a sibling of `src/`) and is the single
source of truth: never repeat exemptions inline in the test, and never keep a second list for
the CLI. A crate with no exemptions may omit the file (the test then runs with an empty list);
once the first entry exists it must go in this file.

Format: one entry per line, `#` comments and blank lines skipped:

```text
# Type.field — reason
MyPayload.cache — construction only
MyPayload.handler — weak target; not traced
```

Every entry is a reviewed claim that the field needs no write barrier — one of the three
legitimate kinds under Rule 5's checklist. The test fails on **stale** entries (field renamed,
wrapped or gone), so the file cannot rot.

### Boilerplate

`tests/gc_audit.rs` is one macro call — no `Path`, no assert, no cfg:

```rust
zuqe::gc_audit_test!(30);
```

Run it with `cargo test --test gc_audit`. The macro reads `gc_audit_exemption.txt`, expands to
the standard `gc_audit_payload_fields` test, and runs `zuqe::gc_audit::test_failures(src,
allowlist, min_roots)` — rules 1–3 (bare/unbound GC fields with `file:line`, container element
types without `impl GcBind`, whole-field guard replacement, stale allowlist entries) plus the
anti-vacuous root-set floor. The generated test name is fixed and neutral so every crate's
report reads the same. (The two-argument form `zuqe::gc_audit_test!(LIST, 30)` takes an inline
slice; that is an escape hatch for one-off tests, not the standard layout.)

`min_roots` is a floor on how many payload root types your `src` tree must yield — it stops a
scan of a wrong or empty directory from passing. Start with a rough guess: if it is too high,
the failure prints your actual count (`root set suspiciously small: N entries`) — set the floor
somewhat below N.

Requires a zuqe version that ships `gc_audit`: if `zuqe::gc_audit_test!` does not resolve, your
zuqe predates the audit — upgrade. The macro scans `<your crate>/src` (the test file's manifest
dir); a crate with a different layout calls `zuqe::gc_audit::test_failures` directly — see
Discovery helpers.

### Workflow: red → green

1. Run `cargo test --test gc_audit` — every reported line is a field to either fix in code (wrap
   it, or add an `impl GcBind` for the element type) or justify with an entry added to
   `gc_audit_exemption.txt`;
2. keep that file short and honest — each entry is a visible diff a reviewer accepts, and the
   test also fails on **stale** entries, so the list cannot rot;
3. green means shape only: an entry is a claim you verified against the write-point checklist of
   Rule 5, not proof that no late write exists.

### Command-line tool

`zuqe-gc-audit` (`src/bin/zuqe-gc-audit.rs`) prints the same inventory and audit for any source
tree as a report instead of a test — the payload root set as a type list with `file:line` and,
per field, its verdict and `bare`/`wrapped`/`container` status: `✅` passes, `⭕` allowlisted,
`❌` violation. The last line counts the fields that fail the audit. It reads the **same**
`gc_audit_exemption.txt` as the test (default: in the nearest directory at or above `SRC_DIR`
that contains a `Cargo.toml`), so the report and `cargo test` always agree. Build the
distributable binary with:

```text
cargo build --release --bin zuqe-gc-audit
# → target/release/zuqe-gc-audit — standalone; point it at any crate:
./target/release/zuqe-gc-audit path/to/crate/src
./target/release/zuqe-gc-audit --allowlist other.txt path/to/crate/src   # override
```

`SRC_DIR` defaults to `./src`. Exit status: `0` clean (with the crate's exemption file applied),
`1` violations (so it can gate scripts), `2` usage error. In-tree,
`cargo run --bin zuqe-gc-audit -- [SRC_DIR]` runs the same tool with the dev profile.

### Discovery helpers

- `gc_leaf_types(dir)` — every struct/enum that *directly* declares a GC-leaf field, with
  `file:line` and the field's `wrapped`/`container` status: the discovery inventory (it covers
  every definition, including ones the audit does not reach). It returns plain data, so a test
  or any binary of yours can print it;
- `field_verdicts(dir, allowlist)` — the audit verdict per field, keyed by `Type.field`:
  `Pass` / `Allowlisted` / `Violation(reason)`. Same root-set walk as `gc_field_violations`,
  which is what `zuqe-gc-audit` prints as `✅` / `⭕` / `❌`;
- `payload_root_types(dir)` — the audit's root set; `test_failures` already applies the floor,
  or assert on it directly;
- `read_exemptions(path)` — read and parse an exemption file (one entry per line, `#` comments
  skipped); the parser behind both the macro and `zuqe-gc-audit`;
- `test_failures(dir, allowlist, min_roots)` — exactly what the macro runs, if you want the
  failure lines instead of a panicking test;
- `all_violations(dir, allowlist)`, `gc_field_violations(dir, allowlist)` and
  `guard_construction_violations(dir)` — the pieces, if you prefer your own shape.

### Limits

The scanner is a deliberate text heuristic (no `syn`): unknown field types are skipped rather
than guessed, only *direct* GC-leaf fields are listed by `gc_leaf_types` (nested local types are
covered by the root-set walk of `gc_field_violations`), and a payload type unreachable from the
root set is invisible. It reports shape, never write timing.

Reference implementations: zuqe's own `tests/gc_audit.rs` (its allowlist plus a payload-set
check) and aike's `tests/gc_audit.rs` (the one-liner) — both run the same standard test.

## Customized GC tracer

In case your native class or module have garbage collectable senario, you may register a customized gc trace handler to trace your gc'ed nodes.

```rust
fn main() -> ExitCode {
    // 1. Create runtime
    let mut rt = Runtime::create().unwrap();

    // 2. Register a customized GC trace handler
    rt.gc_set_trace_handler(|tracer| {
        mod_os_gc_trace(tracer);
    });

    // 3. Create context and proceed with execution
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)
        })
        .unwrap();

    // ... rest of your application setup
}
```

See code in `examples/zjs/mod_os.rs` to find how customized GC is used.

```rust
use gc_lite::{GcTrace, GcTraceCtx};

pub fn mod_os_gc_trace(tr: &mut GcTraceCtx) {
    // ... omitted ...
}
```

`gc_lite` is re-exported by `zuqe` (`use zuqe::gc_lite::{GcTrace, GcTraceCtx};`), and any `JSValue` field you keep alive must call `value.trace(tr)` inside your trace function so it participates correctly in Zuqe's garbage collection.
