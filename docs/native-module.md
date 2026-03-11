# Native Module

This tutorial demonstrates how to create and register native Rust modules in the `zuqe` crate. As a concrete example, we will build a small `os` module that exposes operating system–related helpers to JavaScript.

## 1. Module Functions

First, define the Rust functions that will be exported as part of the `os` module. These are ordinary native functions that take a `&mut ScopedContext` and a `CallContext`, and return a `JSResult`.

```rust
use zuqe::{CallContext, JSResult, JSValue, ScopedContext};

/// Return the current monotonic time in milliseconds.
fn js_os_now(_: &mut ScopedContext, _: CallContext) -> JSResult {
    let tm = unsafe {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    };

    Ok(JSValue::from(tm as f64 / 1e6))
}

/// Test whether the given file descriptor refers to a TTY.
fn js_os_isatty(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let fd = cc.arg(0).to_i32(ctx)?;
    Ok(JSValue::Bool(unsafe { libc::isatty(fd) != 0 }))
}
```

In your own module you can define any number of such internal helpers. The next step is to export them to JavaScript as members of an ES module.

## 2. Module Registration

To expose Rust functions and constants to JavaScript, construct an entry list describing every export and register it as a module using `ScopedContext::register_module`.

Each entry is a `FieldDef`:

- `FieldDef::CALL` exports a function.
- `FieldDef::INT32` (and other variants) export constants.

Here is a minimal entry table for the `os` module:

```rust
use zuqe::{FieldDef, JS_PROP_CONFIGURABLE, ScopedContext};

const MOD_OS_ENTRIES: &[FieldDef] = &[
    FieldDef::CALL("now", 0, js_os_now, 0),
    FieldDef::CALL("isatty", 1, js_os_isatty, 0),
    FieldDef::INT32("SIGINT", libc::SIGINT, JS_PROP_CONFIGURABLE),
];

pub fn init_mod_os(ctx: &mut ScopedContext) -> Result<(), i32> {
    // The module name "os" must match the name used in JavaScript imports.
    ctx.register_module("os", MOD_OS_ENTRIES)?;
    Ok(())
}
```

Field definition parameters:

- **Function name**: The name exposed to JavaScript (e.g. `"now"`).
- **Argument count**: Minimum expected arguments for the function.
- **Callback**: The Rust function implementation (e.g. `js_os_now`).
- **Magic value**: Optional extra tag for overloading (use `0` for normal functions).

Once this initialization function has been called on a context, the `os` module becomes available to JavaScript code.

## 3. Garbage Collection (Optional)

If your module has objects that need garbage collector to manage memory cycle, you may provide a custom GC trace function so that those objects can be properly traced and collected by zuqe's gc.

A module‑specific trace function typically walks your data structures and calls `trace` on any embedded JavaScript values:

```rust
use gc_lite::GcTraceCtx;

pub fn mod_os_gc_trace(tracer: &mut GcTraceCtx) {
    // Trace all JSValue fields that are kept alive by the os module.
    // For example:
    // some_js_value.trace(tracer);
}
```

You can then register this trace function on the runtime (see the GC documentation for details). This ensures that any JavaScript objects owned indirectly by your module participate correctly in Zuqe’s garbage collection.

## 4. Usage in JavaScript

Once registered, you can use the module and its exports in JavaScript:

```javascript
// Import the module
import * as os from "os";

// Call a function from the module
const time = os.now();

// Use a constant from the module
os.isatty(0);
```

This tutorial concludes the series on using the `zuqe` crate. You now have the knowledge to create basic scripts, define complex classes, and build native modules that integrate closely with the runtime and its garbage collector.
