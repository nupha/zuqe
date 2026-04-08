# Native Classes

This tutorial explains how to define native Rust classes within the `zuqe` crate. We'll define a class that wraps a Rust file descriptor, exposing its functionality to JavaScript. The examples are based on the `File` class used in the `zjs` example.

## 1. Define a class

Define a class using `ClassDef::new` and provide three handlers:

- Class initialization – to set up the class prototype (and optionally the constructor) for a context.
- Class instance finalization – called when the instance is disposed, usually used to clean up Rust-side data associated with the instance.
- Class unregister – called when the `Runtime` is being destroyed, used to clean up runtime-level resources associated with the class (for example, backends that require global init/free pairs).

```rust
use std::{cell::Cell, ffi::c_void};
use zuqe::{ClassDef, ClassID, FieldDef, Object, Runtime, RuntimeImpl, ScopedContext};

#[derive(Debug)]
enum FileDesc {
    Stdin,
    Stdout,
    Stderr,
    File(std::fs::File),
}

pub struct JSFile;

impl JSFile {
    fn handle_init(ctx: &mut ScopedContext, _: ClassID) -> Result<Option<Object>, i32> {
        let mut proto = ctx.create_base_object()?;
        proto.install_fields(ctx, CLASS_FILE_PROTO)?;
        Ok(Some(proto))
    }

    /// this handler will be called when a class instance is disposed.
    fn handle_dispose(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
        if !opaque.is_null() {
            let p = unsafe { Box::from_raw(opaque.cast::<FileDesc>()) };
            drop(p);
        }
    }

    thread_local! {
        /// To store class's registration id
        static CLASS_ID: Cell<ClassID> = const { Cell::new(0) };
    }

    /// register class to runtime
    pub fn register(mut rt: Runtime) -> Result<ClassID, i32> {
        let class_id = rt
            .register_class(
                ClassDef::new("File")
                    .on_init(Self::handle_init)
                    .on_dispose(Self::handle_dispose),
            )
            .expect("register class File failed");

        JSFile::CLASS_ID.with(|s| s.set(class_id));
        Ok(class_id)
    }

    /// get class registration id
    #[inline(always)]
    pub fn class_id() -> ClassID {
        JSFile::CLASS_ID.with(|x| x.get())
    }
}
```

_Note: Besides per-instance finalization, some native types manage resources at the `Runtime` level that must be initialized once and released when the runtime is destroyed. For these cases, you can provide a class unregister callback on the `ClassDef` using `ClassDef::on_unregister(...)`. When a `Runtime` is dropped, it will invoke the unregister callback of each registered class in turn, allowing you to perform global cleanup. A typical use-case is integrating libraries such as `llama.cpp`, where you call `llama_backend_init` when registering the class, and then use the class unregister callback to call `llama_backend_free` when the runtime is torn down. Any facility that needs a matched global init/free sequence can plug into this mechanism via the unregister callback._

`FileDesc` showed in above example is the Rust-side data type that wraps a file descriptor or standard stream. Each `File` class instance carries a raw pointer to a `FileDesc` value in its opaque payload. When creating an instance, the `FileDesc` is boxed and converted into a raw pointer using `Box::into_raw`, temporarily “forgetting” Rust’s ownership so that the GC-managed JS object can hold it. When the JS object is disposed, the `handle_dispose` callback reclaims ownership with `Box::from_raw` and drops the value, ensuring that the underlying Rust resources are eventually released.

## 2. Define class methods and properties

Bind Rust functions to JavaScript class methods using `FieldDef::CALL`, and define properties using helpers such as `FieldDef::INT32`. For example, the following `js_file_puts` function defines the implementation of the JS `File.puts` method, and the prototype also exposes an integer property.

```rust
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

fn js_file_get_desc_mut<'a>(
    ctx: &mut ScopedContext,
    obj: JSValue,
) -> Result<&'a mut FileDesc, i32> {
    let opaque = obj
        .retrive_object_payload(ctx, JSFile::class_id())?
        .other()
        .unwrap()
        .opaque();

    if !opaque.is_null() {
        Ok(unsafe { &mut *opaque.cast::<FileDesc>() })
    } else {
        ctx.throw_type_error("file was closed");
        Err(-1)
    }
}

fn js_file_puts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let fh = if magic == 1 {
        js_file_get_desc_mut(ctx, cc.this())?
    } else {
        &mut FileDesc::Stdout
    };

    for arg in cc.arguments() {
        let s = arg.to_string(ctx)?;
        let s = s.content().encode_utf8(false);

        match fh {
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

    Ok(JSValue::UNDEFINED)
}

const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::CALL("puts", 1, js_file_puts, 1),
    FieldDef::INT32("DEFAULT_BUFFER_SIZE", 8192, JS_PROP_C_W_E),
];
```

## 3. Expose the Class to JavaScript Context

Before a native class can be used inside a specific JavaScript context, it must be explicitly enabled for that context. Class registration happens at the `Runtime` level, but each `Context` chooses which classes it actually uses. After registering a class, you must call `ctx.use_class(class_id)` or `ctx.use_classes(&[class_id, ...])` so that the context initializes the class’s prototype and constructor; until then, the class remains unavailable in that context.

