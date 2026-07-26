# Running Loop

The running loop is the core execution mechanism in the Zuqe runtime that manages the lifecycle of JavaScript execution, from simple scripts to complex asynchronous operations. When the `async_support` feature is enabled, the **recommended** way to drive the loop is as a Rust `Future` (section 1). The older synchronous APIs (`eval_wait()` and manual `tick()`) are documented at the end as a reference for hosts that cannot use an async runtime.

## 1. Async runtime mode (`async_support` feature)

When the `async_support` Cargo feature is enabled (by default), you can drive
the runtime as a Rust `Future` with `Runtime::run_async()`. This allows your
host application to `await` the runtime alongside other concurrent work in a
Tokio (or other async runtime) context.

```rust
use zuqe::{EvalFlags, EvalType, Promise};

async fn run_in_async_context(mut rt: Runtime, mut ctx: Context) {
    let mut promise_future = None;

    ctx.with_new_scope(|mut scope| {
        let val = scope.eval(
            b"Promise.resolve(42)",
            "example.js",
            EvalType::Global,
            EvalFlags::empty(),
        )?;

        if let Ok(promise) = Promise::try_from(val) {
            promise_future = Some(promise.into_future(&mut scope)?);
        }
        Ok(())
    });

    if let Some(js_fut) = promise_future {
        // Drive the runtime loop and the JS Promise concurrently
        let (_rt_res, promise_res) = tokio::join!(rt.run_async(), js_fut);
        // The JS Promise has now settled
    }
}
```

In this mode:

- The runtime’s internal tick loop is driven by `RunFuture` (the return type
  of `run_async()`), which integrates with the Rust async executor’s waker
  mechanism.
- JS Promises are converted to Rust `Future`s via `Promise::into_future()`.
- `tokio::join!` (or `futures::join!`) runs them both until completion.

## 2. The Tick

The tick system is the heart-beat of the running loop — both in the async mode (where `RunFuture` repeatedly calls `tick()`) and in the synchronous mode. Each `tick()` call returns a status indicating whether more ticks are needed or the loop should stop:

```rust
pub enum TickStatus {
    Continue,    // More ticks
    Stop(i32),   // Execution should stop
}
```

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
the loop runs synchronously. The host calls `eval_wait()` (which polls `tick()`
internally), or manually loops on `rt.tick()`.

### 3.1 Basic Execution with eval_wait()

The simplest way to execute JavaScript code is the `eval_wait()` method on a scoped context:

```rust
// ...

ctx.with_new_scope(|mut scope| {
    match scope.eval_wait(
        script.as_bytes(), // source script
        &filename,         // file name
        EvalType::Global,  // eval as global
        EvalFlags::empty(),
    ) {
        Ok(val) => {
            println!("Result: {}", val.inspect(&mut scope).unwrap());
        }
        Err(e) => {
            println!("Error: {}", e.message);
        }
    }
});
```

### How eval_wait() works

1. **Script Compilation**: The source code is compiled into bytecode
2. **Execution**: The script is executed in the given context
3. **Promise Handling**: If the script returns a Promise, this function waits for it to resolve
4. **Result Return**: The final value is returned to the caller

Internally, this method works like this:

```rust
pub fn eval_wait(
    &mut self,
    source: &[u8],
    filename: &str,
    eval_type: EvalType,
    eval_flags: EvalFlags,
) -> Result<JSValue, ErrorText> {
    // 1. Execute the script
    let val = self
        .eval_internal(source, filename, eval_type, eval_flags)
        .map_err(|_| {
            let exc = self.rt_mut().take_exception();
            ErrorText::from(self, exc)
        })?;

    // 2. If returned promise, wait for completion
    if let Ok(p) = Promise::try_from(val) {
        loop {
            match self.rt_mut().tick() {
                Ok(TickStatus::Continue) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Ok(TickStatus::Stop(_)) => {
                    // Promise resolved or rejected
                    break;
                }
                Err(_) => {
                    // Error occurred
                    break;
                }
            }
        }
    }

    Ok(val)
}
```

### 3.2 A complex synchronous loop example

Looking at `examples/zjs/main.rs` (synchronous variant), here's how a complex
application sets up its execution loop:

```rust
fn main() -> ExitCode {
    // 1. Create runtime
    let mut rt = Runtime::create().unwrap();

    // 2. Set up module loader
    rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());

    // 3. Configure GC
    rt.gc_set_trace_handler(|tracer| {
        mod_os_gc_trace(tracer);
    });

    // 4. Create context and register modules/classes
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            init_mod_std(cx)?;
            init_mod_os(cx)?;
            setup_globals(cx)
        })
        .unwrap();

    // 5. Execute script with eval_wait inside a scope
    let exit_code = ctx.with_new_scope(|mut scope| -> ExitCode {
        let ret = scope.eval_wait(
            script.as_bytes(),
            &filename,
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            EvalFlags::empty(),
        );

        match ret {
            Ok(val) => {
                colorize_println(val.inspect(&mut scope).unwrap(), GreenFg);
                ExitCode::SUCCESS
            }
            Err(e) => {
                let mut err_msg = e.message;
                if let Some(tb) = e.traceback {
                    err_msg = format!("{err_msg}\n{tb}");
                }
                colorize_println(err_msg, RedFg);
                ExitCode::FAILURE
            }
        }
    });

    exit_code
}
```

This running loop system provides a robust foundation for executing JavaScript code in the Zuqe runtime, handling everything from simple scripts to complex asynchronous applications with promises, timers, and custom event handlers.
