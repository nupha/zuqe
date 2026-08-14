# ESM Module Support

This guide explains how to enable and use ES (ECMAScript) module support in the Zuqe runtime. ESM modules provide a standardized way to organize JavaScript code into reusable modules with `import` / `export`.

## Overview

Zuqe supports ESM modules by providing callbacks for module name normalization and module loading. To enable module support, configure the runtime with these callbacks using `Runtime::set_module_loader`.

Callback function signatures are defined for module handling:

The module loader callback receives a `ScopedContext`, the normalized module name (a `file://` URL), an `attrs` `JSValue` carrying the module's import attributes (e.g. `{ type: "json" }` from `import x from './d.json' with { type: 'json' }`), and an opaque pointer. It is responsible for loading the module source and compiling it into a `ModuleDef`.

## Setting Up Module Loader

Use `Runtime::set_module_loader` to configure module support:

```rust
rt.set_module_loader(
    None,           // optional name normalizer, None means to use the builtin default one
    js_module_loader,
    std::ptr::null_mut(), // an opaque raw pointer
);
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
    _opaque: *mut std::ffi::c_void,
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
    _opaque: *mut std::ffi::c_void,
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
    rt.set_module_loader(
        Some(module_normalize), // could be None to use default normalizer
        module_loader,
        std::ptr::null_mut()
    );

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

When you pass `None` for the `ModuleNormalizeFn` parameter:

```rust
rt.set_module_loader(None, module_loader, std::ptr::null_mut());
```

A default simple normalization function is used that provides basic module name resolution according to ES module specifications.

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
