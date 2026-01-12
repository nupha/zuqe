# Native Classe

This tutorial explains how to define native Rust classes within the `zuqe` crate. We'll define a class that wraps a Rust file descriptor, exposing its functionality to JavaScript.

## 1. Class Definition

Define a class using `ClassDef::new` should provide two handlers for initialization and finalization:

- the initialization handler is where you set up the prototype,
- the finalization handler cleans up any Rust-side data associated with the class.

```rust
use zuqe::{ClassDef, Context, ClassID, RuntimeImpl, c_void};

pub struct JSFile;

impl JSFile {
    fn handle_init(ctx: Context, _: ClassID) -> Result<Option<Object>, i32> {
        // Create a base object to serve as the prototype for instances of this class.
        let mut proto = ctx.create_base_object()?;
        // Install methods on the prototype using FieldDef::CALL.
        proto.install_fields(&ctx, CLASS_FILE_PROTO)?;
        Ok(Some(proto))
    }

    fn handle_fini(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
        // This function is called when an object of this class is garbage collected.
        // You can clean up any Rust-side resources here.
        if !opaque.is_null() {
            unsafe {
                let p = Box::from_raw(opaque.cast::<FileDesc>());
                drop(p);
            }
        }
    }
}
```

## 2. Define method

Bind Rust functions to JavaScript methods using `FieldDef::CALL`. For example, the following `js_file_puts` function defines the implementation of JS `File.puts` method.

_Note that the argument `magic` is an opaque value you defined._

```rust
use zuqe::{CallContext, JSResult, JSValue};

pub(super) fn js_file_puts(cc: CallContext) -> JSResult {
    let magic = cc.magic(); // This value is passed from FieldDef::CALL
    let ctx = cc.context();

    let fh = if magic == 1 {
        js_file_get_desc_mut(ctx, *cc.this())?
    } else {
        &mut FileDesc::Stdout
    };

    for arg in cc.arguments() {
        let s = arg.to_string(&ctx)?;
        let s = s.content().encode_utf8(false);

        // Write the string to the file descriptor.
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

// ... other methods are omitted

// class methods definations
const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::CALL("fileno", 0, js_file_fileno, 0),
    FieldDef::CALL("readAsString", 0, js_file_readAsString, 0),
    FieldDef::CALL("puts", 1, js_file_puts, 1), // The magic value is set to 1
    FieldDef::CALL("flush", 0, js_file_flush, 0),
    FieldDef::CALL("close", 0, js_file_close, 0),
];
```

## 3. Class Registration

Register your class with the runtime to make it available in the zuqe JavaScript environment.

```rust
use zuqe::{ClassID, RuntimeImpl};

thread_local! {
    // register class will return a class_id of the class.
    // we use a thread local static to save this id.
    static CLASS_ID: Cell<ClassID> = const { Cell::new(0) };
}

impl JSFile {
    pub fn register(rt: &mut RuntimeImpl) -> Result<ClassID, i32> {
        let class_id = rt.register_class(
            // class defination
            &ClassDef::new("File")
                .initialize(Self::handle_init)
                .finalize_opaque(Self::handle_fini),
        )?;

        // save the assigned class_id, so we can read it later.
        Self::CLASS_ID.with(|s| s.set(class_id));

        Ok(class_id)
    }

    // a wrapper to get class_id of this class.
    #[inline(always)]
    pub fn class_id() -> ClassID {
        Self::CLASS_ID.with(|x| x.get())
    }
}
```

## 4. Create instance of Class

Create instances of our native class using `ctx.create_object_user_class`. You can also associate a Rust-side data struct (in this example, a `FileDesc` value) with the JavaScript object using raw opaque pointer.

```rust
use zuqe::Context;
use std::fs::File;

impl JSFile {
    fn new_instance_internal(ctx: Context, fd: FileDesc) -> Result<Object, i32> {
        // the class_id of our native class,
        // which is acquired in ::register_class(),
        // you must save it somewhere, so we can get it.
        let class_id = Self::class_id();

        let opaque = Box::into_raw(Box::new(fd)).cast::<c_void>();
        ctx.create_object_user_class(class_id, opaque).map_err(|e| {
            let p = e.other().unwrap();
            let _ = unsafe { Box::from_raw(p.opaque().cast::<FileDesc>()) };
            -1
        })
    }

    pub fn new_instance(ctx: Context, fh: File) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::File(fh))
    }

    // ... other constructors for stdin, stdout, stderr, etc.
}
```

