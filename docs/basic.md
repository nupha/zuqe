# Basic Usage

This tutorial introduces the foundational concepts of integrating the `zhuque` crate into a Rust project. We'll cover setting up the `Cargo.toml`, initializing the runtime, and executing JavaScript code.

## 1. Setup Cargo.toml

Add `zuqe` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
zuqe = "0.3.0"
```

## 2. Initialize Runtime and Context

The core of any `zhuque` application is the `Runtime` and `Context`. The `Runtime` manages the engine, while the `Context` provides an isolated environment for script execution.

```rust
use zuqe::{Runtime, Context};

let mut rt = Runtime::create().unwrap();
let mut ctx = rt.create_context_ecma().unwrap();
```

## 3. Run JavaScript

Use `run_wait` to execute JavaScript within the context, wait till exit.

```rust
use zuqe::{EvalFlags, EvalType};

let script = r#"
    let greet = (name) => `Hello, ${name}!`;
    greet("Zuqe");
"#;

let res = ctx.run_wait(
    script.as_bytes(), // script content
    "example.js", // filename
    EvalType::Global, // global eval mode
    EvalFlags::empty(),
);

match res {
  Ok(val) => {
      println!("eval result: {val:?}");
  }
  Err(e) => {
      let mut err_msg = e.message;
      if let Some(tb) = e.traceback {
          err_msg = format!("{err_msg}\n{tb}");
      }
      println!("error: {err_msg}");
  }
}
```

This tutorial provides a starting point for using the `zuqe` crate. The following tutorials will dive into more advanced topics.
