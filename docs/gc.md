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
        let obj = scope.create_base_object()?;
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

There are exactly **two** entry points through which Rust-held nodes can enter the reachability
graph — if a node isn't covered by one of them, it is unsafe:

### Rule 1 — class payloads holding GC nodes: provide `on_payload_gc`

When a `def_native_object!` payload stores GC nodes, you **MUST** pass the `on_payload_gc` option.
The callback signature is `fn(&PayloadType, &mut GcTraceCtx)`; the macro wires it to the class's
`on_gc_trace`, so the collector invokes it whenever an instance of that class is marked:

```rust
use zuqe::gc_lite::{GcTrace, GcTraceCtx};

def_native_object!(
    "Foo",
    JSFoo,
    FooPayload,
    FOO_PROTO,
    Some(ClassCtorDef::new(js_foo_constructor, 1, true)),
    on_payload_gc: FooPayload::gc_trace,
);

impl FooPayload {
    fn gc_trace(&self, gcx: &mut GcTraceCtx) {
        if let Some(ref o) = *self.field.borrow() {
            o.trace(gcx); // JSValue / Object / native wrappers all impl GcTrace
        }
    }
}
```

**Pitfalls:**

1. **`extends:` subclasses do NOT inherit the parent's gc trace.** GC looks the trace callback up
   by the object's **own** `class_id`, and `new_instance` creates objects with
   `Self::class_id()`. If a subclass reuses a payload that holds GC nodes, it MUST declare its own
   `on_payload_gc` — forgetting this is a silent UAF.
2. **Lazily-cached JS objects are GC nodes too.** A `RefCell<Option<Object>>` cached in the payload
   (e.g. `URL.searchParams`) must be traced once it is only reachable from that payload.

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

### Review checklist

**Rule 1 (class payload)**

- [ ] Does the payload contain GC nodes (`JSValue`/`Object`/native wrapper/callback handle)?
- [ ] If yes — is `on_payload_gc` provided, and does it trace every GC field?
- [ ] Is it an `extends:` subclass? If so, does the subclass carry its own `on_payload_gc`?

**Rule 2 (`thread_local`)**

- [ ] Any `thread_local!` holding GC nodes?
- [ ] If yes — is there a matching trace fn that covers every GC field?
- [ ] Is it registered in `gc_set_trace_handler` on every thread/runtime that holds it?

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
