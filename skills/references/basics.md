# Basics: Runtime, Context, and Execution

> For Rust host code that depends on `zuqe`. This page covers the public API.
> Tick, Promise job, and GC internals are in `docs/loop.md`, `docs/promise.md`,
> and `docs/gc.md`.

## 1. Cargo dependency and features

```toml
[dependencies]
zuqe = "0.12"          # use the version in Cargo.toml
tokio = { version = "1", features = ["full"] } # async hosts only
```

The feature names below match `Cargo.toml`. `default` is a feature group, not a
single switch. Every feature is listed, including the default ones.

| Feature | Default | Purpose |
|---|---|---|
| `default` | - | Group: `inline_cache`, `big_int`, `all_unicode`, `async_support`, and the default `builtin_*` set. |
| `inline_cache` | on | Per-opcode inline caches for property access. |
| `big_int` | on | `BigInt` support, including `BigInt` literals and operators. |
| `all_unicode` | on | Full Unicode identifier rules and normalization (`unicode-id-start`, `unicode-normalization`). |
| `async_support` | on | Async `eval_wait`, `Runtime::run_async()`, and the Promise/Future bridge. Adds `futures-util`. |
| `crossbeam_channel` | off | Use `crossbeam::channel` instead of `std::sync::mpsc` for host channels. |
| `serde_json_support` | off | Convert between `JSValue` and `serde_json::Value`. |
| `zapi` | off | Load zapi cdylib extensions. Adds `zuqe_zapi` and `libloading`. See [zapi-extension.md](zapi-extension.md). |
| `quantum_poll` | off | Allow a fallback periodic poll cap on event-loop park (`use_quantum_poll`). Normally not needed. |
| `persistent` | off | Serialize and deserialize bytecode and JS values (`ZQBC` format). Used by `zjs` `dumpValue`/`restoreValue`. |
| `child_runtime` | off | Create child runtimes driven by a parent runtime event loop. |
| `builtin_proxy` | on | `Proxy` support. |
| `builtin_atomic` | on | `Atomics` support. |
| `builtin_mapset` | on | `Map`, `Set`, `WeakMap`, and `WeakSet`. |
| `builtin_regexp` | on | `RegExp` and regexp string methods through the `lre`/QuickJS regexp library. |
| `builtin_reflect` | on | `Reflect` support. |
| `builtin_json` | on | `JSON` support. |
| `builtin_date` | on | `Date` support. |
| `builtin_math` | on | `Math` support. |
| `builtin_weakref` | on | `WeakRef` and `FinalizationRegistry`. |
| `intl402` | off | `Intl` support. Uses ICU and `fixed_decimal`. |
| `gc_generations` | off | Generational GC: young/old generations, minor collections, and write barriers. |
| `gc_protect_transient` | off | Protect temporary GC objects when `gc_alloc` can trigger a collection. Enable only when the allocator can collect. |
| `dump_bytecode` | off | Print bytecode disassembly during compilation. Development tool. |
| `debug_gc` | off | In debug builds, run a GC after every opcode. Very slow; debug only. |
| `jit` | off | Tiered bytecode JIT with Cranelift. Adds the Cranelift crates. See [Tiered JIT Usage](jit.md). |
| `jit_dump` | off | Dump Cranelift IR and machine code. Implies `jit`. Debug tool. |

For a smaller build, use `--no-default-features --features "..."` and list only
the features you need. For example:

```bash
cargo build --no-default-features --features "async_support,big_int,builtin_json,builtin_mapset,jit"
```

## 2. Create a Runtime and Context

`Runtime` owns the GC heap and the event loop. `Context` is an isolated global
environment. A Context belongs to exactly one Runtime.

```rust
use zuqe::{Context, Runtime, ScopedContext};

fn build() -> Result<(Runtime, Context<'static>), i32> {
    let mut rt = Runtime::create()?;
    let ctx = rt.create_context(|cx| ScopedContext::ECMA_CONTEXT_SETUP(cx))?;
    Ok((rt, ctx))
}
```

`ScopedContext::ECMA_CONTEXT_SETUP` installs the standard ECMAScript globals.
You can also pass your own setup closure and expose only a small host API.

## 3. Run a script: `eval_wait` and `eval`

### 3.1 Recommended: `eval_wait`

`Context::eval_wait` runs an entry script or module. It drives the tick loop
until no work remains. With default `async_support`, it is **async** and does
**not** take `&mut Runtime`:

```rust
use zuqe::{EvalFlags, EvalName, EvalType, Exception, ExitStatus};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut rt = zuqe::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(|cx| zuqe::ScopedContext::ECMA_CONTEXT_SETUP(cx))
        .unwrap();

    let status = ctx
        .eval_wait(
            b"Promise.resolve(42)",
            &EvalName::Name("<host>".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await;

    match status {
        ExitStatus::Value(v) => {
            let s = ctx.with_root_scope(|mut cx| {
                v.to_string(&mut cx).unwrap().content().encode_utf8(false)
            });
            println!("{s}");
            std::process::ExitCode::SUCCESS
        }
        ExitStatus::Interrupted(code) => code,
        ExitStatus::Exception(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                eprintln!("{}", err.to_string(&mut cx).unwrap().content().encode_utf8(false));
            });
            std::process::ExitCode::FAILURE
        }
        ExitStatus::Exception(_) => std::process::ExitCode::FAILURE,
    }
}
```

