# Running Loop

The running loop is the core execution mechanism in the Zuqe runtime that manages the lifecycle of JavaScript execution, from simple scripts to complex asynchronous operations. When the `async_support` feature is enabled (the default), the **recommended** way to drive the loop is `Context::eval_wait()` as an async function (section 1). The lower-level `Runtime::run_async()` and the synchronous API are documented afterwards as a reference for hosts that cannot use an async runtime.

## 1. Async runtime mode (`async_support` feature)

When the `async_support` Cargo feature is enabled (by default), `Context` exposes an **async** `eval_wait()` that evaluates a script (or module) and drives the event loop to completion in a single `.await`:

```rust
use zuqe::{
    Context, EvalFlags, EvalName, EvalType, Exception, ExitStatus, Runtime, ScopedContext,
};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(|cx| ScopedContext::ECMA_CONTEXT_SETUP(cx))
        .unwrap();

    // The entry name must be a canonical `file://` URL: `module_name`
    // (and `import.meta.url`) is a URL, and a `file://` base is also required
    // so that dynamic `import()` from a script entry resolves relative
    // specifiers correctly.
    let entry_url =
        zuqe::path_to_url("example.js").expect("entry path must convert to a file:// URL");

    let run_result = ctx
        .eval_wait(
            &mut rt,
            b"Promise.resolve(42)",
            &EvalName::BaseUrl(entry_url),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await;

    match run_result {
        ExitStatus::Value(v) => {
            // A value produced by the executed script (a module entry, or a
            // script with a meaningful tail value; `Promise.resolve(42)` above
            // settles to `undefined` at the top level).
            println!("result: {:?}", v);
            std::process::ExitCode::SUCCESS
        }
        ExitStatus::Interrupted(code) => code,
        ExitStatus::Exception(Exception::OutOfMemory) => {
            eprintln!("out of memory");
            std::process::ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                eprintln!(
                    "{}",
                    err.to_string(&mut cx).unwrap().content().encode_utf8(false)
                );
            });
            std::process::ExitCode::FAILURE
        }
        ExitStatus::Exception(_) => std::process::ExitCode::FAILURE,
    }
}
```

This async `eval_wait` handles **top-level Promise observation** internally. A module entrypoint evaluates to a Promise; a rejected top-level promise is surfaced as `ExitStatus::Exception(Exception::Error(reason))` so the host can exit non-zero — the rejection is never silently dropped. This mirrors the synchronous `eval_wait` semantics exactly.

Alternatively you can drive the loop at a lower level yourself:

- `Runtime::run_async()` returns a `RunFuture` (a Rust `Future` with output `ExitStatus`) that repeatedly ticks the runtime until no more work is pending.
- Convert a returned JS `Promise` to a Rust `Future` via `Promise::into_future()` and `await` it alongside `rt.run_async()` (usually with `tokio::join!`). See [Basic Usage](basic.md) and the [Promise documentation](promise.md).

## 2. The Tick

The tick is the heart-beat of the running loop — both in the async mode (where `RunFuture` repeatedly calls `tick()`) and in the synchronous mode. Each `tick()` runs, in order:

1. **One-shot jobs** (always complete, never retry) — promise reactions, timer callbacks.
2. **Polling jobs** (one round only) — jobs that may return `Again` to be re-polled on a later tick (e.g. `Promise::create_with_job`, channel- or slot-driven promises).
3. **Customized tick handlers** registered via `ScopedContext::add_tick_handler()`.

The `tick()` function and its `TickStatus` are internal to the runtime — hosts drive the loop through `eval_wait()` / `run_async()`, they never call `tick()` directly.

### Customized tick handler

Tick callbacks let you register custom functions that run on every tick cycle. They're useful for implementing periodic tasks and async event handling.

Use `ScopedContext::add_tick_handler()` to register a customized tick handler:

```rust
fn register_ticks(ctx: &mut ScopedContext) {
    // Register a customized tick handler for periodic tasks.
    ctx.add_tick_handler(|cx| -> bool {
        // returns true to indicate more ticks are needed.
        my_tick_poll(cx)
    });
}
```

### Handler Execution

Each tick handler receives the current context and returns a boolean:

- `true`: Continue ticking (more work pending)
- `false`: No more work from this handler

Following is the code snippet extracted from example `zjs`, which registers a customized tick handler inside a native module:

```rust
pub fn init_mod_os(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("os", MOD_OS_ENTRIES)?;
    ctx.add_tick_handler(|cx| -> bool { js_os_poll(cx) });
    Ok(())
}
```

The tick handler registration design is especially useful for:

- **Asynchronous Event Handling**: Processing events from native APIs
- **Periodic Tasks**: Running background tasks at regular intervals
- **I/O Operations**: Handling file descriptor readiness and network events

## 3. Synchronous mode (reference)

When `async_support` is disabled — or when a host cannot use an async runtime —
the loop runs synchronously. `Context::eval_wait()` blocks, polling the internal
tick loop until all pending microtasks and Promise jobs have finished or the
execution is interrupted.

### 3.1 Basic Execution with eval_wait()

```rust
use zuqe::{Context, EvalFlags, EvalName, EvalType, Exception, ExitStatus, ScopedContext};

