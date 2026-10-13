# Promise

This document describes the Promise API of Zuqe, and how to integrate it with Rust async/await code. It focuses on how to _use_ the API; internal details are only sketched where they help understanding.

Zuqe keeps ECMAScript Promise semantics (microtasks, reaction ordering, error handling) and provides host-facing helpers in Rust to:

- Construct and settle Promises from native code.
- Drive long-running background jobs.
- Bridge between Rust `Future` and JS `Promise` in both directions.

All Promise-related types are available through:

```rust
use zuqe::builtins::promise::Promise;
```

## Core Promise type

The Promise type is:

```rust
pub struct Promise { /* ... */ }
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

### Direct state manipulation

In addition to the constructor-based helpers, `Promise` exposes methods to directly settle or chain a Promise from Rust:

```rust
impl Promise {
    /// Resolve (fulfill) the Promise with the given value.
    /// Triggers any chained reactions registered via `then()`.
    pub fn resolve(&mut self, ctx: &mut ScopedContext, value: JSValue);

    /// Reject the Promise with the given reason.
    /// Triggers any chained reactions registered via `then()`.
    pub fn reject(&mut self, ctx: &mut ScopedContext, value: JSValue);

    /// Attach success and failure handlers, returning a new wrapper Promise.
    /// Equivalent to JS `promise.then(success, fail)`.
    pub fn then(
        &mut self,
        ctx: &mut ScopedContext,
        success: JSValue,
        fail: JSValue,
    ) -> Result<Object, i32>;
}
```

Typical pattern (create, then resolve from native code):

```rust
let (promise, resolving_funcs) = Promise::create(ctx)?;

// ... later, in native code:
promise.resolve(&mut ctx, JSValue::from(42));
// or: promise.reject(&mut ctx, error_value);
```

The `then` method creates a new wrapper Promise (following ECMAScript `SpeciesConstructor` semantics) and registers the success/fail handlers as reactions. This is the same mechanism used by JS `promise.then()`, `promise.catch()`, and `promise.finally()`.

On the JS side, Promise behaves as specified by ECMAScript and is exposed as the global `Promise` constructor with:

- Static methods: `Promise.resolve`, `Promise.reject`, `Promise.all`, `Promise.allSettled`, `Promise.any`, `Promise.race`, `Promise.try`, `Promise.withResolvers`.
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
  The job is not done yet; it will be called again on the next tick.
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

### `Promise::create_with_receiver`

Zuqe exposes a pair of channel-based helpers: `Promise::create_with_receiver` and `Promise::create_with_channel`. `create_with_receiver` is the lower-level primitive: you provide an existing `WakeReceiver<D>`, and the Promise is driven by values arriving on that channel. `create_with_channel` is a convenience wrapper that allocates a new bounded channel internally and returns the sending side.

> **Note on channel types**: The raw `Sender<D>` / `Receiver<D>` type aliases (which map to `std::sync::mpsc` or `crossbeam_channel` depending on the `crossbeam_channel` Cargo feature) live in a private module and are re-exported only crate-internally — they are **not** public API.
>
> The public channel types are the newtypes `zuqe::WakeSender<D>` and `zuqe::WakeReceiver<D>`. A `WakeSender<D>` wakes the parked event loop on every successful `send` / `try_send` and on drop, so the polling job observes the value promptly; `WakeReceiver<D>` is the matching receiving half. Obtain a pair from `ScopedContext::make_channel(n)` (bounded, capacity `n`) or `ScopedContext::make_local_channel()` (unbounded, `D` need not be `Send`).

```rust
impl Promise {
    pub fn create_with_receiver<D: Send + 'static>(
        ctx: &mut ScopedContext,
        rx: WakeReceiver<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32>;
}
```

Internally:

- The runtime polls `rx.try_recv()` from the JS thread.
- When a value `data: D` is received, `convert(ctx, data)` is called exactly once.
- The returned `JSResult` controls whether the Promise is fulfilled or rejected.
- If the channel is disconnected before a value is received, the Promise is rejected with an internal error.

Example: use a manually created channel when you need extra control over the sending side:

```rust
let (tx, rx) = ctx.make_channel(1);

let promise = Promise::create_with_receiver(ctx, rx, |cx, res: Result<Vec<u8>, String>| {
    match res {
        Ok(bytes) => {
            let obj = JSFileData::new_instance(cx, bytes)?;
            Ok(JSValue::from(obj))
        }
        Err(e) => {
            cx.throw_internal_error(&e);
            Err(-1)
        }
    }
})?;

std::thread::spawn(move || {
    let result = do_blocking_io();
    let _ = tx.send(result);
});

