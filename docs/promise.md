# Promise (internals)

This page describes how Zuqe implements Promises and bridges them to Rust
futures. The host-facing API — creating/settling Promises, channels, result
slots, `into_future`, `create_from_future` — is in
**[Promise and Rust Future Bridge](../skills/references/promise.md)**.

## Promise payload

`Promise` is a GC-node wrapper around `PromisePayload` (`src/builtins/promise/`):

- `state: PromiseState` — `Pending`, `Fulfilled`, `Rejected`.
- `result: WriteBarrier<JSValue>` — the fulfillment value or rejection reason.
- reaction lists — one per settlement kind, stored as guarded containers so an
  old Promise accepting a young reaction records the barrier.
- `is_handled` — a debug-only flag; it decides whether `settle` / `then` notify
  the host rejection tracker. Marked `#[deprecated]` in the source.

Settlement is spec-compliant: `settle` returns early if the Promise is no longer
`Pending` (second settle is a silent no-op, matching ECMA-262 resolving-function
semantics). On settlement:

1. Remove the Promise from `RuntimeImpl::gc_pending_promises`.
2. Store state/result through the write barrier (`set_result`).
3. Notify the host rejection tracker if rejected and unhandled.
4. Drain the reaction lists and enqueue one-shot jobs.

`Promise::resolve` / `Promise::reject` call `settle` directly; `Promise::then`
creates a wrapper Promise via `SpeciesConstructor` and registers reaction
handlers (`reaction`), while `perform_then` is the internal variant without the
`@@species` lookup.

## Jobs and the tick loop

Promise reactions are **one-shot jobs** (`OneshotJob`): they run once and always
complete. Host-created polling jobs (`PollingJob`) may return:

| `JobState` | Meaning |
|---|---|
| `Done` | completed, removed |
| `Block` | waiting for external work; re-queued and keeps the loop parked (process does not exit) |
| `Continue` | re-queued but does **not** keep the loop alive (fire-and-forget / unobserved promise) |

The tick (`src/runtime/tick.rs`) processes, in order:

1. all one-shot jobs, draining newly queued ones;
2. one round of polling jobs;
3. customized tick handlers (`ScopedContext::add_tick_handler`).

Each one-shot microtask runs in its **own GC scope**
(`cx.with_new_scope(|job_cx| job.run(job_cx))`). This matters for two reasons:

- temporaries allocated per `await` are released when that job ends;
- a single shared scope would pin every step's locals until the whole queue
  drained, and an `await` loop would grow without bound. (This scoping change
  is also the fix for the historical release fat-LTO miscompile described in
  [known-issue.md](known-issue.md).)

Job GC tracing is held in `RuntimeImpl::gc_job` while the job runs, so values
captured by the job stay reachable even though the job has been popped from the
queue.

## Host bridge internals

Every `create_with_*` helper is a polling job behind the scenes.
`create_with_job` creates a pending Promise plus resolve/reject functions, then
queues a polling job; the closure returns `Ok(Some(v))` (settle), `Ok(None)`
(`Block`), or `Err(code)` (reject with the current exception). The job's
resolving functions are traced via `JobGcTrace2`.

`create_with_receiver` and `create_with_channel` wrap `create_with_job` around a
`WakeReceiver<D>`; the runtime polls `try_recv()` on the JS thread and calls the
host `convert` once. `create_with_result_slot` uses a shared, single-shot
`PromiseResultSlot<D>` instead. The `*_hold_when_awaited` variants return
`Continue` instead of `Block` while the Promise has no reactions, so an
unobserved promise does not keep the process alive.

`Promise::into_future` registers a reaction (through the promise future
registry) that stores the settled `Result<JSValue, JSValue>` and wakes the Rust
`Waker`; `PromiseFuture::poll` returns `Pending` until the registry entry is
filled, then `Ready(result)`. The registry is traced during GC so a pending
future's values stay alive.

`Promise::create_from_future` creates a Promise plus an
`Rc<RefCell<Option<Result<JSValue, JSValue>>>>` result cell, then queues a
polling job that settles the Promise when the cell is filled. The host-supplied
closure receives a `notify` callback; calling it stores the result, and the next
job poll settles the Promise. `notify` is not `Send`, so cross-thread results
must use the channel/slot APIs instead.

## Runtime modes

- `async_support` enabled: `Runtime::run_async()` returns a `RunFuture` that
  ticks until idle; `Context::eval_wait(...).await` combines this with top-level
  Promise observation.
- `async_support` disabled: `eval_wait` blocks, polling the tick loop until all
  microtasks and Promise jobs complete or the host interrupts.

## Unhandled rejections

`RuntimeImpl` carries a `promise_rejection_tracker` callback (the runtime fields
are `#[deprecated]`, and there is no public setter). It is invoked when a
Promise is rejected without a handler and when a handler is later attached.

## Related

- Host API: [Promise and Rust Future Bridge](../skills/references/promise.md)
- Tick loop: [Running Loop](loop.md)
- GC roots and barriers: [Garbage Collection](gc.md)
