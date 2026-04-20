# Promise API Overview

This document describes the public Promise API of Zuqe, and how to integrate it with Rust async/await code. It focuses on how to _use_ the API; internal details are only sketched where they help understanding.

Zuqe keeps ECMAScript Promise semantics (microtasks, reaction ordering, error handling) and provides host-facing helpers in Rust to:

- Construct and settle Promises from native code.
- Drive long-running background jobs.
- Bridge between Rust `Future` and JS `Promise` in both directions.

All Promise-related types are available through:

```rust
use zuqe::builtins::promise::Promise;
```

## Core Promise type

The internal Promise type is:

```rust
pub struct Promise(GcRef<ObjectImpl>);
```

with an observable state and result:

```rust
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

impl Promise {
    pub fn state(&self) -> PromiseState;
    pub fn result(&self) -> JSValue;
}
```

These helpers are mainly useful on the host side (for example in tests or diagnostics) and are not exposed to JS.

On the JS side, Promise behaves as specified by ECMAScript and is exposed as the global `Promise` constructor with:

- Static methods: `Promise.resolve`, `Promise.reject`, `Promise.all`, `Promise.allSettled`, `Promise.any`, `Promise.race`, `Promise.withResolvers`.
- Instance methods: `then`, `catch`, `finally`.

## Creating Promises from Rust

### `Promise::create`

Low-level constructor used internally:

```rust
impl Promise {
    pub fn create(ctx: &mut ScopedContext) -> Result<(Promise, [JSValue; 2]), i32>;
}
```

This returns:

- A `Promise` in `Pending` state.
- A pair of resolving functions `[resolve, reject]` as JS values.

You can then call `resolve` or `reject` from native code to settle the Promise. In most host code you will use higher-level helpers instead.

### `Promise::create_with_job`

Create a Promise whose lifecycle is driven by a background job executed by the runtime’s microtask queue:

```rust
impl Promise {
    pub fn create_with_job(
        ctx: &mut ScopedContext,
        job: impl Fn(&mut ScopedContext) -> Result<Option<JSValue>, i32> + 'static,
    ) -> Result<Promise, i32>;
}
```

The `job` callback is called from the JS runtime thread in the microtask loop and should return:

- `Ok(Some(value))`  
  Resolve the Promise with `value` and mark the job as finished.
- `Ok(None)`  
  The job is not done yet; it will be scheduled again on the next tick.
- `Err(err_code)`  
  The job failed. You are expected to set the current exception on `ctx` and return a non-zero error code. The Promise is rejected with that exception.

Typical pattern (polling some host state until it is ready):

```rust
let promise = Promise::create_with_job(ctx, move |cx| -> Result<Option<JSValue>, i32> {
    match try_get_result_from_host(cx) {
        Ok(Some(val)) => Ok(Some(val)),      // resolve
        Ok(None) => Ok(None),               // run again later
        Err(_) => {
            // set exception on cx
            Err(-1)                         // reject
        }
    }
})?;
```

Use this when:

- The data source is polled from the JS runtime thread.
- You do not need Rust `Future` or external async runtimes.

### `Promise::create_from_sender`

Create a Promise that is completed by sending a value through a channel from any thread or async runtime:

```rust
impl Promise {
    pub fn create_from_sender<D, Conv>(
        ctx: &mut ScopedContext,
        convert: Conv,
    ) -> Result<(Promise, std::sync::mpsc::SyncSender<D>), i32>
    where
        D: Send + 'static,
        Conv: FnOnce(&mut ScopedContext, D) -> JSResult + 'static;
}
```

This helper composes three concerns:

- Use `SyncSender<D>` to receive a host-level result `D` from any thread or executor.
- From the JS runtime thread, convert `D` into a `JSValue` using `convert`.
- Resolve or reject the Promise inside the normal Promise job queue.

Semantics:

