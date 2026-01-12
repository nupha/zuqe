<div align="center">
  <img src="./zuqe-logo.png" alt="Zuqe logo" width="200">
  <h1>Zuqe JS Engine</h1>
</div>

## Introduction

**Zuqe** (pronounced `/zuːk/`, like "zook") is a lightweight JavaScript engine implemented in Rust. It's designed specifically for embedding and general-purpose use. Unlike heavyweight engines (V8 etc), Zuqe prioritizes memory safety, small footprint, and fast startup time while maintaining substantial ECMAScript compliance.

Zuqe is Rust-native, it leverages Rust's philosophy:

- **Memory Safety**: Zero-cost memory safety without garbage collection overhead
- **Concurrency**: Fearless concurrency for potential multi-threaded execution
- **Performance**: Compiled machine code with aggressive optimizations
- **Modern Language**: Good engineering and develope process

Zuqe is named after **朱雀 (Zhuque)**, the Vermilion Bird of Chinese mythology, which represents fire and renewal.

Zuqe is inspired by QuickJS.

## Design Goals

- **Lightweight**: Minimal binary size and low memory overhead
- **Rust-Native**: Leverages Rust's memory safety and zero-cost abstractions
- **Embedding-Friendly**: Easy to embed in host applications; Optimized for IoT devices, edge computing, and resource-constrained environments
- **Fast Bytecode Execution**: Optimized interpreter with efficient garbage collection
- **Standards Compliance**: Evolving implementation of modern JavaScript standards

## ECMAScript Features

Zuqe implements a comprehensive set of ES2023+ features, making it suitable for modern JavaScript development:

### Core Language Features

- **Modern Function**: Arrow functions, async functions, generator functions
- **Class**: Full ES6+ class implementation with inheritance, private fields, and static members
- **Module**: ES6 modules with import/export, dynamic imports, and top-level await
- **Destructuring**: Array and object destructuring with rest parameters
- **Template Literals**: String interpolation and tagged templates
- **Iterators and Generators**: Full implementation of iterator protocol and generator functions

### Built-in Objects and APIs

- **Array**: Complete Array.prototype methods including map, filter, reduce
- **Collection Types**: Map, Set, WeakMap, WeakSet
- **Promise**: Full Promise implementation with async/await support
- **JSON**: JSON.parse() and JSON.stringify() with proper error handling
- **Regular Expressions**: PCRE2-based regex engine with modern features
- **Typed Arrays**: Int8Array, Uint8Array, Float32Array, etc., plus DataView
- **Reflection APIs**: Reflect object with comprehensive reflection capabilities

### Advanced Features

- **Proxy**: Complete proxy implementation for object interception
- **Symbol**: Symbol primitives and well-known symbols
- **Date and Time**: Date object with timezone support
- **Math**: Comprehensive Math library implementation
- **Error**: Structured error types with stack traces

## Use Cases

- **IoT Devices**: Resource-constrained environments requiring JavaScript scripting
- **Microcontrollers**: 32-bit systems with limited memory (512KB - 2MB RAM)
- **Scripting Engine**: Embeddable scripting for applications
- **Development Tools**: IDEs, build tools, and development utilities
- **Serverless Functions**: Lightweight function execution environments
- **Edge Runtime**: Distributed computing at network edge
