# Running Loop

The running loop is the core execution mechanism in the Zuqe runtime that manages the lifecycle of JavaScript execution, from simple scripts to complex asynchronous operations. This document explains how the loop works, starting from the simplest `run_wait()` function to complex execution scenarios.

## 1. Basic Execution with Context::run_wait()

The simplest way to execute JavaScript code is through the `run_wait()` method:

```rust
// ...

let mut ctx = rt.create_context_ecma().unwrap();

match ctx.run_wait(
    script.as_bytes(), // source script
    &filename,         // file name
    EvalType::Global,  // eval as global
    EvalFlags::empty(),
) {
    Ok(val) => {
        println!("Result: {}", val.inspect(&ctx).unwrap());
    }
    Err(e) => {
        println!("Error: {}", e.message);
    }
}
```

### How Context::run_wait() works

1. **Script Compilation**: The source code is compiled into bytecode
2. **Execution**: The script is executed in the given context
3. **Promise Handling**: If the script returns a Promise, this function waits for it to resolve
4. **Result Return**: The final value is returned to the caller

Internally, this method works like this:

```rust
fn run_wait_inner(&mut self, source: &[u8], filename: &str,
                  eval_type: EvalType, eval_flags: EvalFlags)
    -> Result<JSValue, ErrorText> {

    // 1. Execute the script
    let val = self.eval_global(source, filename, eval_flags)?;

    // 3. If returned promise, wait for completion
    if let Ok(p) = Promise::try_from(val) {
        loop {
            match rt.tick() {
                Ok(TickStatus::Continue) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
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

## The Tick

The tick system is the heart of the running loop. Every execution cycle is driven by the `tick()` function. Each `tick()` call returns the status to indicate there need more ticks or stop:

```rust
pub enum TickStatus {
    Continue,    // More work to do
    Stop(i32),   // Execution should stop
}
```

### Customized tick handler

Tick callback allow you to register custom functions that run on every tick cycle. They're useful for implementing periodic tasks and async event handling.

Use `Context::add_tick_handler()` method to register a customized tick handler.

```rust
// ...

let mut ctx = rt.create_context_ecma().unwrap();

// Register a customized tick handler for periodic tasks.
ctx.add_tick_handler(|cx| -> bool {
    // returns true to indicate more ticks are needed.
    my_tick_poll(cx)
});

// ...
```

### Handler Execution

Each tick handler receives the current context and returns a boolean:

- `true`: Continue ticking (more work pending)
- `false`: No more work from this handler

Following is the code snippet extracted from example `zjs`, it registers a customized tick handler performed internally module native:

```rust
pub fn init_mod_os(mut ctx: Context) -> Result<(), i32> {
    ctx.register_module("os", MOD_OS_ENTRIES)?;
    ctx.add_tick_handler(|cx| -> bool { js_os_poll(cx) });
    Ok(())
}
```

The tick handler registration design is especially useful for:

- **Asynchronous Event Handling**: Processing events from native APIs
- **Periodic Tasks**: Running background tasks at regular intervals
- **I/O Operations**: Handling file descriptor readiness and network events

## 5. An example of complex loop

Looking at `examples/zjs/main.rs`, here's how a complex application sets up its execution loop:

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

    // 4. Create context
    let mut ctx = rt.create_context_ecma().unwrap();

    // 5. Register modules and classes
    init_mod_std(ctx).unwrap();
    init_mod_os(ctx).unwrap();
    setup_globals(ctx).unwrap();

    // 6. Execute script with run_wait
    match ctx.run_wait(
        script.as_bytes(),
        &filename,
        if is_module { EvalType::Module } else { EvalType::Global },
        EvalFlags::empty(),
    ) {
        Ok(val) => {
            colorize_println(val.inspect(&ctx).unwrap(), GreenFg);
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
}
```

This running loop system provides a robust foundation for executing JavaScript code in the Zuqe runtime, handling everything from simple scripts to complex asynchronous applications with promises, timers, and custom event handlers.