- On the JS side, you just see a normal `Promise`.
- On the host side:
  - You get `(promise, tx)`.
  - You run your async work anywhere (thread, tokio, async-std).
  - When done, you call `tx.send(data)` with `data: D`.
  - At some later tick, the job will:
    - Call `convert(ctx, data)`; on success the Promise is fulfilled with the returned `JSValue`.
    - If `convert` returns `Err(-1)` with an exception set on `ctx`, the Promise is rejected.

Example: bridge a background thread computing a `LlamaModel`:

```rust
let (promise, tx) = Promise::create_from_sender(ctx, |cx, model: LlamaModel| {
    let obj = JSLlamaModel::new_instance(cx, model)?;
    Ok(JSValue::Object(obj))
})?;

std::thread::spawn(move || {
    let result: Result<LlamaModel, String> = load_model_from_disk();
    if let Ok(model) = result {
        let _ = tx.send(model);
    } else if let Err(msg) = result {
        // You can choose to encode errors into D and convert them in the closure
        let _ = tx.send(LlamaModel::from_error(msg));
    }
});

// JS: const model = await host.loadModel(...);
```

Example: integrate `tokio::fs::read`:

```rust
let (promise, tx) = Promise::create_from_sender(ctx, |cx, res: std::io::Result<Vec<u8>>| {
    match res {
        Ok(bytes) => {
            let obj = JSFileData::new_instance(cx, bytes)?;
            Ok(JSValue::Object(obj))
        }
        Err(e) => {
            cx.throw_internal_error(&e.to_string());
            Err(-1)
        }
    }
})?;

let path = path_string.clone();
tokio::spawn(async move {
    let result = tokio::fs::read(path).await;
    let _ = tx.send(result);
});

// JS: const data = await readFile(path);
```

Use `create_from_sender` when:

- Your async work is naturally expressed as a Rust `Future` or blocking code on some runtime.
- You want a typed channel from host world (`D`) into JS world, and you need to convert `D` into `JSValue` on the JS runtime thread.

## Promise ↔ Future bridge

The async support module (`async_support.rs`, enabled with the `async_support` Cargo feature) provides helpers to translate between Rust `Future` and JS `Promise`.

### Promise → Future: `Promise::into_future`

Convert a JS Promise into a Rust `Future` that can be `await`ed in host code:

```rust
impl Promise {
    pub fn into_future(
        self,
        ctx: &mut ScopedContext,
    ) -> Result<PromiseFuture, i32>;
}

pub struct PromiseFuture {
    // holds state and waker internally
}

impl Future for PromiseFuture {
    type Output = Result<JSValue, JSValue>;
    // poll implementation registers waker and waits for Promise settlement
}
```

Usage example:

```rust
async fn call_js_compute(rt: &mut Runtime) -> JSResult<JSValue> {
    let mut ctx = rt
        .create_context(|cx| {
            Context::ECMA_CONTEXT_SETUP(cx)?;
            Ok(())
        })
        .unwrap();

    let promise_value = ctx.with_new_scope(|mut scope| {
        let code = b"import { compute } from './mod.mjs'; compute(21);";
        let val = scope.eval_wait(code, "main.mjs", EvalType::Module, EvalFlags::empty())?;
        Ok(val)
    })?;

    let promise = promise_value.as_promise().unwrap();
    let js_future = promise.into_future(&mut ctx)?;

    let result = js_future.await?;
    Ok(result)
}
```

This lets you treat JS async functions as plain Rust `Future`s:

- JS side: `export async function compute(x) { return x * 2; }`
- Rust side: `let value = promise.into_future(&mut ctx)?.await?;`

Internally, `into_future`:

- Attaches `then`/`catch` handlers to the Promise.
- Stores the result or rejection in a shared state and wakes the stored `Waker`.
- Resolves the Rust `Future` when the Promise settles.

### Future → Promise: `Promise::create_from_future`

Wrap host-side Rust async operations as JS Promises:

```rust
impl Promise {
    pub fn create_from_future<F>(ctx: &mut ScopedContext, fut: F) -> Result<Self, i32>
    where
        F: FnOnce(Box<dyn FnOnce(Result<JSValue, JSValue>) + 'static>),
   ;
}
```

