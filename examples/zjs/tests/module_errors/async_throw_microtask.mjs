// D2: rejection that settles on a microtask. The old `tokio::join!` driver would
// also drop this, but it is the baseline microtask case.
await Promise.resolve();
throw new Error("ASYNC_THROW_MICROTASK_MARKER");
