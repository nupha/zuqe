<div align="center">
  <img src="docs/zuqe-logo.png" alt="Zuqe logo" width="200">
  <h1>
    Zuqe JavaScript Engine<br>
    <span style="font-size:0.7em; color:#777;">Empower your Rust application with native JavaScript scripting.</span>
  </h1>
</div>

Want to supercharge your Rust application with JavaScript scripting? Look no further than **Zuqe** — a JavaScript engine that is immune to host‑stack overflow and integrates at minimal cost.

**Zuqe** (pronounced `/zuːk/`, like "zook") is a lightweight JavaScript engine implemented in Rust. It is designed to be embedded into Rust applications with low memory overhead and a small binary size, balancing efficiency, safety, and ease of embedding.

**Conformance:** Zuqe implements a comprehensive set of ES2024 features and passes a substantial portion of the [Test262](https://github.com/tc39/test262) conformance suite.

**The name:** Zuqe is named after **朱雀 (Zhuque)**, the Vermilion Bird of Chinese mythology — a creature of fire, rebirth, and guardianship that shape the engine's design. Zuqe is a Rust-native reimagining of the embeddable JavaScript engine. [Read the full story →](docs/about.md)

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
- **Regular Expressions**: QuickJS `libregexp` C library via Rust bindings (`lre`)
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
- **Tiered JIT**: Hot bytecode functions compile to Cranelift machine code while the interpreter stays the reference; compilation is polled automatically at engine safe points, and anything the backend cannot translate falls back to the interpreter (feature `jit`)

## 🔥 Where Zuqe Shines

- **Scripting for Rust Services**: Hot-reload business logic, plugins, and user scripts into your Rust application — without restarting it
- **Edge & Embedded Runtimes**: Tiny binary and low memory footprint let Zuqe fit IoT and edge devices
- **Developer Tools & Sandboxes**: Safe in-process scripting for IDEs, CLIs, and build tooling

## 🧩 Zapi — Version-Independent Native Extensions

Zuqe ships **zapi**, an extension API that decouples the engine version from the extension version. Extensions compiled against the zapi ABI can be loaded by **any** version of the zuqe host — the engine can evolve without breaking existing extensions.

```js
// JS side: extensions are just ES modules
import * as my from "./libmymod.so";
my.add(2, 3); // 5
```

Extensions are compiled as `cdylib` dynamic libraries. The zapi API is defined by two crates (`zuqe_zapi` + `zuqe_zapi_macros`) with a stable ABI boundary. You write functions, classes, and constants using safe Rust with `#[zq_function]`, `#[zq_class]`, `#[zq_module]` — the macros generate the C ABI glue. Users drop the `.dylib`/`.so`/`.dll` next to their scripts and `import` it, regardless of which zuqe version they're running.

This means extension authors ship a single binary; it works across engine upgrades. See [Writing a Zapi Extension](docs/zapi-extension.md) for the full guide.

## 🏁 Getting Started

### Setup `Cargo.toml`

Add `zuqe` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
zuqe = "0.12.0"
tokio = { version = "1", features = ["full"] } # for the async entry point below
```

Zuqe provides several Cargo features to customize the build:

| Feature              | What's for                                                              |
| -------------------- | ----------------------------------------------------------------------- |
| `jit`                | Tiered bytecode JIT (Cranelift) with worker-thread compilation          |
| `async_support`      | Enable Promise↔Future bridge and `Runtime::run_async()`                 |
| `gc_generations`     | Generational GC with minor collections and write barriers               |
| `all_unicode`        | Full Unicode support (XID, normalization)                               |
| `zapi`               | Load zapi `cdylib` extensions as ES modules                             |
| `intl402`            | `Intl` support (ICU-backed)                                             |
| `persistent`         | Bytecode persistence helpers                                            |
| `crossbeam_channel`  | Use `crossbeam::channel` instead of `std::sync::mpsc`                   |
| `serde_json_support` | Enable `JSValue` ↔ `serde_json::Value` conversion                       |
| `builtin_*`          | Individual built-in feature gates (symbol, proxy, mapset, regexp, etc.) |

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

With the `async_support` feature, `Context::eval_wait` is an async
function. It executes the entry and drives the event loop until all jobs and
promises settle; the context's own runtime is used, so there is no runtime
argument:

```rust
use zuqe::{EvalFlags, EvalName, EvalType, Exception, ExitStatus, Runtime, ScopedContext};

#[tokio::main]
async fn main() {
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

    let res = ctx
        .eval_wait(
            script.as_bytes(),
            &EvalName::Name("example.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await;

    match res {
        ExitStatus::Value(val) => {
            let s = ctx.with_root_scope(|mut scope| {
                val.to_string(&mut scope).unwrap().content().encode_utf8(false)
            });
            println!("eval result: {s}");
        }
        ExitStatus::Exception(Exception::Error(err)) => {
            let msg = ctx.with_root_scope(|mut scope| {
                err.to_string(&mut scope).unwrap().content().encode_utf8(false)
            });
            println!("error: {msg}");
        }
        other => println!("stopped: {other:?}"),
    }
}
```

`Context::eval_wait` creates its own GC scope, so no manual `with_new_scope` is
needed. For the full execution guide (sync mode, `eval`, `EvalName`, scopes,
resource limits), see [Basics: Runtime, Context, and Execution](skills/references/basics.md).

### Embedding API Highlights

Beyond basic execution, Zuqe provides a rich embedding API:

**Native Functions & Classes** — Wrap Rust functions as JS callables and define full classes with the `def_native_object!` macro:

```rust
use std::cell::RefCell;
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, def_native_object};

#[derive(Debug)]
enum FileDesc { Stdin, Stdout, Stderr, File(std::fs::File) }
// Payload holds only OS handles: no GC nodes to trace.
zuqe::gc_lite::unsafe_empty_gc_trace!(FileDesc);

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
    Ok(JSValue::from(n))
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

Documentation is split by audience:

- **[Skills](skills/SKILL.md)** — how to use and embed Zuqe from Rust. Start here
  if you are writing host code or an extension:
  - [Basics: Runtime, Context, and Execution](skills/references/basics.md) — Runtime/Context, `eval`/`eval_wait`, limits
  - [Native Function Binding](skills/references/native-function.md)
  - [Native Classes](skills/references/native-class.md)
  - [Modules: ESM Loading and Native Modules](skills/references/native-module.md)
  - [Promise and Rust Future Bridge](skills/references/promise.md)
  - [Tiered JIT Usage](skills/references/jit.md)
  - [Writing a zapi Extension (cdylib)](skills/references/zapi-extension.md)
  - [GC Safety Rules](skills/references/gc-safety.md)
  - [Workflow and Review Checklist](skills/references/workflow-checklist.md)
- **[Docs](docs/)** — internals, design, build, and tool manuals:
  - [About Zuqe](docs/about.md) — the name, philosophy, and design goals
  - [Garbage Collection](docs/gc.md) — collector implementation
  - [Running Loop](docs/loop.md) — tick internals
  - [Module System](docs/module.md) — module graph internals
  - [Promise internals](docs/promise.md)
  - [Tiered JIT](docs/jit.md) — pipeline and design
  - [ZJS](docs/zjs.md) — the `zjs` command-line interpreter
  - [Build on Windows](docs/build-windows-gnu.md) and
    [cross-compile](docs/build-cross.md)
  - [Known issue](docs/known-issue.md)
- **[Examples](examples/)**

## 🤝 Contributing

Zuqe is an open-source project and welcomes contributions! Whether you're fixing bugs, adding features, or improving documentation, your help is appreciated.

## 📄 License

Licensed under the Apache License 2.0.

Copyright (c) 2025-2026 John Ray <996351336@qq.com> All rights reserved.