`eval_wait` watches the entry top-level Promise. A module rejection becomes
`ExitStatus::Exception(Exception::Error(reason))`. It is not dropped.

When `async_support` is off, `Context::eval_wait` and `ScopedContext::eval_wait`
are synchronous. They also take no Runtime:

```rust
let status = ctx.with_root_scope(|mut scope| {
    scope.eval_wait(
        b"1 + 1",
        &zuqe::EvalName::Name("<host>".into()),
        zuqe::EvalType::Global,
        zuqe::EvalFlags::empty(),
    )
});
```

### 3.2 Low level: `eval` plus `run_async`

`eval` does not block. A plain value returns at once. If the script returns a
Promise, convert it to a Rust Future and drive it with `Runtime::run_async()`.

```rust
use zuqe::builtins::promise::Promise;
use zuqe::{EvalFlags, EvalName, EvalType};

let mut promise_future = None;
let mut direct = None;

ctx.with_root_scope(|mut scope| {
    let val = scope.eval(
        b"Promise.resolve(7)",
        &EvalName::Name("<host>".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )?;
    if let Ok(p) = Promise::try_from(val) {
        promise_future = Some(p.into_future(&mut scope)?);
    } else {
        direct = Some(val);
    }
    Ok::<_, i32>(())
})?;

let value = if let Some(fut) = promise_future {
    let (_run, result) = tokio::join!(rt.run_async(), fut);
    result.unwrap_or_else(|reason| reason)
} else {
    direct.unwrap_or(zuqe::JSValue::UNDEFINED)
};
```

`Runtime::run_async()` returns `RunFuture`. Its `Output` is `ExitStatus`. More
Promise details are in [promise.md](promise.md).

## 4. `EvalName`, `EvalType`, and `EvalFlags`

- `EvalName::Name(s)` is only a debug filename. It does not bind a URL base.
  `Name("")` means no name.
- `EvalName::BaseUrl(url)` is both the name and the URL base. **Module execution
  must use it**, so `import.meta.url` and relative `import()` resolve.
- `EvalType::Global` runs a script. `EvalType::Module` runs an ES module and
  returns a Promise. `EvalType::Indirect` uses indirect eval semantics.
  `Direct` is for internal eval only.
- `EvalFlags` has `STRICT`, `STRIP`, `BACKTRACE_BARRIER`, `COMPILE_TO_ASYNC`,
  and `NO_AWAIT_RESULT`. Host entries usually use `EvalFlags::empty()`.

Use `zuqe::path_to_url` to convert an entry filename to a URL.

## 5. Public GC scope entry points

- `Context::with_root_scope(|scope| ...)` runs in the root scope. Values in the
  root scope live until the Context is dropped. Call
  `scope.remove_local(node)` when a value is no longer needed.
- `ScopedContext::with_new_scope(|scope| ...)` creates a child scope. The
  closure result is promoted to the parent scope. Use it for temporary work.
- `Context::eval` and `Context::eval_wait` already create a child scope. You do
  not need to wrap them.

`Context::with_new_scope` is crate-internal. Hosts should write
`ctx.with_root_scope(|scope| scope.with_new_scope(...))`.

## 6. Resource limits and host setup

```rust
let mut rt = zuqe::Runtime::create()?;
rt.install_panic_handler();                 // try to free GC on panic in release

rt.gc_set_memory_limit(64 << 20);           // GC heap limit in bytes
rt.set_vm_stack_arena(256 << 10).map_err(|_| -1)?; // VM frame bump arena
rt.set_vm_stack_limit(512 << 10);           // total VM stack: arena + heap spill
rt.set_thread_stack_limit(512 << 10);       // C stack recursion limit; 0 = auto
```

- `set_vm_stack_arena` returns `Result<(), ()>`. It returns `Err(())` if the
  allocation fails.
- `set_vm_stack_limit` should be `>=` the arena size. A smaller limit only
  wastes arena space.
- `set_thread_stack_limit(0)` uses auto mode. It derives the limit from the
  real thread stack bottom and keeps `THREAD_STACK_LIMIT_MARGIN` for the throw
  path.
- Auto mode needs platform support for the stack bottom.
  `Runtime::CAN_AUTO_THREAD_STACK_LIMIT` tells you at compile time.

## 7. JIT

JIT setup, attachment, tiers, thresholds, metrics, and compile timing are in
[Tiered JIT Usage](jit.md).

## 8. Module loading hooks

Module sources are host policy:

```rust
rt.set_module_normalizer(my_normalizer); // optional; a default normalizer exists
rt.set_module_loader(my_loader);         // without it, every import throws
```

Both callbacks are plain `fn` pointers. They carry no host state. Inject state
with `Runtime::create_with(state)` and read it back with
`ctx.rt().user_state::<T>()`. See
[Modules: ESM Loading and Native Modules](native-module.md) for signatures,
import attributes, `import.meta`, and zapi extension loading.

## 9. Quick checklist

- [ ] Runtime and Context are created in the right order. `Context` is not sent
      across threads or runtimes.
- [ ] With default `async_support`, use `Context::eval_wait(...).await`. Do not
      pass `&mut Runtime`.
- [ ] Module entries use `EvalName::BaseUrl(path_to_url(...)?)`.
- [ ] Promise results use `Promise::try_from` and `into_future`, driven with
      `run_async`.
- [ ] Temporary values use `with_new_scope`. Remove root-scope values with
      `remove_local` when done.
- [ ] Resource limits are set as needed. `set_vm_stack_arena` errors are handled.
