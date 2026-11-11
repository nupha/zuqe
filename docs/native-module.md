# Native Module

Native modules are Rust-backed ES modules registered with
`ScopedContext::create_module(name, &'static [FieldDef])`. They are distinct from
JavaScript/ESM modules (which the host supplies through a module loader) and from
zapi `cdylib` extensions (which use the `Zq*` ABI).

The complete guide — `create_module`, `FieldDef` variants, ESM `set_module_loader`
/ `set_module_normalizer`, import attributes, `import.meta`, and the zapi
boundary — is in:

- **[Modules: ESM Loading and Native Modules](../skills/references/native-module.md)**
- **[Writing a zapi Extension (cdylib)](../skills/references/zapi-extension.md)**

Minimal registration:

```rust
use zuqe::{FieldDef, JSResult, JSValue, ScopedContext};

fn js_add(ctx: &mut ScopedContext, cc: zuqe::CallContext) -> JSResult {
    let a = cc.arg(0).to_number(ctx)?.to_i32();
    let b = cc.arg(1).to_number(ctx)?.to_i32();
    Ok(JSValue::from(a + b))
}

static EXPORTS: &[FieldDef] = &[FieldDef::FUNC("add", 2, js_add, 0)];

ctx.create_module("my:native", EXPORTS)?;
```

Internal module graph, linking, and evaluation are described in
[Module System](module.md).
