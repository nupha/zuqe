# Native Classe

This tutorial explains how to define native Rust classes within the `zuqe` crate. We'll define a class that wraps a Rust file descriptor, exposing its functionality to JavaScript. The examples are based on the `File` class used in the `zjs` example.

## 1. Class Definition

Define a class using `ClassDef::new` and provide two handlers:

- an initialization handler to set up the prototype,
- a finalization handler to clean up any Rust-side data associated with the class.

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

    fn handle_fini(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
        if !opaque.is_null() {
            let p = unsafe { Box::from_raw(opaque.cast::<FileDesc>()) };
            drop(p);
        }
    }

    thread_local! {
        static CLASS_ID: Cell<ClassID> = const { Cell::new(0) }; // To store our class's registration id
    }

    pub fn register(mut rt: Runtime) -> Result<ClassID, i32> {
        let class_id = rt
            .register_class(
                &ClassDef::new("File")
                    .initialize(Self::handle_init)
                    .finalize_opaque(Self::handle_fini),
            )
            .expect("register class File failed");

        JSFile::CLASS_ID.with(|s| s.set(class_id));
        Ok(class_id)
    }

    #[inline(always)]
    pub fn class_id() -> ClassID {
        JSFile::CLASS_ID.with(|x| x.get())
    }
}
```

## 2. Methods

Bind Rust functions to JavaScript class methods using `FieldDef::CALL`. For example, the following `js_file_puts` function defines the implementation of the JS `File.puts` method.

```rust
use zuqe::{CallContext, JSResult, JSValue};

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
    // other methods omitted
];
```

## 3. Creating Instances

Use `create_object_user_class` to create instances and attach a Rust-side payload.

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

## 4. Important Note

The `handle_fini` handler is called when a class instance is collected. It receives the `opaque` pointer attached when the object was created. This is where you take back ownership (with `Box::from_raw`) and ensure Rust resources are properly dropped to avoid memory leaks.

```rust
fn handle_fini(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
    if !opaque.is_null() {
        unsafe {
            let p = Box::from_raw(opaque.cast::<FileDesc>());
            drop(p);
        }
    }
}
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
