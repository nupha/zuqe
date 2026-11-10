# Basic Usage

This page is a short pointer. The full embedding guide — Cargo features,
`Runtime`/`Context` setup, synchronous and async `eval_wait`, `EvalName`,
`EvalType`, `EvalFlags`, GC scopes, resource limits, and module hooks — lives in:

- **[Embedding Basics: Runtime, Context, and Execution](../skills/embedding-basics.md)**
- **[zuqe Embedding Skill](../skills/SKILL.md)**

Minimal shape:

```rust
use zuqe::{Runtime, ScopedContext};

let mut rt = Runtime::create()?;
let mut ctx = rt.create_context(|cx| ScopedContext::ECMA_CONTEXT_SETUP(cx))?;
```

Then execute with `Context::eval_wait(...).await` (default `async_support`) or
`Context::eval(...)`; see the skill for the complete, current signatures.

Promise ↔ Future integration is covered in
**[Promise and Rust Future Bridge](../skills/promise.md)**.
