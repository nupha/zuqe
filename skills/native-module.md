# Modules: ESM Loading and Native Modules

> There are two different module paths:
>
> 1. **JS/ESM modules**: the source or bytecode comes from a host loader. The
>    engine parses, links, and evaluates it. The default loader throws on every
>    import. The host must install a loader.
> 2. **Host/builtin native modules**: Rust registers them with
>    `ScopedContext::create_module` and `FieldDef`. Exports can be functions,
>    constants, or classes.
>
> Loadable cdylib extensions are a third path (zapi ABI). See
> [Writing a zapi Extension](zapi-extension.md). Do not mix `FieldDef` and
> `ZqFieldDef`.

Module internals (ModuleDef, linking/evaluation, cycles) are in
`docs/module.md`.

## 1. ESM module support

### 1.1 Default behavior

Every Runtime starts with:

- **A default normalizer**: absolute paths become URLs. Relative specifiers
  join against the importing module URL. `file://` URLs are canonicalized after
  normalization, so symlink aliases share one cache key.
- **A default loader**: a stub that throws
  `ReferenceError: could not load module '<name>'` on every import.

The engine can compile, but **every import fails until a loader is set**.
Override the normalizer only when you need a different resolution policy.

### 1.2 Callback signatures

```rust
use url::Url;
use zuqe::{JSValue, ModuleDef, ScopedContext};

pub type ModuleNormalizeFn =
    fn(ctx: &mut ScopedContext, base_name: &Url, name: &str) -> Result<Url, i32>;

pub type ModuleLoadFn =
    fn(ctx: &mut ScopedContext, module_name: &Url, attrs: JSValue) -> Result<ModuleDef, i32>;
```

- `attrs` holds import attributes. Example:
  `import data from './config.json' with { type: 'json' }` passes
  `{ type: "json" }` in `attrs`.
- Both callbacks are plain `fn` pointers. They carry no host state. Put state in
  `RuntimeImpl::user_state` and read it with `ctx.rt()`.
- Set them on `Runtime`. `Runtime` derefs to `RuntimeImpl`:

```rust
rt.set_module_normalizer(module_normalize); // optional
rt.set_module_loader(module_loader);       // without it, imports fail
```

### 1.3 File system loader example

```rust
use url::Url;
use zuqe::{JSValue, ModuleDef, ScopedContext, url_to_abspath};

fn module_normalize(
    _ctx: &mut ScopedContext,
    base_name: &Url,
    name: &str,
) -> Result<Url, i32> {
    base_name.join(name).map_err(|_| -1)
}

fn module_loader(
    ctx: &mut ScopedContext,
    module_name: &Url,
    attrs: JSValue,
) -> Result<ModuleDef, i32> {
    let path = match zuqe::url_to_abspath(module_name) {
        Some(p) => p,
        None => {
            ctx.throw_reference_error(&format!(
                "could not load module '{module_name}': not a file:// URL"
            ));
            return Err(-1);
        }
    };

    let is_json = attrs.is_object()
        && attrs
            .get_property_str(ctx, "type")
            .map(|v| v.as_string(true).is_some_and(|s| {
                s.content().encode_utf8(false).as_ref() == "json"
            }))
            .unwrap_or(false);

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            if is_json {
                // Wrap JSON as `export default <json>`.
                let src = format!("export default {content}\n");
                ctx.compile_module(src.as_bytes(), module_name, false)
            } else {
                ctx.compile_module(content.as_bytes(), module_name, false)
            }
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            Err(-1)
        }
    }
}
```

`compile_module(source, module_url, strip)` compiles the source and returns a
`ModuleDef`. `strip` maps to `EvalFlags::STRIP`.

### 1.4 `import.meta`

Modules can use:

- `import.meta.url`: the module URL (`module_name`).
- `import.meta.main`: whether this is the main module.

Module entry points must use `EvalName::BaseUrl(path_to_url(filename)?)`.
`EvalName::Name` gives an error (`module eval requires an URL base`).

### 1.5 List loaded modules

`ScopedContext::loaded_module_names(filter)` lists module names:

```rust
use zuqe::ModuleNameFilter;
let native = ctx.loaded_module_names(ModuleNameFilter::NativeOnly);
let bytecode = ctx.loaded_module_names(ModuleNameFilter::BytecodeOnly);
let all = ctx.loaded_module_names(ModuleNameFilter::All);
```

## 2. Host/builtin native modules

### 2.1 `ScopedContext::create_module`

Source: `src/module/mdef.rs`.