## ⚠️ Important Note

Remember the `JSFile::handle_fini` handler mentioned above? When a class instance is disposed, this function will be triggered with the `opaque` argument holds exactly the raw pointer we attached when creating the object.

This is the place we take raw pointer's ownership back (using `Box::from_raw` in this example), and ensure it's dropped. This is a crucial step to avoid memory leaks, ensuring that Rust resources attached to each JS object are properly cleaned up.

```rust
fn handle_fini(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
    // This function is called when an object is disposed.
    // You can clean up any Rust-side resources here.
    if !opaque.is_null() {
        unsafe {
            let p = Box::from_raw(opaque.cast::<FileDesc>());
            drop(p);
        }
    }
}
```

## 5. The whole code

Below is a complete implementation example that integrates all components together:

```rust
use zuqe::{ClassDef, Context, ClassID, RuntimeImpl, c_void, CallContext, JSResult, JSValue, FieldDef, Object};
use std::cell::Cell;
use std::io::{self, Write};
use std::fs::File;
use std::os::unix::io::AsRawFd;

// File descriptor type definition
enum FileDesc {
    File(File),
    Stdout,
    Stderr,
    Stdin,
}

// JavaScript File class
pub struct JSFile;

impl JSFile {
    fn handle_init(ctx: Context, _: ClassID) -> Result<Option<Object>, i32> {
        let mut proto = ctx.create_base_object()?;
        proto.install_fields(&ctx, CLASS_FILE_PROTO)?;
        Ok(Some(proto))
    }

    fn handle_fini(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
        if !opaque.is_null() {
            unsafe {
                let p = Box::from_raw(opaque.cast::<FileDesc>());
                drop(p);
            }
        }
    }

    fn new_instance_internal(ctx: Context, fd: FileDesc) -> Result<Object, i32> {
        let class_id = Self::class_id();
        let opaque = Box::into_raw(Box::new(fd)).cast::<c_void>();
        ctx.create_object_user_class(class_id, opaque).map_err(|e| {
            let p = e.other().unwrap();
            let _ = unsafe { Box::from_raw(p.opaque().cast::<FileDesc>()) };
            -1
        })
    }

    pub fn new_instance(ctx: Context, fh: File) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::File(fh))
    }

    pub fn new_stdout(ctx: Context) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdout)
    }

    pub fn new_stderr(ctx: Context) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stderr)
    }

    pub fn new_stdin(ctx: Context) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdin)
    }
}

// Method implementations
pub(super) fn js_file_puts(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let ctx = cc.context();

    let fh = if magic == 1 {
        js_file_get_desc_mut(ctx, *cc.this())?
    } else {
        &mut FileDesc::Stdout
    };

    for arg in cc.arguments() {
        let s = arg.to_string(&ctx)?;
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

pub(super) fn js_file_fileno(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let fh = js_file_get_desc(ctx, *cc.this())?;

    let fd = match fh {
        FileDesc::File(f) => f.as_raw_fd(),
        FileDesc::Stdout => 1,
        FileDesc::Stderr => 2,
        FileDesc::Stdin => 0,
    };

    Ok(JSValue::from(fd as i32))
}

pub(super) fn js_file_flush(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let fh = js_file_get_desc_mut(ctx, *cc.this())?;

    match fh {
        FileDesc::File(f) => {
            f.flush().map_err(|e| {
                ctx.throw_internal_error(&e.to_string());
                -1
            })?;
        }
        FileDesc::Stdout => {
            std::io::stdout().flush().map_err(|e| {
                ctx.throw_internal_error(&e.to_string());
                -1
            })?;
        }
        FileDesc::Stderr => {
            std::io::stderr().flush().map_err(|e| {
                ctx.throw_internal_error(&e.to_string());
                -1
            })?;
        }
        FileDesc::Stdin => {}
    }

    Ok(JSValue::UNDEFINED)
}

pub(super) fn js_file_close(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let this = cc.this();

    // Get file descriptor and close
    let fh = js_file_get_desc(ctx, this)?;
    match fh {
        FileDesc::File(_) => {
            // File will be automatically closed in handle_fini
            ctx.set_private_data(this, std::ptr::null_mut());
        }
        _ => {}
    }

    Ok(JSValue::UNDEFINED)
}

// Helper functions
fn js_file_get_desc(ctx: &Context, obj: Object) -> Result<&FileDesc, i32> {
    let opaque = ctx.get_private_data(obj)
        .ok_or_else(|| ctx.throw_internal_error("Invalid file object"))?;
    unsafe { Ok(&*opaque.cast::<FileDesc>()) }
}

fn js_file_get_desc_mut(ctx: &Context, obj: Object) -> Result<&mut FileDesc, i32> {
    let opaque = ctx.get_private_data(obj)
        .ok_or_else(|| ctx.throw_internal_error("Invalid file object"))?;
    unsafe { Ok(&mut *opaque.cast::<FileDesc>()) }
}

// Class method definitions
const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::CALL("fileno", 0, js_file_fileno, 0),
    FieldDef::CALL("puts", 1, js_file_puts, 1),
    FieldDef::CALL("flush", 0, js_file_flush, 0),
    FieldDef::CALL("close", 0, js_file_close, 0),
];

// Class registration
thread_local! {
    static CLASS_ID: Cell<ClassID> = const { Cell::new(0) };
}

impl JSFile {
    pub fn register(rt: &mut RuntimeImpl) -> Result<ClassID, i32> {
        let class_id = rt.register_class(
            &ClassDef::new("File")
                .initialize(Self::handle_init)
                .finalize_opaque(Self::handle_fini),
        )?;

        Self::CLASS_ID.with(|s| s.set(class_id));
        Ok(class_id)
    }

    #[inline(always)]
    pub fn class_id() -> ClassID {
        Self::CLASS_ID.with(|x| x.get())
    }
}
```

