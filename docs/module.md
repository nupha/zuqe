# ESM Module Support

This guide explains how to enable and use ES (ECMAScript) module support in the Zuqe runtime. ESM modules provide a standardized way to organize JavaScript code into reusable modules with import/export statements.

## Overview

Zuqe supports ESM modules by providing callbacks for module name normalization and module loading. To enable module support, you must configure the runtime with these callbacks using `Runtime::set_module_loader`.

Callback function signatures are defined for module handling:

### ModuleNormalizeFn

```rust
pub type ModuleNormalizeFn = fn(
    ctx: Context,
    base_name: &str,
    name: &str,
    opaque: *mut ::core::ffi::c_void,
) -> Result<String, i32>;
```

This function normalizes module names during resolution. It's called when resolving relative imports against a base module name. If you pass `None` for this parameter to `set_module_loader`, a default normalization callback will be used.

### ModuleLoadFn

```rust
pub type ModuleLoadFn = fn(
    ctx: Context,
    module_name: &str,
    opaque: *mut ::core::ffi::c_void
) -> Result<ModuleDef, i32>;
```

This function loads a module given its normalized name and returns a compiled module definition.

## Setting Up Module Loader

Use `RuntimeImpl::set_module_loader` to configure module support:

```rust
rt.set_module_loader(
    normalize_func, // name normalizer func - optional
    load_func,     // module loader func
    opaque,
);
```

## Complete Example

Here's a complete example showing how to implement script file based module loading:

```rust
use zuqe::{Runtime, Context, ModuleDef, EvalFlags, EvalType};

// Module name normalization (optional)
fn module_normalize(
    mut ctx: Context,
    base_name: &str,
    name: &str,
    _: *mut std::ffi::c_void,
) -> Result<String, i32> {
    // Simple normalization: if name starts with './' or '../', resolve against base
    if name.starts_with("./") || name.starts_with("../") {
        let base_path = std::path::Path::new(base_name).parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let normalized = base_path.join(name);
        Ok(normalized.to_string_lossy().to_string())
    } else {
        // Return as-is for absolute imports or bare module names
        Ok(name.to_string())
    }
}

// Module loader function
fn module_loader(
    mut ctx: Context,
    module_name: &str,
    _: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    // module_name is the normalized module name
    // suppose it's a js file in this example.

    // Try to read the file
    match std::fs::read(module_name) {
        Ok(buf) => {
            // Compile source script using ModuleDef::compile() method:
            // - Takes source bytes, filename, and strip flag
            // - Compiles the source as an ES module (not global code)
            // - Returns a `ModuleDef` that can be evaluated
            let md = ModuleDef::compile(&mut ctx, &buf, module_name, false)?;
            Ok(md)
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            Err(-1)
        }
    }
}

 fn main()   {
    let mut rt = Runtime::create()?;

    // Setup module loading callbacks
    rt.set_module_loader(
        Some(module_normalize), // could be None to use default normalizer
        module_loader,
        std::ptr::null_mut()
    );

    // Create execution context
    let mut ctx = rt.create_context_ecma().expect("fail to create context");

    // .. omitted ...

    Ok(())
}
```

## Default Normalization

When you pass `None` for the `ModuleNormalizeFn` parameter:

```rust
rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());
```

A default normalization function is used that provides basic module name resolution according to ES module specifications.

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
