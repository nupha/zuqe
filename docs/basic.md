# Basic Usage

This tutorial introduces the foundational concepts of integrating the `zuqe` crate into a Rust project. It covers adding the dependency, creating a runtime and context, and executing JavaScript code.

## 1. Setup Cargo.toml

Add `zuqe` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
zuqe = "0.4.0" # replace with the latest version
```

## 2. Initialize Runtime and Context

The core of any `zuqe` application is the `Runtime` and `Context`. The `Runtime` owns the engine, while the `Context` provides an isolated environment for script execution.

```rust
use zuqe::{Context, Runtime};

let mut rt = Runtime::create().unwrap();

let mut ctx = rt
    .create_context(|cx| {
        Context::ECMA_CONTEXT_SETUP(cx)
    })
    .unwrap();
```

Where `Context::ECMA_CONTEXT_SETUP` is the context setup handler that initialize a context to meet ECMA standard. You may also provide your own context setup callback.

## 3. Run JavaScript

Use `with_new_scope` and `eval_wait` to execute JavaScript inside a new scope.

```rust
use zuqe::{EvalFlags, EvalType};

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
```

This tutorial provides a starting point for using the `zuqe` crate. The following tutorials cover modules, native functions, classes, and more advanced usage.
