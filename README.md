<div align="center">
  <img src="docs/zuqe-logo.png" alt="Zuqe logo" width="200">
  <h1>Zuqe JavaScript Engine</h1>
</div>

**Zuqe** (pronounced `/zuːk/`, like "zook") is a lightweight JavaScript engine implemented in Rust. It is designed for embedded systems and general‑purpose use, balancing performance, memory safety, and a minimal footprint.

Zuqe is named after **朱雀 (Zhuque)**, the Vermilion Bird of Chinese mythology, which represents fire and renewal.

Zuqe is inspired by QuickJS.

## 🎯 Design Goals

- **Lightweight**: Small binary footprint and low memory overhead for IoT devices, edge computing, and resource-constrained environments
- **Rust-Native**: Leverages Rust's memory safety, zero-cost abstractions and modern toolchain to boost engineering process, with seamless async bridging that connects JS Promises with Rust `Future`s in both directions
- **Embedding-Friendly**: Easy to embed in and integrate with host applications
- **Stackless Interpreter**: Call frames live in a managed bump-arena (spilling to the heap), not on the native Rust stack — deep recursion can't overflow the host stack, and stack usage is bounded by a precise, configurable quota
- **Configurable Footprint**: Built-in objects and full Unicode support are individually gated behind Cargo features, so you can trim the binary down for embedded targets
- **Fast Bytecode Execution**: Optimized interpreter (hidden-class objects, inline caches) with efficient scope-based garbage collection
- **Standards Compliance**: Evolving implementation of modern JavaScript standards

## 🚀 ECMAScript Features

Zuqe implements a comprehensive set of ES2023+ features, making it suitable for modern JavaScript development:

### Core Language Features

- **Modern Functions**: Arrow functions, async functions, generator functions
- **Classes**: Full ES6+ class implementation with inheritance, private fields, and static members
- **Modules**: ES6 modules with import/export, dynamic imports, and top-level await
- **Destructuring**: Array and object destructuring with rest parameters
- **Template Literals**: String interpolation and tagged templates
- **Iterators and Generators**: Full implementation of iterator protocol and generator functions

### Built-in Objects and APIs

- **Arrays**: Complete Array.prototype methods including map, filter, reduce
- **Collection Types**: Map, Set, WeakMap, WeakSet
- **Promises**: Full Promise implementation with async/await support
- **JSON**: JSON.parse() and JSON.stringify() with proper error handling
- **Regular Expressions**: PCRE2-based regex engine with modern features
- **Typed Arrays**: Int8Array, Uint8Array, Float32Array, etc., plus DataView
- **Reflection APIs**: Reflect object with comprehensive reflection capabilities

### Advanced Features

- **Proxy**: Complete proxy implementation for object interception
- **Symbols**: Symbol primitives and well-known symbols
- **Date and Time**: Date object with timezone support
- **Math**: Comprehensive Math library implementation
- **Error Handling**: Structured error types with stack traces

## 📋 Project Status

