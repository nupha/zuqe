# Promise and Rust Future Bridge

> For Rust host code. The engine keeps JS Promise behavior (microtasks,
> reaction order, error handling). This page shows how the host creates,
> settles, and waits for Promises.
> Promise job and tick internals are in `docs/promise.md` and `docs/loop.md`.

```rust
use zuqe::builtins::promise::Promise;
```

## 1. Type and state

```rust
pub enum PromiseState { Pending, Fulfilled, Rejected }

impl Promise {
    pub fn state(&self) -> PromiseState;
    pub fn result(&self) -> JSValue;
}
```

`state()` and `result()` are for host tests and diagnostics. They are not
exposed to JS.

## 2. Create and settle from Rust

```rust
impl Promise {
    /// Return a pending Promise and the [resolve, reject] JS functions.
    pub fn create(ctx: &mut ScopedContext) -> Result<(Promise, [JSValue; 2]), i32>;

    pub fn resolve(&mut self, ctx: &mut ScopedContext, value: JSValue);
    pub fn reject(&mut self, ctx: &mut ScopedContext, value: JSValue);
    pub fn then(&mut self, ctx: &mut ScopedContext, success: JSValue, fail: JSValue)
        -> Result<Object, i32>;
}
```

```rust
let (mut promise, resolving) = Promise::create(ctx)?;
let _ = resolving; // call them later, or settle directly:
promise.resolve(ctx, JSValue::from(42));
```

`resolve` and `reject` trigger registered reactions. `then` follows
`SpeciesConstructor` and returns a new wrapper Promise.

## 3. Drive a Promise from host work

### 3.1 `create_with_job`: poll on the JS thread

```rust
impl Promise {
    pub fn create_with_job(
        ctx: &mut ScopedContext,
        job: impl Fn(&mut ScopedContext) -> Result<Option<JSValue>, i32> + 'static,
    ) -> Result<Promise, i32>;
}
```

The job runs in the JS thread microtask loop:

- `Ok(Some(v))`: resolve the Promise and finish the job.
- `Ok(None)`: keep waiting. The job stays registered. The loop parks if no
  external wake arrives.
- `Err(code)`: the job failed. The caller should already have set an exception
  on `ctx`. The Promise is rejected.

### 3.2 Channels: `create_with_channel` and `create_with_receiver`

```rust
impl Promise {
    pub fn create_with_channel<D: Send + 'static>(
        ctx: &mut ScopedContext,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<(Self, WakeSender<D>), i32>;

    pub fn create_with_receiver<D: Send + 'static>(
        ctx: &mut ScopedContext,
        rx: WakeReceiver<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32>;
}
```

- `WakeSender<D>` and `WakeReceiver<D>` are public newtypes. The raw
  `Sender`/`Receiver` aliases are not public API.
- `WakeSender` wakes the parked event loop on a successful `send` or `try_send`,
  and on drop.
- `convert` runs once on the JS thread. Its `JSResult` decides fulfill or
  reject.
- If the channel disconnects before a value arrives, the Promise is rejected
  with an internal error.
- Use `ctx.make_channel(n)` for a bounded channel. Use
  `ctx.make_local_channel()` for an unbounded channel; `D` does not need
  `Send`. If you already have a `WakeReceiver`, use `create_with_receiver`.

```rust
let (promise, tx) = Promise::create_with_channel(ctx, |cx, res: Result<Vec<u8>, String>| {
    match res {
        Ok(bytes) => Ok(JSValue::from(cx.create_base_object()?)),
        Err(e) => { cx.throw_internal_error(&e); Err(-1) }
    }
})?;

std::thread::spawn(move || {
    let _ = tx.send(do_blocking_io());
});
// JS: const data = await host.readFile(...);
```

### 3.3 `create_with_result_slot`: a single-write slot

```rust
pub struct PromiseResultSlot<D: Send> { /* ... */ }

impl<D: Send + 'static> PromiseResultSlot<D> {
    pub fn new() -> Self;
    pub fn is_empty(&self) -> bool;
    pub fn commit(&self, data: D) -> Result<(), D>;
}

impl Promise {
    pub fn create_with_result_slot<D: Send + 'static>(
        ctx: &mut ScopedContext,
        slot: PromiseResultSlot<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32>;
}
```

