# Running Loop (internals)

The running loop drives pending microtasks, Promise jobs, timers, and custom
tick handlers. Hosts do **not** call `tick()` directly; they drive it through:

- `Context::eval_wait(...).await` (default `async_support`) — executes an entry
  and runs the loop to idle while observing the top-level Promise.
- `Runtime::run_async()` — a `RunFuture` (`Output = ExitStatus`) that repeatedly
  ticks until no work remains.
- synchronous `eval_wait` when `async_support` is disabled.

Host-facing setup and signatures are in
**[Basics: Runtime, Context, and Execution](../skills/basics.md)**;
Promise-specific bridging is in **[Promise and Rust Future Bridge](../skills/promise.md)**.

## One tick

`RuntimeImpl::tick_self` / `tick_tree` iterate contexts and run, in order:

1. **One-shot jobs.** Promise reactions and timer callbacks. They always
   complete and are never retried; newly queued one-shot jobs are drained in the
   same round. Each job runs in its own GC scope.
2. **Polling jobs, one round.** Jobs may return `JobState::Done`, `Block`, or
   `Continue`:
   - `Done` — removed;
   - `Block` — re-queued and counts as parked; the loop must not exit while it
     is pending;
   - `Continue` — re-queued but does **not** count as parked (fire-and-forget /
     unobserved promise).
3. **Custom tick handlers** registered with `ScopedContext::add_tick_handler`.
   A handler returns `true` if it produced progress.

The tick also collects the earliest deadline reported by polling jobs
(`poll_hint`) so the driver can park until that deadline or an external wake.

## Parking

When a polling job is blocked (I/O, timer, channel), the driver parks the
thread. Wakes come from:

- `WakeSender`/`WakeReceiver` and `PromiseResultSlot` commits;
- timers;
- custom host wake handles (`ctx.wake_handle()` / runtime park state).

With `quantum_poll` enabled, every park is capped by a quantum; by default the
wait is purely event-driven (self-pipe + deadline).

## Custom tick handler

```rust
fn register_ticks(ctx: &mut ScopedContext) {
    ctx.add_tick_handler(|_cx| -> bool {
        // return true if this handler produced work
        false
    });
}
```

The handler is a plain `fn(&mut ScopedContext) -> bool` (no captured host
state). It runs once per tick; use it for periodic polling or native event
draining. For Promise-driven work prefer the job APIs.

`zjs` registers its OS event poller this way:

```rust
pub fn init_mod_os(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("os", MOD_OS_ENTRIES)?;
    ctx.add_tick_handler(|cx| -> bool { js_os_poll(cx) });
    Ok(())
}
```

## Synchronous mode

When `async_support` is disabled, `Context::eval_wait` and
`ScopedContext::eval_wait` block, polling the tick loop until all microtasks and
Promise jobs finish or the host interrupts:

```rust
let status = ctx.with_root_scope(|mut scope| {
    scope.eval_wait(
        source.as_bytes(),
        &EvalName::Name("<script>".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
});
```

`ExitStatus` carries `Value(jsvalue)`, `Interrupted(code)`, or
`Exception(Exception)`. A completion-value Promise is awaited unless
`EvalFlags::NO_AWAIT_RESULT` is set (module entries always observe their
top-level promise).

## Async mode

With `async_support` (default), `Context::eval_wait` is an async function and
takes **no** `&mut Runtime`:

```rust
let status = ctx
    .eval_wait(
        source.as_bytes(),
        &EvalName::BaseUrl(entry_url),
        EvalType::Module,
        EvalFlags::empty(),
    )
    .await;
```

Internally it combines `eval` with `Promise::into_future` and
`futures_util::future::select`, so a rejected top-level module promise is
reported as `ExitStatus::Exception` instead of being dropped.
