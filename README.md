<div align="center">
  <img src="docs/zuqe-logo.png" alt="Zuqe logo" width="200">
  <h1>Zuqe JavaScript Engine</h1>
</div>

**Zuqe** (pronounced `/zuːk/`, like "zook") is a lightweight JavaScript engine implemented in Rust. It is designed for embedded systems and general‑purpose use, balancing performance, memory safety, and a minimal footprint.

Zuqe is Rust-native, it leverages Rust's philosophy:

- **Memory Safety**: Zero-cost memory safety without garbage collection overhead
- **Concurrency**: Fearless concurrency for potential multi-threaded execution
- **Performance**: Compiled machine code with aggressive optimizations
- **Modern Language**: Good engineering and develope process

Zuqe is named after **朱雀 (Zhuque)**, the Vermilion Bird of Chinese mythology, which represents fire and renewal.

Zuqe is inspired by QuickJS.

## 🎯 Design Goals

- **Lightweight**: Minimal binary size and low memory overhead
- **Rust-Native**: Leverages Rust's memory safety and zero-cost abstractions
- **Embedding-Friendly**: Easy to embed in host applications; Optimized for IoT devices, edge computing, and resource-constrained environments
- **Fast Bytecode Execution**: Optimized interpreter with efficient garbage collection
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
- **Microcontrollers**: 32-bit systems with limited memory (512KB - 2MB RAM)
- **Scripting Engine**: Embeddable scripting for applications
- **Development Tools**: IDEs, build tools, and development utilities
- **Serverless Functions**: Lightweight function execution environments
- **Edge Runtime**: Distributed computing at network edge

## 🏁 Getting Started

### Setup `Cargo.toml`

Add `zuqe` to your dependencies in `Cargo.toml`:

```toml
[dependencies]
zuqe = "0.3.0"
```

### Initialize Runtime and Context

The core of any `zuqe` application is the `Runtime` and `Context`. The `Runtime` manages the engine, while the `Context` provides an isolated environment for script execution.

```rust
use zuqe::{Runtime, Context};

let mut rt = Runtime::create().unwrap();
let mut ctx = rt.create_context_ecma().unwrap();
```

### Run JavaScript

Use `run_wait_with` to execute JavaScript code within the context, wait till script exits.

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
    EvalFlags::empty()
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

### Advanced Usage Examples

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
  - [GC](docs/gc.md)
- **[Examples](examples/)**:
  - [ZJS](docs/zjs.md)

## 🤝 Contributing

Zuqe is an open-source project and welcomes contributions! Whether you're fixing bugs, adding features, or improving documentation, your help is appreciated.

## 📄 License

Licensed under the Apache License 2.0.

Copyright (c) 2025-2026 John Ray <996351336@qq.com> All rights reserved.