This design allows multiple contexts to share the same registered classes while selectively enabling only the ones they need. One context can use a class such as `File`, while another context simply never enables it and therefore cannot access it.

Once a native class has been registered in the runtime and enabled for a given context, you typically want to make its constructor visible from JavaScript. For example, you may want a global `File` constructor or a named export from a native module.

There are two common ways to do this:

- Obtain the constructor object with `Context::class_constructor(ClassID)` and attach it to any JavaScript object using `define_property_value` (or its `*_str` variants).
- Declare a `FieldDef::CLASS("File", prop_flags)` entry inside another object or module definition so that the constructor is resolved and exported automatically.

`Context::class_constructor` returns an `Option<Object>` because a class may not yet have a constructor in a given context. In real code you should either ensure that the constructor has been created before calling this API, or gracefully handle the `None` case. To create the constructor for a native class and register it in the context, call `ctx.init_class_constructor`, typically inside the class’s `handle_init` callback (after creating the prototype) or immediately before the first place where you need the constructor. When you pass a non‑empty `entries` slice to `init_class_constructor`, those fields are installed on the constructor function object itself, which is how you define static methods for your native class.

Let's talk above options in detail:

### 3.1 Manually attach the constructor

If you already know the `ClassID` for your native class (for example via `JSFile::class_id()`), you can query the constructor from the context and attach it to an object. A typical use‑case is to place the constructor on the global object so that JavaScript code can call `new File(...)`:

```rust
use zuqe::{JS_PROP_C_W_E, ScopedContext};

fn install_file_global(ctx: &mut ScopedContext) -> Result<(), i32> {
    let ctor = ctx.class_constructor(JSFile::class_id()).unwrap();

    let mut global = ctx.global_obj();
    global.define_property_value_str(ctx, "File", ctor.into(), JS_PROP_C_W_E)?;
    Ok(())
}
```

### 3.2 Export via `FieldDef::CLASS`

Another convenient way to expose a native class is to declare it in a `FieldDef` array and let the runtime resolve the constructor by name. This is especially useful for native modules or namespace objects that are initialized from a list of `FieldDef` entries.

`FieldDef::CLASS` creates a field whose value is automatically set to the constructor of the class with the given name (looked up in the runtime’s class registry):

```rust
use zuqe::{FieldDef, JS_PROP_C_W_E, ScopedContext};

static FILE_MODULE_ENTRIES: &[FieldDef] = &[
    FieldDef::CLASS("File", JS_PROP_C_W_E),
];

fn create_file_module(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("file", FILE_MODULE_ENTRIES)?;
    Ok(())
}
```

When the module is initialized, each `FieldDef::CLASS` entry:

- Looks up the registered class by its name (for example `"File"`).
- Retrieves the associated constructor object for that class.
- Exports the constructor under the same name as a property of the module or object.

This pattern lets you expose native classes declaratively, alongside functions and constant values, without manually calling `class_constructor` or `define_property_value` for each one.

## 4. Creating Instances

Use `create_object_user_class` to create instances and attach a Rust-side payload. In the `zjs` example, `JSFile` provides helper constructors for regular files and for standard streams:

```rust
impl JSFile {
    fn new_instance_internal(ctx: &mut ScopedContext, fd: FileDesc) -> Result<Object, i32> {
        let class_id = Self::class_id();
        let opaque = Box::into_raw(Box::new(fd)).cast::<c_void>();
        ctx.create_object_user_class(class_id, opaque).map_err(|e| {
            let p = e.other().unwrap();
            let _ = unsafe { Box::from_raw(p.opaque().cast::<FileDesc>()) };
            -1
        })
    }

    pub fn new_instance(ctx: &mut ScopedContext, fh: std::fs::File) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::File(fh))
    }

    pub fn new_instance_stdin(ctx: &mut ScopedContext) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdin)
    }

    pub fn new_instance_stdout(ctx: &mut ScopedContext) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdout)
    }

    pub fn new_instance_stderr(ctx: &mut ScopedContext) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stderr)
    }
}
```

These helpers are then used by the native `"std"` module to expose pre-built `File` instances:

```rust
const MOD_STD_ENTRIES: &[FieldDef] = &[
    FieldDef::BUILD("in", |cx| Ok(JSValue::Object(JSFile::new_instance_stdin(cx)?)), 0),
    FieldDef::BUILD("out", |cx| Ok(JSValue::Object(JSFile::new_instance_stdout(cx)?)), 0),
    FieldDef::BUILD("err", |cx| Ok(JSValue::Object(JSFile::new_instance_stderr(cx)?)), 0),
];
```

## 5. Using the Class from JavaScript

Once registered and enabled in a context, the class can be used from JavaScript:

```javascript
const file = new File("/tmp/example.txt", "w");
file.puts("Hello from zuqe\n");
file.flush();
file.close();
```

This demonstrates how native Rust classes integrate with the Zuqe JavaScript environment, allowing you to expose powerful Rust functionality to JavaScript code.
