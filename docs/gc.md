# Garbage Collection

Zuqe uses a tracing and mark-sweep memory management approach that combines Rust's ownership system with JavaScript's garbage collection.

## GC Scopes

`ScopedContext` represents a **GC scope** — a lightweight, nestable sub-region within a `Context`. Scopes form a stack: each call to `with_new_scope` pushes a child scope on top of the current one.

### Why scopes?

Scope-local GC allocations are **temporarily protected** — the GC treats them as reachable roots for the lifetime of that scope, so they won't be freed even if nothing else references them. This avoids the need for Rust references or `GcRef` handles in short-lived operations:

```rust
// A simple scope: create and use a temporary array, discard when done
ctx.with_new_scope(|mut scope| {
    let arr = Array::create(&mut scope)?;          // protected by this scope
    arr.define_property_value_u32(&mut scope, 0, JSValue::Int(42), JS_PROP_C_W_E)?;
    // arr is automatically released when this scope closes
    scope.return_with(Ok(JSValue::UNDEFINED))
});
```

### Scope stack and promotion

Scopes are nested like a stack. When the top-most scope exits, its **return value** is promoted to the parent scope's protection — any GC nodes embedded in that value remain safe. All other scope-local nodes lose their temporary protection and become eligible for GC (freed only if no other references exist):

```rust
fn compute_result(ctx: &mut Context) -> Result<Vec<JSValue>, i32> {
    ctx.with_new_scope(|mut scope| {
        // These arrays are local — protected only inside this scope
        let tmp1 = Array::create(&mut scope)?;
        let tmp2 = Array::create(&mut scope)?;
        // ... do work with tmp1, tmp2 ...

        // Only 'result' survives the scope exit
        let result = vec![JSValue::Int(1), JSValue::Int(2)];
        scope.return_with(Ok(result))
    })  // tmp1, tmp2 lose protection here; 'result' is promoted to parent
}
```

### Nested scopes

You can nest scopes to fine-tune lifetime boundaries:

```rust
ctx.with_new_scope(|mut outer| {
    let outer_val = JSValue::Int(1);

    outer.with_new_scope(|mut inner| {
        let inner_val = JSValue::Int(2);
        // Both outer_val and inner_val are protected here
        inner.return_with(())
    }); // inner_val loses protection here; outer_val still protected

    outer.return_with(outer_val)
}); // outer_val promoted to parent scope
```

### `with_root_scope`

The root scope (depth = 1) lives for the **entire lifetime** of the `Context`. Any GC node promoted into it is **permanently reachable** — never collected until the `Context` itself is dropped:

```rust
ctx.with_root_scope(|mut scope| {
    let val = JSValue::Int(42);
    // val is now in the root scope — it will never be GC'd
});
```

For long-running applications, avoid accumulating values in the root scope. Use `with_new_scope` for temporary work so that intermediate values are released when the child scope exits.

> **Notice**: Any value placed into the root scope will **persist forever** — even if nothing references it, the GC will not reclaim it, because the root scope acts as a permanent GC root set. If you know a value in the root scope is no longer needed, you **must** manually call `scope.remove_local(node)` to remove it from the scope's protection. Otherwise it will leak until the `Context` itself is dropped.

### `return_with`

When returning a value from `with_new_scope`, you **must** call `scope.return_with(value)` instead of a plain `value`. This triggers the promote mechanism: GC nodes in the return value are moved to the parent scope's cache before the child scope is destroyed:

```rust
ctx.with_new_scope(|mut scope| {
    let obj = scope.create_base_object()?;
    // promote obj to parent before this scope drops
    scope.return_with(Ok(obj))
})
```

The `#[must_use]` wrapper ensures you cannot forget to propagate the return value through the scope boundary.

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
