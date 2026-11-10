# Native Function

Native functions are Rust callbacks exposed to JavaScript. The callback signature
is:

```rust
pub type CallbackFn = fn(ctx: &mut ScopedContext, cc: CallContext) -> JSResult;
pub type JSResult = Result<JSValue, i32>;
```

For the full, current API — `CallContext` accessors, argument/error handling,
`FieldDef::FUNC`, `create_native_function`, `create_native_function_with_data`,
global installation, and constructor caveats — see:

- **[Binding native functions](../skills/native-function.md)**
- **[zuqe-embedding skill index](../skills/SKILL.md)**

For native modules see [Native Module](native-module.md); for classes see
[Native Classes](native-classe.md).
