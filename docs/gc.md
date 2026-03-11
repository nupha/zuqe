# Garbage Collection

Zuqe uses a tracing and mark-sweep memory management approach that combines Rust's ownership system with JavaScript's garbage collection.

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
            Context::ECMA_CONTEXT_SETUP(cx)
        })
        .unwrap();

    // ... rest of your application setup
}
```

See code in `examples/zjs/mod_os.rs` to find how customized GC is used.

```rust
use gc_lite::{GcTracable, GcTracer};

pub fn mod_os_gc_trace(tracer: &mut GcTracer) {
    // ... omitted ...
}
```
