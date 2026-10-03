# ESM Module Support

This guide explains how to enable and use ES (ECMAScript) module support in the Zuqe runtime. ESM modules provide a standardized way to organize JavaScript code into reusable modules with `import` / `export`.

## Overview

Zuqe supports ESM modules by providing callbacks for module name normalization and module loading. Both have a builtin default installed in every runtime, so neither has to be registered for the engine to compile: name resolution works out of the box, and every import fails with a `ReferenceError` until `Runtime::set_module_loader` supplies a loader. Name resolution only needs `Runtime::set_module_normalizer` when a host wants different resolution.

Callback function signatures are defined for module handling:

The module loader callback receives a `ScopedContext`, the normalized module name (a `file://` URL), and an `attrs` `JSValue` carrying the module's import attributes (e.g. `{ type: "json" }` from `import x from './d.json' with { type: 'json' }`). It is responsible for loading the module source and compiling it into a `ModuleDef`.

## Setting Up Module Loader

Use `Runtime::set_module_loader` to supply a loader (until then every import throws):

```rust
rt.set_module_loader(js_module_loader);
```

## Complete Example

Here's a complete example showing how to implement script file based module loading:

```rust
use url::Url;
use zuqe::{JSValue, ModuleDef, Runtime, ScopedContext, url_to_abspath};

// Module name normalization (optional)
fn module_normalize(
    ctx: &mut ScopedContext,
    base_name: &Url,
    name: &str,
) -> Result<Url, i32> {
    // Resolve relative to the importing module's URL. `Url::join` handles
    // `.`/`..` and works uniformly for `file:` and `http:` bases.
    base_name.join(name).map_err(|_| -1)
}

// Module loader function
fn module_loader(
    ctx: &mut ScopedContext,
    module_name: &Url,
    attrs: JSValue,
) -> Result<ModuleDef, i32> {
    // `module_name` is the normalized module name (a `file://` URL). Convert it
    // back to a local path for filesystem access; dispatch by scheme here for
    // future `https:` support.
    let path = match zuqe::url_to_abspath(module_name) {
        Some(p) => p,
        None => {
            ctx.throw_reference_error(&format!(
                "could not load module '{module_name}': not a file:// URL"
            ));
            return Err(-1);
        }
    };

    // Support JSON modules via import attributes, e.g.
    //   import data from './config.json' with { type: 'json' };
    let is_json = attrs.is_object()
        && attrs
            .get_property_str(ctx, "type")
            .map(|v| {
                v.as_string(true)
                    .is_some_and(|s| s.content().encode_utf8(false).as_ref() == "json")
            })
            .unwrap_or(false);

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            if is_json {
                // Wrap JSON as `export default <json>` so it can be imported as
                // a normal ES module namespace.
                let mut src = String::from("export default ");
                src.push_str(&content);
                src.push('\n');
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

fn main() -> Result<(), i32> {
    let mut rt = Runtime::create()?;

    // Setup module loading callbacks
    rt.set_module_normalizer(module_normalize); // omit to keep the default normalizer
    rt.set_module_loader(module_loader);

    // Create execution context
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)
        })
        .expect("fail to create context");

    // .. omitted ...

    Ok(())
}
```

## Default Normalization

A runtime starts out with a builtin normalizer, so `set_module_normalizer` only needs to be called to _override_ it. That default is a simple resolution following the ES module specifications: an absolute path is turned into a URL directly, anything else is joined against the importing module's URL. Note that `file://` URLs are canonicalized centrally after normalization, so symlink aliases collapse to a single cache key for custom normalizers too.

There is no "unset" state, and the builtin normalizer is not exported — a host that needs to go back to it should keep its own copy of the pointer. In practice resolution policy is picked once at startup, so this rarely matters.

## Default Loader

Likewise `module_load_func` starts out holding a stub that throws a `ReferenceError` for every import, because where modules come from is entirely host policy. Until `set_module_loader` is called, every `import` fails with `could not load module '<name>'`:

```rust
// Any import at this point throws:
//   ReferenceError: could not load module './foo.js'
rt.set_module_loader(module_loader);
```

## Import.meta Support

Modules have access to `import.meta` which provides metadata about the module:

- `import.meta.url`: The URL of the module
- `import.meta.main`: Boolean indicating if this is the main module

## Best Practices

1. **Error Messages**: Provide clear error messages in your module loader
2. **Path Resolution**: Implement proper path resolution in your normalize function
3. **File Extensions**: Consider handling different file extensions (.js, .mjs, .ts)
4. **Caching**: Implement module caching to avoid recompiling already loaded modules
5. **Security**: Validate module paths to prevent directory traversal attacks

## Write a native module

You have learned how to enable module support in zuqe. Next, let's see how to write the native module and expose it to JavaScript world. Read [Native Module](native-module.md) for detail.
