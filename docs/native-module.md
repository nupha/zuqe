# Native Module

This tutorial demonstrates how to create and register native Rust modules in the `zhuque` crate. We'll create a module that provides operating system-related functionality to JavaScript.

## 1. Module Functions

Define Rust functions that can be called from JavaScript. These functions will be the core of your module.

```rust
use zuqe::{CallContext, JSResult, JSValue};

fn js_os_now(_: CallContext) -> JSResult {
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

fn js_os_isatty(cc: CallContext) -> JSResult {
    let fd = cc.arg(0).to_i32(cc.ctx())?;
    Ok(JSValue::Bool(unsafe { libc::isatty(fd) != 0 }))
}
```

## 2. Module Registration

Create a table of module entries using `FieldDef::CALL` for functions and `FieldDef::INT32` for constants. Register this table as a module using `ctx.register_module`.

```rust
use zuqe::{Context, FieldDef, JS_PROP_CONFIGURABLE};

const MOD_OS_ENTRIES: &[FieldDef] = &[
    FieldDef::CALL("isatty", 1, js_os_isatty, 0),
    // ... other function entries ...
    FieldDef::INT32("SIGINT", libc::SIGINT, JS_PROP_CONFIGURABLE),
    // ... other constant entries ...
];

pub fn init_mod_os(mut ctx: Context) -> Result<(), i32> {
    ctx.register_module("os", MOD_OS_ENTRIES)?;
    Ok(())
}
```

## 3. Tick Handler (Optional)

Add a tick handler to the context to perform periodic tasks. This is useful for handling asynchronous events.

```rust
pub fn init_mod_os(mut ctx: Context) -> Result<(), i32> {
    ctx.register_module("os", MOD_OS_ENTRIES)?;
    ctx.add_tick_handler(|cx| -> bool { js_os_poll(cx) });
    Ok(())
}
```

## 4. Garbage Collection (Optional)

If your module uses thread-local storage with JavaScript values, implement a GC tracer to properly manage their lifecycle.

```rust
use gc_lite::{GcTracable, GcTracer};

pub fn mod_os_gc_trace(tracer: &mut GcTracer) {
    // ... trace JavaScript values stored in thread-local storage ...
}
```

## 5. Usage in JavaScript

Once registered, you can use the module and its functions in your JavaScript code.

```javascript
// Import the module
import * as os from "os";

// Call a function from the module
let time = os.now();

// Use a constant from the module
os.isatty(0);
```

This tutorial concludes the series on using the `zhuque` crate. You now have the knowledge to create basic scripts, define complex classes, and build powerful native modules.