Zuqe is under active development. It currently passes a substantial portion of the [Test262](https://github.com/tc39/test262) test suite and is evolving toward production readiness.

## 🎪 Use Cases

- **IoT Devices**: Resource-constrained environments requiring JavaScript scripting
- **Scripting Engine**: Embeddable scripting for applications
- **Development Tools**: IDEs, build tools, and development utilities
- **Serverless Functions**: Lightweight function execution environments
- **Edge Runtime**: Distributed computing at network edge

## 🏁 Getting Started

### Setup `Cargo.toml`

Add `zuqe` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
zuqe = "0.8.0"
```

Zuqe provides several Cargo features to customize the build:

| Feature              | Description                                                             | Default |
| -------------------- | ----------------------------------------------------------------------- | ------- |
| `async_support`      | Enable Promise↔Future bridge and `Runtime::run_async()`                 | on      |
| `all_unicode`        | Full Unicode support (XID, normalization)                               | on      |
| `crossbeam_channel`  | Use `crossbeam::channel` instead of `std::sync::mpsc`                   | off     |
| `serde_json_support` | Enable `JSValue` ↔ `serde_json::Value` conversion                       | off     |
| `builtin_*`          | Individual built-in feature gates (symbol, proxy, mapset, regexp, etc.) | on      |

### Initialize Runtime and Context

The core of any `zuqe` application is the `Runtime` and `Context`. The `Runtime` manages the engine, while the `Context` provides an isolated execution environment.

```rust
use zuqe::{Context, Runtime, ScopedContext};

fn main() {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)
        })
        .unwrap();

    // Ready to execute JavaScript with ctx
}
```

`ScopedContext::ECMA_CONTEXT_SETUP` is a built‑in helper that initializes the Context with a standard ECMAScript‑compliant environment, wiring up well-known globals, built‑in objects, and runtime behavior. You can also pass your own setup function to `create_context()` for custom host APIs or restricted environments.

### Run JavaScript

Use `with_new_scope` and `eval_wait` to execute code within a bounded scope, waiting until all jobs and promises settle:

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

`with_new_scope` executes the closure inside a dedicated, bounded scope. Each call creates a fresh scope, runs the script within it, and tears it down when the closure returns. This isolates local variables and temporary values, preventing leaks between executions and allowing safe reuse of the same `Context`.

### Embedding API Highlights

Beyond basic execution, Zuqe provides a rich embedding API:

**Native Functions & Classes** — Wrap Rust functions as JS callables and define full classes with the `def_native_object!` macro:

```rust
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, def_native_object};

def_native_object!("File", JSFile, RefCell<FileDesc>, CLASS_FILE_PROTO, None);

const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::FUNC("fileno", 0, js_file_fileno, 0),
    FieldDef::FUNC("puts",   1, js_file_puts,   1),
];
```

**Native Modules** — Export Rust functions and constants as ES modules via `ctx.create_module()`:

```rust
const MOD_OS_ENTRIES: &[FieldDef] = &[
    FieldDef::FUNC("now", 0, js_os_now, 0),
    FieldDef::INT32("SIGINT", 2, JS_PROP_CONFIGURABLE),
];

ctx.create_module("os", MOD_OS_ENTRIES)?;
```

**Async Support** — Convert JS Promises to Rust `Future`s and vice versa:

```rust
// Promise → Rust Future
let promise = Promise::try_from(val)?;
let fut = promise.into_future(&mut scope)?;
let result: Result<JSValue, JSValue> = fut.await;

// Rust Future → Promise (via channel)
let (promise, tx) = Promise::create_with_channel(ctx, |cx, n: i32| {
    Ok(JSValue::Int(n))
})?;
tokio::spawn(async move { tx.send(42).ok(); });
```

**Garbage Collection** — Configure GC heap limits and register custom trace handlers for native objects:

```rust
rt.gc_set_memory_limit(64_000_000);  // 64 MB heap limit
rt.gc_set_trace_handler(|tracer| {
    my_module_gc_trace(tracer);
});
```

For more comprehensive examples, check the [examples](examples/) directory which includes:

- Basic JavaScript execution
- Module system usage
- Promise and async/await
- Custom native functions and classes
- Error handling and debugging

## 📚 Documentation

- **[User Guide](docs/)**:
  - [Basic Usage](docs/basic.md)
  - [Modules](docs/module.md)
  - [Native Functions](docs/native-function.md)
  - [Native Classe](docs/native-classe.md)
  - [Native Module](docs/native-module.md)
  - [Running Loop](docs/loop.md)
  - [Promise](docs/promise.md)
  - [GC](docs/gc.md)
- **[Examples](examples/)**:
  - [ZJS](docs/zjs.md)

## 🤝 Contributing

Zuqe is an open-source project and welcomes contributions! Whether you're fixing bugs, adding features, or improving documentation, your help is appreciated.

## 📄 License

Licensed under the Apache License 2.0.

Copyright (c) 2025-2026 John Ray <996351336@qq.com> All rights reserved.
