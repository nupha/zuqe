<div align="center">
  <img src="docs/zuqe-logo.png" alt="Zuqe logo" width="200">
  <h1>
    Zuqe JavaScript Engine<br>
    <span style="font-size:0.7em; color:#777;">Empower your Rust application with native JavaScript scripting.</span>
  </h1>
</div>

Want to supercharge your Rust application with JavaScript scripting? Look no further than **Zuqe** — a JavaScript runtime that is immune to host‑stack overflow and integrates at minimal cost.

**Zuqe** (pronounced `/zuːk/`, like "zook") is a lightweight JavaScript engine implemented in Rust. It is designed to be embedded into Rust applications with low memory overhead and a small binary size, balancing efficiency, safety, and ease of embedding.

**Conformance:** Zuqe implements a comprehensive set of ES2023+ features and passes a substantial portion of the [Test262](https://github.com/tc39/test262) conformance suite.

**The name:** Zuqe is named after **朱雀 (Zhuque)**, the Vermilion Bird of Chinese mythology — a creature of fire, rebirth, and guardianship that shape the engine's design. In the spirit of QuickJS, Zuqe is a Rust‑native reimagining of the embeddable JavaScript engine. [Read the full story →](docs/about.md)

## 🎯 Design Goals

- **Lightweight**: Small binary footprint and low memory overhead for IoT devices, edge computing, and resource-constrained environments
- **Rust-Native**: Leverages Rust's memory safety, zero-cost abstractions and modern toolchain to boost engineering process, with seamless async bridging that connects JS Promises with Rust `Future`s in both directions
- **Embedding-Friendly**: Easy to embed in and integrate with host applications
- **Stackless Interpreter**: Call frames live off the native Rust stack, so deep recursion can't overflow the host — bounded by a precise, configurable quota
- **Configurable Footprint**: Built-in objects and full Unicode support are individually gated behind Cargo features, so you can trim the binary down for embedded targets
- **Fast Bytecode Execution**: Optimized interpreter with efficient scope-based garbage collection
- **Standards Compliance**: Evolving implementation of modern JavaScript standards

## 🚀 ECMAScript Features

Zuqe's JavaScript support spans modern syntax and a broad set of built-in objects and APIs:

### Core Language Features

- **Modern Functions**: Arrow functions, async functions, generator functions
- **Classes**: Full ES6+ class implementation with inheritance, private fields, and static members
- **Modules**: ES6 modules with import/export, dynamic imports, and top-level await
- **Destructuring**: Array and object destructuring with rest parameters
- **Template Literals**: String interpolation and tagged templates
- **Iterators and Generators**: Full implementation of iterator protocol and generator functions

### Built-in Objects and APIs

- **Arrays**: Complete `Array.prototype` methods (`map`, `filter`, `reduce`, …)
- **Collections**: `Map`, `Set`, `WeakMap`, `WeakSet`
- **Promises**: Full `Promise` implementation with `async`/`await` support
- **JSON**: `JSON.parse()` and `JSON.stringify()` with proper error handling
- **Regular Expressions**: PCRE2-based regex engine with modern ES features
- **Typed Arrays**: `Int8Array`, `Uint8Array`, `Float32Array`, … plus `DataView`/`ArrayBuffer`
- **Reflection**: `Reflect` object with comprehensive reflection capabilities
- **Proxy**: Complete object-interception implementation (get/set/apply/… traps)
- **Symbols**: Symbol primitives and well-known symbols
- **Date & Time**: `Date` object with timezone support
- **Math**: Comprehensive `Math` library
- **Errors**: Structured error types with stack traces

### Design Highlights

Engine-level design choices that set Zuqe apart:

- **Stackless Interpreter**: Call frames live in a managed bump-arena (spilling to the heap), so deep recursion can't overflow the host stack and stack usage is bounded by a precise, configurable quota
- **Hidden Classes & Inline Caches**: Objects share `Shape`s and use per-opcode inline caches for fast, predictable property access
- **String Ropes**: Concatenations are kept as lazy ropes rather than eagerly copied, keeping string operations memory-efficient
- **NaN-Boxing Values**: JS values are encoded in a compact 64-bit NaN-boxed representation for cache-friendly, low-overhead storage
- **Scope-Based Garbage Collection**: Generational, scope-bounded GC with a dedicated StackFrame GC partition
- **Bidirectional Async Bridge**: JS `Promise`s and Rust `Future`s interoperate in both directions (feature `async_support`)

## 🔥 Where Zuqe Shines

- **Scripting for Rust Services**: Hot-reload business logic, plugins, and user scripts into your Rust application — without restarting it
- **Edge & Embedded Runtimes**: Tiny binary and low memory footprint let Zuqe fit IoT and edge devices
- **Developer Tools & Sandboxes**: Safe in-process scripting for IDEs, CLIs, and build tooling

## 🏁 Getting Started

### Setup `Cargo.toml`

Add `zuqe` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
zuqe = "0.9.0"
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
  - [About Zuqe](docs/about.md) — the name, the philosophy, and the engine
  - [Basic Usage](docs/basic.md)
  - [Modules](docs/module.md)
  - [Native Functions](docs/native-function.md)
  - [Native Classes](docs/native-classe.md)
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
