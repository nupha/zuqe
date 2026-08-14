# Native Classes

This tutorial explains how to define native Rust classes within the `zuqe` crate. We'll define a class that wraps a Rust file descriptor, exposing its functionality to JavaScript. The examples are based on the `File` class and `Emitter` class used in the `zjs` example.

## 1. Define a class with `def_native_object!`

The simplest way to define a native class is via the `def_native_object!` macro, which handles class registration, ID storage, payload management, and lifecycle callbacks automatically:

```rust
use zuqe::{FieldDef, ScopedContext, def_native_object};

#[derive(Debug)]
enum FileDesc {
    Stdin,
    Stdout,
    Stderr,
    File(std::fs::File),
}

// The macro generates:
// - A `#[repr(transparent)]` struct `JSFile` wrapping an `Object` (the payload
//   `RefCell<FileDesc>` is stored in the object's opaque slot)
// - Class registration function `JSFile::register(rt)`
// - Class ID accessor `JSFile::class_id()`
// - Payload access via `as_native_object::<JSFile>()` (then `.payload()` / `.take_payload()`)
// - Instance creation via `JSFile::new_instance(ctx, new_target, payload)`
// - Proper dispose/unregister lifecycle callbacks
//
// Arguments: (name, struct_name, payload_type, proto_entries, ctor_option)
def_native_object!("File", JSFile, RefCell<FileDesc>, CLASS_FILE_PROTO, None);

// The prototype entries are defined separately:
const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::FUNC("fileno", 0, js_file_fileno, 0),
    FieldDef::FUNC("puts", 1, js_file_puts, 1),
    FieldDef::FUNC("flush", 0, js_file_flush, 0),
    FieldDef::FUNC("close", 0, js_file_close, 0),
];
```

When using `def_native_object!`, the generated struct implements the `NativeObject` trait, which provides:

- `as_native_object::<T>()` — recover the native object wrapper (`NativeObject`) from a `JSValue`; call `.payload()` to reach the Rust data
- `take_payload()` — extract and take ownership of the Rust payload (e.g., when closing a resource), freeing its heap allocation

The `def_native_object!` macro arguments are:

- **name**: The JavaScript class name (e.g., `"File"`)
- **struct_name**: The Rust struct name to generate (e.g., `JSFile`)
- **payload_type**: The Rust data type stored inside each instance
- **proto_entries**: A `&[FieldDef]` slice defining instance methods and properties
- **ctor_option**: `Some(ClassCtorDef)` to expose a constructor, or `None` to hide it

Optional keyword arguments (all prefixed with `,`):

- `extends: $class_id_fn` — register the class as a subclass of another native class
- `on_class_register: $fn(rt)` — hook called when the class is registered
- `on_class_unregister: $fn(rt)` — hook called when the class is unregistered
- `on_payload_gc: $fn(&PayloadType, &mut GcTraceCtx)` — **required whenever the payload holds GC nodes**; the collector calls it during GC mark so embedded JS values stay reachable (see the GC safety section below).

> **⚠️ GC safety (MUST READ):** A `JSValue`/`Object` stored in a class payload or in a
> `thread_local!` is invisible to Zuqe's GC — if the collector reclaims it while you still use it,
> the engine panics with a use-after-free. **Any payload holding GC nodes must provide
> `on_payload_gc`, and any `thread_local!` holding GC nodes must be traced via
> `rt.gc_set_trace_handler`.** Note that `extends:` subclasses do *not* inherit the parent's GC
> trace and must declare their own `on_payload_gc`. See [GC safety for native code — class payloads
> & thread_local](gc.md) for the full rules, examples, and review checklist.

For classes that need a constructor exposed to JavaScript (e.g., `new Emitter()`), pass a `ClassCtorDef`:

```rust
use std::cell::RefCell;
use zuqe::{CallContext, ClassCtorDef, JSResult, JSValue, ScopedContext};

fn js_emitter_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    JSEmitter::new_instance(
        ctx,
        new_target,
        EmitterData { listeners: RefCell::new(Vec::new()) },
    )
    .map(JSValue::from)
}

def_native_object!(
    "Emitter",
    JSEmitter,
    EmitterData,
    CLASS_EMITTER_PROTO,
    Some(ClassCtorDef::new(js_emitter_constructor, 0, false).expose_to_global())
);
```

`ClassCtorDef::new(callback, length, call_as_func)` configures:

- `callback`: The constructor function implementation
- `length`: The number of expected arguments (for `Function.length`)
- `call_as_func`: Whether the constructor can also be called without `new`
- `.expose_to_global()`: Automatically export the constructor to the global namespace
- `.magic(magic)`: Set the constructor's magic value

`ClassCtorDef` also supports static methods via `.entries(&[...])`.

## 2. Define methods and properties

Bind Rust functions to JavaScript class methods using `FieldDef::FUNC`, and define properties using helpers such as `FieldDef::INT32`. Access the Rust payload via the `as_native_object::<T>()` method on `JSValue`:

```rust
use std::io::Write;
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

// Access the Rust payload from a JSValue
fn get_file_fd<'a>(ctx: &mut ScopedContext, val: &'a JSValue) -> Result<&'a RefCell<FileDesc>, i32> {
    val.as_native_object::<JSFile>()
        .and_then(|o| o.payload())
        .ok_or_else(|| {
            ctx.throw_type_error("not a File instance");
            -1
        })
}