This helper is designed to be runtime-agnostic: it does not take a `Future` directly, but a registration closure `fut` that receives a `notify` function:

- `notify(Ok(value))` – request to fulfill the Promise with `value`.
- `notify(Err(reason))` – request to reject the Promise with `reason`.

Typical usage with a Rust async function:

```rust
fn js_fetch_config(ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    let promise = Promise::create_from_future(ctx, |notify| {
        // The user decides which runtime to use
        tokio::spawn(async move {
            let res: Result<JSValue, JSValue> = fetch_config_async().await;
            notify(res);
        });
    })?;

    Ok(promise.into())
}
```

In tests, `create_from_future` can be exercised by manually polling an async function’s Future once and calling `notify` with the result.

Internally, `create_from_future`:

- Constructs a `Promise` and its resolve/reject functions.
- Stores an internal result slot.
- Schedules a job that:
  - Checks whether a result has been written by `notify`.
  - If so, resolves or rejects the `Promise` accordingly and completes the job.
  - Otherwise, returns `JobState::Again` to be polled again in a later tick.

This design keeps the JS Promise semantics inside Zhuque’s job + tick system, while allowing the host to decide how to schedule and execute the actual Rust async work.

## JS-level APIs

On the JS side, the runtime exposes a standard `Promise` constructor and methods, implemented over the internal `Promise` type:

- `new Promise(executor)` – uses `create_with_executor` internally, passing `resolve` and `reject` into the executor.
- `Promise.resolve(value)` / `Promise.reject(value)` – use `from_resolve_value` / `from_reject_value`.
- `Promise.all`, `Promise.allSettled`, `Promise.any`, `Promise.race` – implemented via microtask jobs and internal helpers in `class_promise.rs` and `settle.rs`.
- `Promise.withResolvers()` – returns an object `{ promise, resolve, reject }` built on top of `Promise::create_with_ctor`.
- `promise.then(onFulfilled, onRejected)` – attaches `PromiseReaction`s using the internal `reaction` mechanism.
- `promise.catch(onRejected)` and `promise.finally(onFinally)` – implemented in terms of `then`.

As a user of the Rust embedding API, you normally do not need to manipulate these directly; you use the Rust helpers to produce a `Promise` and then return it to JS code.

## Integration recap

When embedding Zuqe in a Rust application, you can mix JS Promise code and Rust async code as follows:

- JS → Rust:
  - Call JS async functions from Rust by:
    - Evaluating code that returns a Promise.
    - Converting the resulting `JSValue` to `Promise`.
    - Calling `Promise::into_future(&mut ctx)` and `await`ing the returned `Future`.
- Rust → JS:
  - Expose host async operations as JS Promises by:
    - Using `Promise::create_with_job` when the work is naturally polled on the JS thread.
    - Using `Promise::create_from_sender` when the work runs in a separate runtime or thread and results are sent through a channel.
    - Using `Promise::create_from_future` when you prefer a callback-based integration with existing `Future`-returning APIs.

In all cases, Promise settlement (state transitions and reaction execution) remains inside Zhuque’s job + tick microtask engine, preserving ECMAScript semantics while giving the host full control over how and where asynchronous work is actually performed.

## `async_support` feature and runtime modes

Promise–Future integration and async helpers are guarded by the Cargo feature `async_support`.

- When `async_support` is **enabled** (the default), Zuqe exposes:
  - `Runtime::run_async() -> RuntimeFuture<'_>` to drive the JS runtime as a `Future`.
  - `Promise::into_future` and `Promise::create_from_future` to bridge JS Promise and Rust async/await.
- When `async_support` is **disabled**, you can still run JS code and Promises, but the host drives execution explicitly by looping `rt.tick()` or using high-level helpers like `eval_wait`.

### Example: synchronous host without `async_support`

When the feature is disabled, the `examples/zjs` main entry point uses a synchronous flow:

```rust
#[cfg(not(feature = "async_support"))]
fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    let exit_code = ctx.with_new_scope(|mut scope| -> ExitCode {
        let result = scope.eval_wait(
            script.as_bytes(),
            &args.filename,
            if is_module { EvalType::Module } else { EvalType::Global },
            EvalFlags::empty(),
        );

        match result {
            Ok(val) => {
                colorize_println(val.inspect(scope.rt_mut()).unwrap(), GreenFg);
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

Here `eval_wait`:

- Evaluates the script.
- Drives the job + tick engine until all pending Promise jobs and microtasks are complete.
- Returns the final result value or error.

The host does not use Rust async/await; it simply blocks until the JS program (including all Promises) has finished.

### Example: async host with `async_support` enabled

With `async_support` enabled, `examples/zjs` switches to an async main and uses both `Promise::into_future` and `Runtime::run_async`:

```rust
#[cfg(feature = "async_support")]
#[tokio::main]
async fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    let mut promise_future = None;

    let mut exit_code = ctx.with_new_scope(|mut scope| -> ExitCode {
        let result = scope
            .eval(
                script.as_bytes(),
                &args.filename,
                if is_module { EvalType::Module } else { EvalType::Global },
                EvalFlags::empty(),
            )
            .map_err(|_| {
                let exc = scope.rt_mut().take_exception();
                ErrorText::from(&mut scope, exc)
            });

        match result {
            Ok(val) => {
                if let Ok(promise) = Promise::try_from(val) {
                    match promise.into_future(&mut scope) {
                        Ok(fut) => {
                            promise_future = Some(fut);
                            ExitCode::SUCCESS
                        }
                        Err(_) => {
                            let exc = scope.rt_mut().take_exception();
                            let err = ErrorText::from(&mut scope, exc);
                            let mut err_msg = err.message;
                            if let Some(tb) = err.traceback {
                                err_msg = format!("{err_msg}\n{tb}");
                            }
                            colorize_println(err_msg, RedFg);
                            ExitCode::FAILURE
                        }
                    }
                } else {
                    colorize_println(val.inspect(&mut rt).unwrap(), GreenFg);
                    ExitCode::SUCCESS
                }
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

    if let Some(js_fut) = promise_future {
        // In this example we want both: drive the runtime and also obtain the Promise result,
        // so we poll two futures together. If you only need to drive the runtime (and do not
        // care about the Promise result here), it is enough to await `rt.run_async()` alone.
        let (rt_res, promise_res) = tokio::join!(rt.run_async(), js_fut);

        if let Err(code) = rt_res {
            let msg = format!("runtime error: {code}");
            colorize_println(msg, RedFg);
            exit_code = ExitCode::FAILURE;
        }

        match promise_res {
            Ok(val) => {
                colorize_println(val.inspect(&mut rt).unwrap(), GreenFg);
            }
            Err(reason) => {
                let err_msg = reason
                    .inspect(&mut rt)
                    .unwrap_or_else(|_| "<exception>".into());
                colorize_println(err_msg.clone(), RedFg);
                exit_code = ExitCode::FAILURE;
            }
        }
    } else {
        let _ = rt.run_async().await;
    }

    exit_code
}
```

Key points:

- `#[tokio::main]` creates a Tokio runtime; `main` becomes an async function.
- The script is evaluated once. If the result is a JS Promise:
  - `Promise::into_future(&mut scope)` converts it to a Rust `Future`.
  - `tokio::join!(rt.run_async(), js_fut)` concurrently:
    - Drives the JS runtime’s job + tick engine (`rt.run_async()`).
    - Awaits the JS Promise result as a Rust `Future` (`js_fut`).
  - The final JS value (or rejection) is printed and mapped to an `ExitCode`.
- If the script does not return a Promise, the result is printed immediately, and `rt.run_async().await` is called once to flush any remaining microtasks.

This pattern shows how `async_support` lets you:

- Treat the entire JS runtime as a `Future` that can be awaited (`rt.run_async()`).
- Treat individual JS Promises as Rust Futures (`Promise::into_future`).
- Integrate both into a standard async Rust application (here driven by Tokio), keeping JS semantics intact while leveraging Rust’s async ecosystem.
