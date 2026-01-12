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

ctx.run_wait(
    script.as_bytes(), // script content
    "example.js", // filename
    EvalType::Global, // global eval mode
    EvalFlags::empty(),
)?;
```

## 4. Module Loading (Optional)

For loading JavaScript modules (`.mjs` files), use should provide customized module loader:

```rust
use zuqe::ModuleDef;

fn js_module_loader(
    mut ctx: Context,
    module_name: &str,
    _: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    match std::fs::read(module_name) {
        Ok(buf) => {
            let md = ModuleDef::compile(&mut ctx, &buf, module_name, false)?;
            Ok(md)
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            Err(-1)
        }
    }
}

rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());
```

This `js_module_loader` function will be called to resolve and load module files. The `is_module` flag in `run_wait_with` can be used to determine the evaluation type based on the file extension or the script's content using `ModuleDef::detect_module`.

This tutorial provides a solid starting point for using the `zhuque` crate. The following tutorials will delve into more advanced topics like defining native classes and modules.