fn js_file_puts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    if magic == 1 {
        let this = cc.this();
        let fd = get_file_fd(ctx, &this)?;
        let mut fdesc = fd.borrow_mut();
        for arg in cc.arguments() {
            let s = arg.to_string(ctx)?;
            let s = s.content().encode_utf8(false);

            match &mut *fdesc {
                FileDesc::File(f) => {
                    f.write_all(s.as_bytes()).map_err(|e| {
                        ctx.throw_internal_error(&e.to_string());
                        -1
                    })?;
                }
                FileDesc::Stdout => {
                    std::io::stdout().write_all(s.as_bytes()).map_err(|e| {
                        ctx.throw_internal_error(&e.to_string());
                        -1
                    })?;
                }
                FileDesc::Stderr => {
                    std::io::stderr().write_all(s.as_bytes()).map_err(|e| {
                        ctx.throw_internal_error(&e.to_string());
                        -1
                    })?;
                }
                FileDesc::Stdin => {}
            }
        }
    } else {
        for arg in cc.arguments() {
            let s = arg.to_string(ctx)?;
            let s = s.content().encode_utf8(false);
            std::io::stdout().write_all(s.as_bytes()).map_err(|e| {
                ctx.throw_internal_error(&e.to_string());
                -1
            })?;
        }
    }

    Ok(JSValue::UNDEFINED)
}

const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::FUNC("fileno", 0, js_file_fileno, 0),
    FieldDef::FUNC("puts", 1, js_file_puts, 1),
    FieldDef::FUNC("flush", 0, js_file_flush, 0),
    FieldDef::FUNC("close", 0, js_file_close, 0),
    FieldDef::INT32("DEFAULT_BUFFER_SIZE", 8192, JS_PROP_C_W_E),
];
```

The `close` method demonstrates how to safely extract and drop the Rust payload:

```rust
fn js_file_close(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let mut jsfile = this.as_native_object::<JSFile>().ok_or_else(|| {
        ctx.throw_type_error("not a File instance");
        -1
    })?;
    let _ = jsfile.take_payload();
    Ok(JSValue::UNDEFINED)
}
```

## 3. Register and Use the Class

After defining the class with `def_native_object!`, register it with the runtime and enable it in a context:

```rust
use zuqe::{Context, ScopedContext, Runtime};

fn main() {
    let mut rt = Runtime::create().unwrap();

    // Register the class — this stores metadata in the runtime.
    let class_id = JSFile::register(&mut rt).unwrap();

    let mut ctx = rt
        .create_context(|cx| {
            // Set up ECMA standard built-ins
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;

            // Enable the class in this context — this initializes its
            // prototype (and constructor, if configured).
            cx.use_class(class_id)?;

            Ok(())
        })
        .unwrap();
}
```

You can also call `cx.use_classes(&[class_id_1, class_id_2])` on the setup context to enable multiple classes at once.

### Export via `FieldDef::CLASS`

Another convenient way to expose a native class constructor is to declare it in a `FieldDef` array using `FieldDef::CLASS`. This is especially useful for native modules:

```rust
use zuqe::{FieldDef, JS_PROP_C_W_E};

// The closure returns the ClassID, which the runtime uses to
// look up and expose the constructor.
static FILE_MODULE_ENTRIES: &[FieldDef] = &[
    FieldDef::CLASS("File", || JSFile::class_id(), JS_PROP_C_W_E),
];
```

## 4. Creating Instances

The `def_native_object!` macro generates a `new_instance` method. Use it to create instances with a Rust payload:

> **⚠️ GC note:** if `PayloadType` itself contains GC nodes (`JSValue`/`Object`/native wrapper),
> the class **must** also declare `on_payload_gc` (see [GC safety for native code — class payloads
> & thread_local](gc.md)), or those nodes can be reclaimed while the instance is alive.

```rust
use std::cell::RefCell;
use zuqe::{Object, JSValue, ScopedContext};

impl JSFile {
    /// Create a File instance from a std::fs::File handle.
    pub fn create_file_instance(ctx: &mut ScopedContext, fh: std::fs::File) -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::File(fh)))
            .map(Into::into)
    }

    /// Create a File instance wrapping stdin.
    pub fn create_stdin_instance(ctx: &mut ScopedContext) -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::Stdin))
            .map(Into::into)
    }
}
```

These helpers are then used by the native `"std"` module to expose pre-built `File` instances via `FieldDef::LAZY_VALUE`:

```rust
const MOD_STD_ENTRIES: &[FieldDef] = &[
    FieldDef::LAZY_VALUE(
        "in",
        |cx| {
            let obj = JSFile::create_stdin_instance(cx)?;
            Ok(JSValue::from(obj))
        },
        JS_PROP_ENUMERABLE,
    ),
    FieldDef::LAZY_VALUE(
        "out",
        |cx| {
            let obj = JSFile::create_stdout_instance(cx)?;
            Ok(JSValue::from(obj))
        },
        JS_PROP_ENUMERABLE,
    ),
];
```

## 5. Using the Class from JavaScript

Once registered and enabled in a context, the class can be used from JavaScript:

```javascript
// The `File` class above is registered without a constructor (`None`), so
// instances are created by native helpers — e.g. the `std` module's `open`
// returns a `File` instance, and `std.in`/`std.out`/`std.err` are pre-built.
const file = std.open("/tmp/example.txt", "w");
file.puts("Hello from zuqe\n");
file.flush();
file.close();

// Classes with a constructor (via `Some(ClassCtorDef)` + `expose_to_global()`)
// can be `new`-ed directly:
const emitter = new Emitter();
emitter.on("event", 1);
```

This demonstrates how native Rust classes integrate with the Zuqe JavaScript environment, allowing you to expose powerful Rust functionality to JavaScript code.
