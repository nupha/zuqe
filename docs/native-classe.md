# Native Classes

This tutorial explains how to define native Rust classes within the `zuqe` crate. We'll define a class that wraps a Rust file descriptor, exposing its functionality to JavaScript. The examples are based on the `File` class used in the `zjs` example.

## 1. Define a class

Define a class using `ClassDef::new` (or `ClassDef::new_with_init_func` when you need a custom initializer), and optionally attach lifecycle callbacks:

- Class instance finalization – called when the instance is disposed, usually used to clean up Rust-side data associated with the instance.
- Class unregister – called when the `Runtime` is being destroyed, used to clean up runtime-level resources associated with the class (for example, backends that require global init/free pairs).

In more detail:

- Using the regular `ClassDef::new(name, proto_entries, ctor)` API:
  - `proto_entries` is the definition of instance-level properties and methods for this class. When a `Context` first “uses” this class (via `ctx.use_class(class_id)`), the engine will internally call `ctx.init_class_proto(class_id, proto_entries)` to create a base Object using as prototype and install these entries.
  - The `ctor` argument describes how to create the constructor function for this class. If `ctor` is `Some(ClassCtorDef::new(...))`, then when the class is first used in the context, the engine will call `init_class_ctor_internal` to create the constructor, bind its prototype, and, if the `ClassCtorDef` has `.global()` set, automatically export that constructor into the global namespace.
  - If `ctor` is `None`, the class has no constructor in that context by default; you can still create instances through internal helpers or other APIs, but there is no `new File(...)` constructor function exposed automatically.

- Using `ClassDef::new_with_init_func(name, init_fn)`:
  - Instead of letting the engine build the prototype and constructor from a static `proto_entries` table, you provide a custom initializer function `init_fn: fn(&mut ScopedContext, ClassID) -> Result<(), i32>`.
  - When a `Context` first uses this class, the engine will call your `init_fn`. Inside that function you are responsible for:
    - Creating the prototype object (for example with `ctx.init_class_proto`, or manually using `ctx.create_base_object` + `install_fields`), and storing it into the corresponding class slot.
    - Calling `ctx.init_class_constructor` / `ctx.init_class_constructor_or_func` or `ctx.create_constructor` as needed to build the constructor, and attaching it to the global object or any other namespace where you want it to appear.
  - This approach is useful when you need full control over the initialization flow — for example, when the prototype must inherit from an object oter than the base `Object`, or when the constructor needs a complex set of static fields.

```rust
use std::{cell::Cell, ffi::c_void};

use zuqe::{ClassDef, ClassID, FieldDef, Runtime, RuntimeImpl};

#[derive(Debug)]
enum FileDesc {
    Stdin,
    Stdout,
    Stderr,
    File(std::fs::File),
}

pub struct JSFile;

impl JSFile {
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
                ClassDef::new("File", CLASS_FILE_PROTO, None)
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

## 2. Define methods and properties

Bind Rust functions to JavaScript class methods using `FieldDef::CALL`, and define properties using helpers such as `FieldDef::INT32`. For example, the following `js_file_puts` function defines the implementation of the JS `File.puts` method, and the prototype also exposes an integer property:

```rust
use std::io::Write;
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

Before a native class can be used inside a specific JavaScript context, it must be explicitly enabled for that context. Class registration happens at the `Runtime` level, but each `Context` chooses which classes it actually uses. After registering a class, you must call `ctx.use_class(class_id)` or `ctx.use_classes(&[class_id, ...])` so that the context initializes the class’s prototype (and, if configured, its constructor); until then, the class remains unavailable in that context.

This design allows multiple contexts to share the same registered classes while selectively enabling only the ones they need. One context can use a class such as `File`, while another context simply never enables it and therefore cannot access it.

Once a native class has been registered in the runtime and enabled for a given context, you typically want to make its constructor visible from JavaScript. For example, you may want a global `File` constructor or a named export from a native module.

For classes defined with the regular `ClassDef::new(name, proto_entries, ctor)` API, the runtime will automatically create the constructor (if `ctor` is `Some(...)`) the first time the class is used in a context, and will export it into the global namespace if the `ClassCtorDef` was created with `.global()`. In these common cases you do not need to manually wire the constructor into the global object – the constructor setup is driven entirely by the `ctor` settings you provided when building the `ClassDef`.

The APIs below are mainly useful when:

- you want to re‑export an existing constructor into another namespace (for example, a module object), or
- you defined the class with `ClassDef::new_with_init_func` and are performing manual initialization inside your custom init function.

`Context::class_constructor` returns an `Option<Object>` because a class may not yet have a constructor in a given context (for example if `ctor` was `None`, or if you chose not to create one in a custom initializer). In manual initialization code you typically use `ctx.init_class_constructor` / `ctx.init_class_constructor_or_func` inside your custom init function, after you have created the prototype, to build the constructor and optionally attach it to a global or module object. When you pass a non‑empty `entries` slice to `init_class_constructor`, those fields are installed on the constructor function object itself, which is how you define static methods for your native class.

In the simplest case, you can rely solely on `ClassCtorDef::global()` and skip the patterns below.

Let's look at the re‑export options in more detail:

### 3.1 Manually attach the constructor

If you already know the `ClassID` for your native class (for example via `JSFile::class_id()`), you can query the constructor from the context and attach it to an object. A typical use‑case is to also expose the constructor on the global object (in addition to any automatic export done via `.global()`), or to re‑export it under a different name:

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
