# Basic Usage

This tutorial introduces the foundational concepts of integrating the `zuqe` crate into a Rust project and **running JavaScript in async mode**. It covers adding the dependency, creating a runtime and context, and executing JavaScript as a Rust `Future` inside an async runtime such as Tokio.

## 1. Setup Cargo.toml

Add `zuqe` to your dependencies and enable the `async_support` feature so the engine can be driven as a `Future`. You'll also need an async runtime (Tokio here) on the host side.

```toml
[dependencies]
zuqe = { version = "0.8.0" } # replace with the latest version
tokio = { version = "1", features = ["full"] }
```

## 2. Initialize Runtime and Context

The core of any `zuqe` application is the `Runtime` and `Context`. The `Runtime` owns the engine, while the `Context` provides an isolated environment for script execution.

```rust
use zuqe::{Context, Runtime, ScopedContext};

fn build() -> (Runtime, Context) {
    let mut rt = Runtime::create().unwrap();

    let ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)
        })
        .unwrap();

    (rt, ctx)
}
```

`ScopedContext::ECMA_CONTEXT_SETUP` is the context setup handler that initializes a context to meet the ECMA standard. You may also provide your own context setup callback.

## 3. Run JavaScript (async mode)

In async mode you drive the runtime with `Runtime::run_async()` and evaluate scripts with the non-blocking `eval()` (not the blocking `eval_wait()`). When a script returns a JS `Promise`, convert it to a Rust `Future` via `Promise::into_future()` and `await` it alongside `rt.run_async()`.

```rust
use zuqe::{Context, EvalFlags, EvalType, JSValue, Promise, Runtime, ScopedContext};

#[tokio::main]
async fn main() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(|cx| ScopedContext::ECMA_CONTEXT_SETUP(cx))
        .unwrap();

    let script = r#"
        let greet = (name) => `Hello, ${name}!`;
        greet("Zuqe");
    "#;

    // Evaluate inside a scope. `eval()` is non-blocking: a plain value is
    // returned immediately, while a Promise gives us a Rust Future to await.
    let mut promise_future = None;
    let mut direct_value = None;

    ctx.with_new_scope(|mut scope| {
        let val = scope.eval(
            script.as_bytes(),
            "example.js",
            EvalType::Global,
            EvalFlags::empty(),
        )?;

        if let Ok(promise) = Promise::try_from(val) {
            promise_future = Some(promise.into_future(&mut scope)?);
        } else {
            direct_value = Some(val);
        }
        Ok(())
    });

    // Await the JS Promise (if any) together with the runtime loop.
    let value = if let Some(js_fut) = promise_future {
        let (_rt_res, promise_res) = tokio::join!(rt.run_async(), js_fut);
        match promise_res {
            Ok(v) => v,
            Err(e) => e, // the Promise rejected; `e` is the rejection reason
        }
    } else {
        direct_value.unwrap_or(JSValue::UNDEFINED)
    };

    ctx.with_new_scope(|mut scope| {
        println!("result: {}", value.inspect(&mut scope).unwrap());
        Ok(())
    });
}
```

Key points of the async workflow:

- Use **`scope.eval()`** (non-blocking) instead of the blocking `scope.eval_wait()`.
- Convert a returned JS `Promise` into a Rust `Future` via `Promise::into_future()`.
- Drive the runtime with **`rt.run_async().await`** — usually via `tokio::join!` so the runtime loop and the Promise future progress concurrently.
- A plain (non-Promise) value from `eval()` is available immediately after the scope, with no need to await the runtime.

This tutorial provides a starting point for using the `zuqe` crate in async mode. The following tutorials cover modules, native functions, classes, and more advanced usage.

See the [Promise documentation](promise.md) for a complete reference on Promise–Future integration, and the [Running Loop](loop.md) document for the lower-level tick mechanism (including the synchronous mode as a reference).