## 6. Integrate with zuqe

Now integrate our native class with the zuqe environment.

```rust
use zuqe::{Runtime, Context, EvalType, EvalFlags};

// Initialize runtime and context
let mut rt = Runtime::create().unwrap();
let mut ctx = rt.create_context_ecma().unwrap();

// Register the File class
let class_id_file = JSFile::register(&mut rt).unwrap();

// Enable the class in the context
ctx.use_class(class_id_file).unwrap();

let script = r#"
    const file = new File('/tmp/test.txt', 'w');
    file.puts('Hello from zuqe');
    file.flush();
    file.close();
"#;

// A simple run loop to execute script only and exit.
// for comprehensive run loop, see examples.
ctx.run_wait(
    script.as_bytes(),
    "example.js",
    EvalType::Global,
    EvalFlags::empty(),
)?;
```

This demonstrates how seamlessly native Rust classes integrate with the zhuque JavaScript environment, allowing you to expose powerful Rust functionality to JavaScript users.

## 7. More JavaScript Examples

Below is JavaScript script example showing how to use our defined `File` class in zuqe:

```javascript
// Create a File instance
const file = new File("/tmp/example.txt", "w");

// Write data
file.puts("Hello, zhuque world!\n");
file.puts("This is a test of native Rust classes.\n");
file.puts("File descriptor: " + file.fileno());

// Flush buffer
file.flush();

// Close file
file.close();

// Use stdout
const stdout = File.stdout();
stdout.puts("This goes to stdout");
stdout.flush();

// Use stderr
const stderr = File.stderr();
stderr.puts("This goes to stderr");
stderr.flush();

// Use stdin
const stdin = File.stdin();
stdin.puts("Reading from stdin...");

// Read file
const readFile = new File("/tmp/example.txt", "r");
const content = readFile.readAsString();
console.log("File content:", content);
readFile.close();

// Error handling example
try {
  const invalidFile = new File("/nonexistent/path/file.txt", "r");
  const content = invalidFile.readAsString();
} catch (error) {
  console.error("Failed to read file:", error.message);
}

// File operations completed
console.log("File operations completed successfully!");
```
