# Basic Usage

This tutorial introduces the foundational concepts of integrating the `zuqe` crate into a Rust project. It covers adding the dependency, creating a runtime and context, and executing JavaScript code.

## 1. Setup Cargo.toml

Add `zuqe` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
zuqe = "0.8.0" # replace with the latest version
```

## 2. Initialize Runtime and Context

The core of any `zuqe` application is the `Runtime` and `Context`. The `Runtime` owns the engine, while the `Context` provides an isolated environment for script execution.

```rust
use zuqe::{Context, Runtime, ScopedContext};

fn main() {
    let mut rt = Runtime::create().unwrap();

    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)
        })
        .unwrap();
}
```

Where `ScopedContext::ECMA_CONTEXT_SETUP` is the context setup handler that initializes a context to meet ECMA standard. You may also provide your own context setup callback.

## 3. Run JavaScript

Use `with_new_scope` and `eval_wait` to execute JavaScript inside a new scope.

```rust
use zuqe::{Context, EvalFlags, EvalType, Runtime, ScopedContext};

fn main() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)
        })
        .unwrap();

    let script = r#"
        let greet = (name) => `Hello, ${name}!`;
        greet("Zuqe");
    "#;

    ctx.with_new_scope(|mut scope| {
        let res = scope.eval_wait(
            script.as_bytes(),
            "example.js",
            EvalType::Global,
            EvalFlags::empty(),
        );

        match res {
            Ok(val) => {
                println!("eval result: {}", val.inspect(&mut scope).unwrap());
            }
            Err(e) => {
                let mut err_msg = e.message;
                if let Some(tb) = e.traceback {
                    err_msg = format!("{err_msg}\n{tb}");
                }
                println!("error: {err_msg}");
            }
        }
    });
}
```

This tutorial provides a starting point for using the `zuqe` crate. The following tutorials cover modules, native functions, classes, and more advanced usage.

## 4. Async execution with `async_support`

When the `async_support` Cargo feature is enabled, the runtime can
also be driven as a Rust `Future` inside a Tokio (or other async runtime) context.
This lets JS `Promise`s be `await`ed from Rust code:

```rust
use zuqe::{EvalFlags, EvalType, Promise};

async fn run_async_example(mut rt: Runtime, mut ctx: Context) {
    let mut promise_future = None;

    ctx.with_new_scope(|mut scope| {
        // Use eval() (non-blocking) instead of eval_wait() (blocking)
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
        // Drive the runtime and the Promise future concurrently
        let (_rt_res, promise_res) = tokio::join!(rt.run_async(), js_fut);
        // promise_res: Result<JSValue, JSValue>
    }
}
```

Key differences from the synchronous approach:

- Use **`scope.eval()`** instead of `scope.eval_wait()` to avoid blocking.
- Convert the returned JS `Promise` into a Rust `Future` via `Promise::into_future()`.
- Drive the runtime with **`rt.run_async().await`** instead of calling `eval_wait`.

When `async_support` is **disabled**, the runtime works purely synchronously as
shown in section 3. The host calls `eval_wait()` which blocks until all JS
Promises have settled.

See the [Promise documentation](promise.md) for a complete reference on
Promise–Future integration.