// JS: const data = await host.readFileWithReceiver(...);
```

Use `create_with_receiver` when:

- You already have a `WakeReceiver<D>` created elsewhere (for example, shared between multiple producers).
- You need to plug a pre-existing channel into the Promise machinery without allocating a new one.

### `Promise::create_with_channel`

Create a Promise that is completed by sending a value through a channel from any thread or async runtime. This is a convenience wrapper around `create_with_receiver` that allocates a fresh `sync_channel` internally and returns the sending side:

```rust
impl Promise {
    pub fn create_with_channel<D: Send + 'static>(
        ctx: &mut ScopedContext,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<(Self, WakeSender<D>), i32>;
}
```

This helper composes three concerns:

- Use `WakeSender<D>` to receive a host-level result `D` from any thread or executor.
- From the JS runtime thread, convert `D` into a `JSValue` using `convert`.
- Resolve or reject the Promise inside the normal Promise job queue.

Semantics:

- On the JS side, you just see a normal `Promise`.
- On the host side:
  - You get `(promise, tx)` where `tx` is a `WakeSender<D>` (see channel types note above).
  - You run your async work anywhere (thread, tokio, async-std).
  - When done, you call `tx.send(data)` with `data: D`.
  - At some later tick, the job will:
    - Call `convert(ctx, data)`; on success the Promise is fulfilled with the returned `JSValue`.
    - If `convert` returns `Err(-1)` with an exception set on `ctx`, the Promise is rejected.

Example: bridge a background thread computing a `LlamaModel` using a channel and convert thread results into Promise fulfillment or rejection:

```rust
let (promise, tx) =
    Promise::create_with_channel(ctx, |cx, model: Result<LlamaModel, String>| -> JSResult {
        let model = model.map_err(|e| {
            cx.throw_internal_error(&e);
            -1
        })?;
        let obj = JSLlamaModel::new_instance(cx, model)?;
        Ok(JSValue::from(obj))
    })?;

let _ = std::thread::spawn(move || {
    let model_params = LlamaModel::default_params();
    let result =
        LlamaModel::load_from_file(&CString::new(file_name).unwrap(), model_params);
    let _ = tx.send(result);
});