- `commit` is thread-safe. The slot accepts one value. A second commit returns
  `Err(data)`.
- The slot must be empty when passed to `create_with_result_slot`. Otherwise an
  internal error is thrown.
- If all committers drop without committing, debug builds panic at once.
  Release builds reject the Promise. It does not hang forever.

### 3.4 `hold_when_awaited` variants

`create_with_receiver_hold_when_awaited` and
`create_with_channel_hold_when_awaited` keep the event loop alive only while
the Promise is observed (`await` or `.then`). An unobserved fire-and-forget
Promise does not block process exit.

## 4. Promise and Future

### 4.1 Promise to Future: `into_future`

```rust
impl TryFrom<JSValue> for Promise { type Error = i32; }

impl Promise {
    pub fn into_future(self, ctx: &mut ScopedContext) -> Result<PromiseFuture, i32>;
}
```

```rust
let val = scope.eval(code, &EvalName::Name("<test>".into()), EvalType::Global, EvalFlags::empty())?;
if let Ok(p) = Promise::try_from(val) {
    let fut = p.into_future(scope)?;
    let result: Result<JSValue, JSValue> = fut.await?;
}
```

`PromiseFuture` implements `Future<Output = Result<JSValue, JSValue>>`. It
registers two reactions (fulfill and reject) on the Promise and wakes the
Waker when the Promise settles.

### 4.2 Future to Promise: `create_from_future`

```rust
impl Promise {
    pub fn create_from_future<F>(ctx: &mut ScopedContext, fut: F) -> Result<Self, i32>
    where
        F: FnOnce(Box<dyn FnOnce(Result<JSValue, JSValue>) + 'static>);
}
```

This API does not take a `Future`. It takes a registration closure. The closure
receives `notify`:

```rust
let promise = Promise::create_from_future(ctx, |notify| {
    // Call this on the JS thread, or from same-thread polling logic.
    // `notify` has no Send bound. Do not move it to another thread.
    notify(Ok(JSValue::from(42)));
})?;
```

`notify(Ok(v))` fulfills. `notify(Err(reason))` rejects. The result settles on a
later tick, still inside the Zuqe job and tick system. For cross-thread
results, use `create_with_channel` or `create_with_result_slot`, then build the
`JSValue` in `convert`.

## 5. Drive it with the event loop

- Default `async_support`: `ctx.eval_wait(...).await` handles the top-level
  Promise. At low level, use
  `tokio::join!(rt.run_async(), promise_future)`.
- Without `async_support`: `eval_wait` blocks until the loop is idle.
- `Runtime::run_async()` returns `RunFuture` with `Output = ExitStatus`. Each
  poll advances the tick loop until no work remains.
- Do not pass `&mut Runtime` to `eval_wait`. The Context belongs to one Runtime
  and gets the loop from `ctx.rt()`.

## 6. JS-side Promise

The standard `Promise` constructor and static methods are available:
`Promise.resolve`, `reject`, `all`, `allSettled`, `any`, `race`, `try`, and
`withResolvers`. Instance methods are `then`, `catch`, and `finally`. The host
usually only returns a Rust-created Promise. JS can `await` it as usual.

## 7. Choosing an API

| Use case | API |
|---|---|
| Poll host state on the JS thread | `create_with_job` |
| One result from a thread or async runtime | `create_with_channel` |
| You already have a `WakeReceiver` | `create_with_receiver` |
| Single-write slot with many producers | `create_with_result_slot` |
| Existing callback-style Future integration | `create_from_future` |
| Unobserved Promise must not keep the process alive | `*_hold_when_awaited` |

## 8. GC notes

Promises, resolving functions, and JS values captured by `convert` must stay
reachable:

- Use `#[derive(GcTrace)]` for `Promise`/`JSValue` in long-lived Rust structs.
- Keep values alive across JS calls per Rule 4 in
  [GC Safety Rules](gc-safety.md).
- When you write Promise or callback fields in a payload, use a guard slot and
  setter per Rule 5.