// ...

let exit_status = ctx.with_root_scope(|mut scope| {
    scope.eval_wait(
        script.as_bytes(),                   // source script
        &EvalName::Name("<script>".into()),  // file name
        EvalType::Global,                    // eval as global
        EvalFlags::empty(),
    )
});

match exit_status {
    ExitStatus::Value(v) => {
        // A synchronous result value produced by the script.
    }
    ExitStatus::Interrupted(code) => {
        // The script called std.exit(code) or the host interrupted it.
    }
    ExitStatus::Exception(Exception::Error(err)) => {
        // An uncaught error; `err` is the JS error value.
    }
    _ => {}
}
```

The synchronous `eval_wait` works like this:

1. **Compile & Execute**: The source is compiled into bytecode and run in the given context.
2. **Promise Handling**: If the script returns a Promise, it keeps ticking until the Promise settles or the loop is interrupted.
3. **Result Return**: The final outcome is returned as an `ExitStatus`.

### 3.2 A complex synchronous loop example

Looking at `examples/zjs/main.rs` (the `#[cfg(not(feature = "async_support"))]` variant), here's how a complex application sets up its execution loop:

```rust
fn main() -> ExitCode {
    let mut rt = Runtime::create().unwrap();

    // 1. Set up module loader
    rt.set_module_loader(js_module_loader);

    // 2. Configure GC
    rt.gc_set_trace_handler(|tracer| {
        mod_os_gc_trace(tracer);
    });

    // 3. Create context and register modules/classes
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            init_mod_std(cx)?;
            init_mod_os(cx)?;
            setup_globals(cx)
        })
        .unwrap();

    // 4. Execute script with eval_wait inside the root scope
    let entry_name =
        zuqe::path_to_url(&filename).expect("entry path must convert to a file:// URL");

    let exit_code = ctx.with_root_scope(|mut scope| {
        scope.eval_wait(
            script.as_bytes(),
            &EvalName::BaseUrl(entry_name),
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            EvalFlags::empty(),
        )
    });

    match exit_code {
        ExitStatus::Value(_) => ExitCode::SUCCESS,
        ExitStatus::Interrupted(code) => code,
        ExitStatus::Exception(Exception::Error(err)) => {
            let msg = err.to_string(&mut ctx.root_scope());
            eprintln!("{}", msg.unwrap().content().encode_utf8(false));
            ExitCode::FAILURE
        }
        _ => ExitCode::FAILURE,
    }
}
```

This running loop system provides a robust foundation for executing JavaScript code in the Zuqe runtime, handling everything from simple scripts to complex asynchronous applications with promises, timers, and custom event handlers.