// JS: const model = await host.loadModel(...);
```

Example: integrate `tokio::fs::read`:

```rust
let (promise, tx) = Promise::create_with_channel(ctx, |cx, res: std::io::Result<Vec<u8>>| {
    match res {
        Ok(bytes) => {
            let obj = JSFileData::new_instance(cx, bytes)?;
            Ok(JSValue::from(obj))
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

Use `create_with_channel` when:

- Your async work is naturally expressed as a Rust `Future` or blocking code on some runtime.
- You want a typed channel from host world (`D`) into JS world, and you need to convert `D` into `JSValue` on the JS runtime thread.
- You do not already have a `WakeReceiver<D>` and are happy for the helper to create the channel for you. If you already own a `WakeReceiver<D>`, prefer `create_with_receiver`.

### `Promise::create_with_result_slot`

Create a Promise driven by a [`PromiseResultSlot<D>`]. This is an alternative to the channel-based helpers that uses a shared slot instead of a channel:

```rust
impl Promise {
    pub fn create_with_result_slot<D: Send + 'static>(
        ctx: &mut ScopedContext,
        data_slot: PromiseResultSlot<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32>;
}
```

#### `PromiseResultSlot<D>`

A thread-safe, single-shot data container used to submit a result from another thread into the Promise system:

```rust
pub struct PromiseResultSlot<D: Send> { /* ... */ }

impl<D: Send + 'static> PromiseResultSlot<D> {
    pub fn new() -> Self;
    pub fn is_empty(&self) -> bool;
    pub fn commit(&self, data: D) -> Result<(), D>;
}
```

Key semantics:

- `new()` – creates an empty slot.
- `is_empty()` – returns `true` if the slot contains no data.
- `commit(data)` – submits a value into the slot. Returns `Err(data)` if the slot already contains a value (i.e. it is single-shot). This is thread-safe and can be called from any thread.

#### How `create_with_result_slot` works

Internally:

- When a value `D` is taken, `convert(ctx, data)` is called exactly once.
- The returned `JSResult` controls whether the Promise is fulfilled or rejected.
- If the slot is empty, the job is rescheduled on the next tick.
- The slot **must** be empty when `create_with_result_slot` is called; otherwise an internal error is thrown.

Example:

```rust
let slot = PromiseResultSlot::<i32>::new();
let promise = Promise::create_with_result_slot(ctx, slot.clone(), |_, n: i32| {
    Ok(JSValue::from(n))
})?;

// From another thread:
slot.commit(42).unwrap(); // Promise will be fulfilled with 42

// JS: const value = await host.compute(...);
```

Example with a result type:

```rust
let slot = PromiseResultSlot::new();
let promise = Promise::create_with_result_slot(ctx, slot.clone(), |cx, res: Result<Vec<u8>, String>| {
    match res {
        Ok(bytes) => {
            let obj = JSFileData::new_instance(cx, bytes)?;
            Ok(JSValue::from(obj))
        }
        Err(e) => {
            cx.throw_internal_error(&e);
            Err(-1)
        }
    }
})?;

std::thread::spawn(move || {
    let result = do_blocking_io();
    let _ = slot.commit(result);
});

// JS: const data = await host.readFileWithSlot(...);
```

Use `create_with_result_slot` when:

- You prefer a slot-based pattern over a channel-based pattern.
- You want a thread-safe, single-shot container that is simpler than a full channel.
- You need to share the slot between multiple producers (only the first `commit` succeeds).

## Promise ↔ Future bridge

The async support module (`async_support.rs`, enabled with the `async_support` Cargo feature) provides helpers to translate between Rust `Future` and JS `Promise`.

### Promise → Future: `Promise::into_future`

Convert a JS Promise into a Rust `Future` that can be `await`ed in host code. You typically obtain the `Promise` from a `JSValue` using `Promise::try_from`:

```rust
impl TryFrom<JSValue> for Promise {
    type Error = i32;
    // succeeds when the value is a Promise object
}
```

Usage pattern:

```rust
// Evaluate JS code that returns a Promise (non-blocking):
let val = scope.eval(code, &EvalName::Name("<test>".into()), eval_type, flags)?;

// Convert JSValue to Promise:
if let Ok(promise) = Promise::try_from(val) {
    let fut = promise.into_future(scope)?;
    let result = fut.await?; // Result<JSValue, JSValue>
}
```

```rust
impl Promise {
    pub fn into_future(
        mut self,
        ctx: &mut ScopedContext,
    ) -> Result<PromiseFuture, i32>;
}

pub struct PromiseFuture { /* ... */ }

impl Future for PromiseFuture {
    type Output = Result<JSValue, JSValue>;
    // poll implementation registers waker and waits for Promise settlement
}
```

Usage example:

```rust
async fn call_js_compute(rt: &mut Runtime) -> JSResult {
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            Ok(())
        })
        .unwrap();

    // Non-blocking eval: a script that returns a JS Promise comes back
    // immediately as that Promise value.
    let promise_value = ctx.eval(
        b"Promise.resolve(42)",
        &EvalName::Name("<test>".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )?;

    let promise = Promise::try_from(promise_value).map_err(|_| -1)?;
    let js_future = promise.into_future(&mut ctx.root_scope())?;

    // Drive the runtime's tick loop and await the JS Promise concurrently.
    let (_rt_res, promise_res) = tokio::join!(rt.run_async(), js_future);
    let result = promise_res.map_err(|_| -1)?;
    Ok(result)
}
```

This lets you treat JS async functions as plain Rust `Future`s:

- JS side: `async function compute(x) { return x * 2; }`
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
fn js_fetch(ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    let promise = Promise::create_from_future(ctx, |notify| {
        // Assume we're using tokio
        tokio::spawn(async move {
            let res: Result<JSValue, JSValue> = an_async_func().await;
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
  - Otherwise, reschedules the job to be polled again in a later tick.

This design keeps the JS Promise semantics inside Zuqe's job + tick system, while allowing the host to decide how to schedule and execute the actual Rust async work.

## JS-level APIs

On the JS side, the runtime exposes a standard `Promise` constructor and methods, implemented over the internal `Promise` type:

- `new Promise(executor)` – creates a Promise and calls the executor with `resolve` and `reject` functions.
- `Promise.resolve(value)` / `Promise.reject(value)` – create an already-settled Promise.
- `Promise.all`, `Promise.allSettled`, `Promise.any`, `Promise.race`, `Promise.try` – combinator methods implemented via microtask jobs.
- `Promise.withResolvers()` – returns an object `{ promise, resolve, reject }`.
- `promise.then(onFulfilled, onRejected)` – attaches reaction handlers.
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
    - Using `Promise::create_with_channel` when the work runs in a separate runtime or thread and results are sent through a channel.
    - Using `Promise::create_with_result_slot` when you prefer a slot-based, single-shot pattern to submit results from another thread.
    - Using `Promise::create_from_future` when you prefer a callback-based integration with existing `Future`-returning APIs.

In all cases, Promise settlement (state transitions and reaction execution) remains inside Zuqe's job + tick microtask engine, preserving ECMAScript semantics while giving the host full control over how and where asynchronous work is actually performed.

## `async_support` feature and runtime modes

Promise–Future integration and async helpers are guarded by the Cargo feature `async_support`.

- When `async_support` is **enabled** (the default), Zuqe exposes:
  - `Runtime::run_async() -> RunFuture<'_>` to drive the JS runtime as a `Future`.
  - `Promise::into_future` and `Promise::create_from_future` to bridge JS Promise and Rust async/await.
- When `async_support` is **disabled**, you can still run JS code and Promises, but the host drives execution explicitly. The synchronous `eval_wait` (on `ScopedContext`/`Context`) blocks, polling the tick loop until all pending Promise jobs and microtasks are complete.

### Example: synchronous host without `async_support`

When the feature is disabled, the `examples/zjs` main entry point uses a synchronous flow built on `eval_wait`, which returns an `ExitStatus`:

```rust
#[cfg(not(feature = "async_support"))]
fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    // The entry filename must be a canonical `file://` URL so that relative
    // dynamic `import()` and `import.meta.url` resolve correctly.
    let entry_name =
        zuqe::path_to_url(&args.filename).expect("entry path must convert to a file:// URL");

    let exit_code = ctx.with_root_scope(|mut scope| {
        match scope.eval_wait(
            script.as_bytes(),
            &EvalName::BaseUrl(entry_name),
            if is_module { EvalType::Module } else { EvalType::Global },
            EvalFlags::empty(),
        ) {
            ExitStatus::Value(_) => ExitCode::SUCCESS,
            ExitStatus::Interrupted(code) => code,
            ExitStatus::Exception(Exception::Error(err)) => {
                let msg = err
                    .to_string(&mut scope)
                    .unwrap()
                    .content()
                    .encode_utf8(false);
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
            ExitStatus::Exception(Exception::OutOfMemory) => {
                eprintln!("out of memory");
                ExitCode::FAILURE
            }
            _ => ExitCode::FAILURE,
        }
    });

    exit_code
}
```

Here `eval_wait`:

- Evaluates the script.
- Drives the job + tick engine until all pending Promise jobs and microtasks are complete.
- Returns the final outcome as an `ExitStatus`: `Value(jsvalue)` for a settled script value, `Exception(...)` for an uncaught error, or `Interrupted(code)` if `std.exit()` / the host interrupt fired.

The host does not use Rust async/await; it simply blocks until the JS program (including all Promises) has finished.

### Example: async host with `async_support` enabled

With `async_support` enabled, `examples/zjs` switches to an async main that uses `Context::eval_wait` as an async function. The async `eval_wait` drives the loop to completion and observes the top-level module Promise, surfacing a rejection as `ExitStatus::Exception`:

```rust
#[cfg(feature = "async_support")]
#[tokio::main]
async fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    // The entry filename must be a canonical `file://` URL so that relative
    // dynamic `import()` and `import.meta.url` resolve correctly.
    let entry_name =
        zuqe::path_to_url(&args.filename).expect("entry path must convert to a file:// URL");

    // Eval the entry and drive the event loop to completion. `eval_wait`
    // handles top-level module Promise observation internally, so a rejected
    // top-level promise is surfaced as `ExitStatus::Exception` and never
    // silently dropped. A synchronous (non-Promise) result is returned as
    // `ExitStatus::Value`.
    let run_result = ctx
        .eval_wait(
            &mut rt,
            script.as_bytes(),
            &EvalName::BaseUrl(entry_name),
            if is_module { EvalType::Module } else { EvalType::Global },
            EvalFlags::empty(),
        )
        .await;

    // Map the runtime exit status to a process exit code.
    match run_result {
        ExitStatus::Value(_) => ExitCode::SUCCESS,
        ExitStatus::Interrupted(code) => code,
        ExitStatus::Exception(Exception::OutOfMemory) => {
            eprintln!("out of memory");
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                eprintln!(
                    "{}",
                    err.to_string(&mut cx).unwrap().content().encode_utf8(false)
                );
            });
            ExitCode::FAILURE
        }
        _ => ExitCode::FAILURE,
    }
}
```

Key points:

- `#[tokio::main]` creates a Tokio runtime; `main` becomes an async function.
- `ctx.eval_wait(&mut rt, ...).await` evaluates the script once and drives the JS runtime's job + tick engine until the loop stops and the top-level Promise has settled.
- A rejected top-level module promise is never silently dropped — it becomes `ExitStatus::Exception(Exception::Error(reason))`.
- If you need finer control, you can still evaluate with the non-blocking `eval()`, convert the returned JS `Promise` with `Promise::into_future()`, and run `rt.run_async()` concurrently via `tokio::join!` — see [Basic Usage](basic.md).

This pattern shows how `async_support` lets you:

- Treat the entire JS runtime as a `Future` that can be awaited (`rt.run_async()`).
- Treat individual JS Promises as Rust Futures (`Promise::into_future`).
- Integrate both into a standard async Rust application (here driven by Tokio), keeping JS semantics intact while leveraging Rust’s async ecosystem.