```rust
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

fn js_add(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_number(ctx)?.to_i32();
    let b = cc.arg(1).to_number(ctx)?.to_i32();
    Ok(JSValue::from(a + b))
}

// Must be 'static: create_module takes &'static [FieldDef].
pub static MY_MOD_EXPORTS: &[FieldDef] = &[
    FieldDef::FUNC("add", 2, js_add, 0),
    FieldDef::INT32("ANSWER", 42, JS_PROP_C_W_E),
    // STR, FLOAT64, BOOL, UNDEFINED, LAZY_VALUE, OBJECT, CLASS,
    // GETSET, GETSET_ENUM, and ALIAS are in src/function/fields.rs.
];

pub fn install_my_module(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("my:native", MY_MOD_EXPORTS)?;
    Ok(())
}
```

Key points:

- `entries: &'static [FieldDef]`. Use `static` or `const` with literals or
  constant fields.
- `FieldDef.name` is the export name. `set_export_values` installs values
  during init.
- The module name is a string id. It works with the loader/normalizer policy.
  Native modules are recorded in loaded modules.
- `register_module` is deprecated. Use `create_module` in new code.
- `create_module` returns a `ModuleDef` for inspection or manual evaluation.

### 2.2 Common `FieldDef` constructors

| Constructor | Export |
|---|---|
| `FieldDef::FUNC(name, length, cb, magic)` | Native function |
| `FieldDef::FUNC_ENUM(...)` | Enumerable function |
| `FieldDef::GETSET(name, get, set, magic)` | Getter/setter, non-enumerable |
| `FieldDef::GETSET_ENUM(...)` | Enumerable getter/setter for WebIDL host objects |
| `FieldDef::STR(name, "v", flags)` | String constant |
| `FieldDef::INT32`, `FLOAT64`, `BOOL`, `UNDEFINED` | Basic constants |
| `FieldDef::LAZY_VALUE(name, fn, flags)` | Build a JS value at module evaluation |
| `FieldDef::OBJECT(name, entries, flags)` | Nested object |
| `FieldDef::CLASS(name, || Type::class_id(), flags)` | Class constructor |
| `FieldDef::ALIAS`, `ALIAS_BASE` | Alias |

### 2.3 Runtime integration

1. `Runtime::create()` and `create_context`.
2. Register native modules with `create_module("name", ENTRIES)` during setup or
   later.
3. Set `set_module_normalizer` and `set_module_loader` for a file system,
   virtual FS, aliases, and so on.
4. Run the entry script or module with `EvalName::BaseUrl`.

> The host decides how the loader tells native modules, compiled JS modules,
> and zapi extensions apart. Keep the policy simple and testable. `zjs`
> `js_module_loader` is a complete example.

## 3. zapi module boundary

zapi extensions ship as **cdylibs**:

- Export lists use `ZqFieldDef` (`FUNC`, `CLASS`, `GETSET`, constants, ...).
  The entry point is generated by `#[zq_module(exports = [...])]` as
  `zq_module_register`.
- The host must enable the `zapi` feature. Its loader calls
  `ScopedContext::load_zapi_extension(path)`. The zjs loader recognizes `.so`,
  `.dylib`, and `.dll` by suffix.
- Callback signature is `ZqBodyFn`. Types are `ZqRawCtx`, `ZqCallInfo`, and
  `ZqRawValue`. Errors use `ctx.throw_*` and return `ZqError`.
- A zapi class `gc_trace` must report every GC field it holds. The engine
  re-scans old zapi instance payloads on each minor, because the zapi ABI has no
  write barrier.
- The host caches extension libraries for the process and keeps them mapped. It
  does not unload them.

See [Writing a zapi Extension](zapi-extension.md).

## 4. GC and scope notes

> The only rule source is [GC Safety Rules](gc-safety.md), rules 0-5.

Module records (capability, resolver, namespace, async parent, and so on) are
long-lived old containers. Host writes go through setters or guards per Rule 5.
Module-level cached `JSValue`/`Object` must be covered by `GcTrace`. Keep
temporary values alive across JS re-entry per Rule 4.

## 5. Anti-patterns

- Use deprecated `register_module` instead of `create_module`.
- Pass a non-`'static` `FieldDef` table to `create_module`.
- Keep hidden global mutable state in a normalizer or loader. `fn` pointers
  cannot capture.
- Mix native modules (Rust registration) with JS modules (loader compilation).
- Use host Rust APIs in a zapi extension, or put `Zq*` ABI types in host code.
- Use `EvalName::Name` instead of `BaseUrl` for a module entry.
