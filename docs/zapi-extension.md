# Writing a Zapi Extension

A zapi extension is a dynamic library (`dylib` on macOS, `so` on Linux, `dll`
on Windows) that the Zuqe engine loads and treats as a JavaScript module:

```js
import * as my from "./libmymod.dylib";
my.add(2, 3); // 5
```

Loading a native module is the **host's** job, not JavaScript's. Two
requirements: the runtime installs a module loader that recognizes
`.dylib`/`.so`/`.dll`, and it's built with the `zapi` feature. `zjs`
(`examples/zjs`) does both — its `js_module_loader` checks the file extension
and calls `ScopedContext::load_zapi_extension`, which `dlopen`s the library and
resolves `zq_module_register` (`src/zapi/host.rs`). No loader (registered via
`Runtime::set_module_loader`), no `import "./libmymod.dylib"`.

This guide builds an extension from scratch, one piece at a time: project
setup → your first simple module → objects and arrays → module values → native
classes → async work. The repository ships runnable extensions you can read
alongside it:

| Example               | What it shows                                                                  |
| --------------------- | ------------------------------------------------------------------------------ |
| `examples/zapi_basic` | Every JS value type crossing the boundary, a GC-traced class, a value export.  |
| `examples/zapi_event` | A full class: methods, `magic` dispatch, static accessors, thread-local state. |
| `examples/zapi_raw`   | The raw ABI, if you ever need to drop below the safe layer.                    |

You build against two crates:

- `zuqe_zapi` — the safe Rust API (no raw pointers, no `unsafe extern "C"`).
- `zuqe_zapi_macros` — the `#[zq_*]` attributes that generate the C ABI glue.

There is also a raw layer (`zuqe_zapi::ffi`) hidden underneath. Don't start
there. The safe layer covers everything in this guide, and `examples/zapi_raw`
shows the raw ABI only if you ever need an escape hatch.

---

## 1. Project setup

Start with a normal Cargo library. The only unusual part is the crate type.

`Cargo.toml`:

```toml
[package]
name = "mymod"
version = "0.1.0"
edition = "2024"

[lib]
name = "mymod"                 # → libmymod.dylib / libmymod.so
crate-type = ["cdylib"]        # a dynamic library, not a Rust rlib

[dependencies]
zuqe_zapi = "0.3.0"
zuqe_zapi_macros = "0.3.0"
```

Two things matter here:

- `crate-type = ["cdylib"]` produces the loadable library. Forget it and the
  host has nothing to `dlopen`.
- `[lib] name` becomes the file name the script imports. Keep it short and
  stable — it's part of your public API.

---

## 2. Your first simple module

Create `src/lib.rs` and export a few functions. Start with one:

```rust
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_function, zq_module};

// `#[zq_function]` makes `add` callable from JS. The JS name defaults to the
// Rust name; rename it with `#[zq_function(name = "...")]`.
#[zq_function]
fn add<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    // cc.arg(i) is the i-th argument (undefined when omitted).
    //
    // to_f64 / to_i32 are ToNumber coercions, to_bool is ToBoolean; they return
    // Result<_, ZqError> and only fail when the coercion itself throws (a
    // non-numeric value coerces to NaN / 0 like JS ToNumber instead of
    // failing). string_read_utf8 is a *strict* string read: a non-string is an
    // Err and leaves a pending TypeError. The `?` propagates the error to JS as
    // the thrown exception.
    let a = cc.arg(0).to_f64(ctx)?;
    let b = cc.arg(1).to_f64(ctx)?;
    Ok(ZqValue::number(a + b)) // build the return value
}
```

A string in / string out, and two numbers in / object out:

```rust
#[zq_function]
fn greet<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    // coerce_string is ToString (`String(x)`); use string_read_utf8 when the
    // value is known to be a string, or try_string_read_utf8 to probe.
    let name = cc.arg(0).coerce_string(ctx)?; // string → Rust String
    ZqValue::new_string(ctx, &format!("hello, {name}")) // Result<ZqValue> — returned directly
}
```

Note: the `ZqValue` constructors (`new_string`, `new_object`, `new_array`,
...) return `Result<ZqValue, ZqError>`. When one is the last expression, return
it directly — don't add `?` (which would unwrap it) or `Ok(...)` (which would
double-wrap). When you need the value mid-function, use `?` to unwrap:

```rust
let obj = ZqValue::new_object(ctx)?;   // unwrap: now `obj` is ZqValue, not Result
obj.set_property_str(ctx, "key", val)?;
Ok(obj)                                // wrap back for the return type
```

Another example — building an object:

```rust
#[zq_function(name = "makePoint")]             // JS name is makePoint
fn make_point<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let x = cc.arg(0).to_f64(ctx)?;
    let y = cc.arg(1).to_f64(ctx)?;

    let p = ZqValue::new_object(ctx)?;         // a fresh `{}`
    p.set_property_str(ctx, "x", x)?;          // p.x = x
    p.set_property_str(ctx, "y", y)?;          // p.y = y
    Ok(p)
}
```

Register them:

```rust
// `exports` lists the Rust identifiers to export — nothing is auto-discovered.
// For functions, the JS name is the Rust name (or `#[zq_function(name = "...")]`).
// For classes, the JS name is the `name = "..."` in `#[zq_class(name = "...")]`.
#[zq_module(name = "mymod", exports = [add, greet, make_point])]
fn module_init() {}
```

The pieces you'll reach for most:

```rust
// ── read an argument ─────────────────────────────────────────────────────
cc.arg(0).to_f64(ctx)?            // number → f64
cc.arg(1).to_i32(ctx)?            // number → i32
cc.arg(2).to_bool(ctx)?           // truthy → bool
cc.arg(3).coerce_string(ctx)?     // ToString → String (string_read_utf8 is strict)
cc.arg_f64(ctx, 0)?               // same as cc.arg(0).to_f64(ctx); also arg_i32/arg_bool/arg_string
cc.arg(0).kind()                  // ZqValueType::{Undefined,Null,Bool,Number,String,Symbol,Function,Object,BigInt}
cc.arg(0).is_undefined()          // argument was omitted
cc.len()                          // argument count
cc.this()                         // the `this` value

// ── build a return value ─────────────────────────────────────────────────
ZqValue::int32(1)                 // integer
ZqValue::number(1.5)              // f64
ZqValue::bool(true)
ZqValue::null()
ZqValue::undefined()
ZqValue::new_string(ctx, "hi")?   // string
ZqValue::new_object(ctx)?         // plain object
ZqValue::new_array(ctx, &[])?     // array

// ── throw a JS error ─────────────────────────────────────────────────────
// ctx.throw_* returns ZqError (not Result). Wrap it in Err() to propagate.
return Err(ctx.throw_type("`path` is required"));    // TypeError
return Err(ctx.throw_range("expected 2 arguments")); // RangeError

// A common pattern: payload() returns Option; throw TypeError on None:
let p = JsCounter::payload(ctx, cc.this())
    .ok_or_else(|| ctx.throw_type("not a Counter"))?;

// set_property_str takes anything Into<ZqValue> (i32 / f64 / bool / String / ZqValue):
p.set_property_str(ctx, "count", 7i32)?;
```

### Build and load

```sh
cargo build --release
```

The artifact lands in `target/release/libmymod.dylib` (or `.so`). Copy it next
to your script and import it by path:

```js
import * as my from "./libmymod.dylib";
console.log(my.add(2, 3)); // 5
console.log(my.greet("zapi")); // hello, zapi
console.log(my.makePoint(3, 4).x); // 3
```

The host build must have the `zapi` feature enabled. `zjs` is the usual test
runner:

```sh
target/debug/examples/zjs --module test.mjs
```

That's a complete, loadable extension. Everything below is elaboration.

---

## 3. Objects and arrays

Here's a function that reads an options object and returns a new one:

```rust
#[zq_function]
fn describe<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let opts = cc.arg(0);

    // Read a property. get_property_str returns undefined when absent.
    let name = opts.get_property_str(ctx, "name")?;
    let name = if name.is_undefined() { "anon".to_string() } else { name.coerce_string(ctx)? };

    let out = ZqValue::new_object(ctx)?;
    out.set_property_str(ctx, "name", ZqValue::new_string(ctx, &name)?)?;
    out.set_property_str(ctx, "count", cc.len() as i32)?;
    Ok(out)
}
```

The property API mirrors JS:

```rust
obj.set_property_str(ctx, "key", value)?;  // obj.key = value
obj.get_property_str(ctx, "key")?;         // obj.key  (undefined if missing)
obj.has_property_str(ctx, "key")?;         // "key" in obj
obj.del_property_str(ctx, "key")?;         // delete obj.key
obj.set_property(ctx, 0i32, value)?;       // obj[0] = value (any key type)
obj.get_property(ctx, any_key)?;           // obj[any_key]
```

(The shorter aliases `set_str` / `get_str` / `has_str` / `delete_str` / `set` /
`get` still exist but are deprecated.)

Arrays:

```rust
let arr = ZqValue::new_array(ctx, &[])?;
arr.push(ctx, ZqValue::int32(1))?;
let len = arr.length(ctx)?;            // arr.length
let first = arr.get_property(ctx, 0i32)?;  // arr[0]
```

`new_array` takes a slice of `ZqValue`, so elements can be passed directly;
alternatively build the array empty and push:

```rust
// Option A: upfront
ZqValue::new_array(ctx, &[
    ZqValue::int32(1),
    ZqValue::new_string(ctx, "two")?,
])?;

// Option B: push after
let arr = ZqValue::new_array(ctx, &[])?;
arr.push(ctx, ZqValue::int32(1))?;
arr.push(ctx, ZqValue::new_string(ctx, "two")?)?;
```

### Calling JS from the extension

Grab built-ins from the global object:

```rust
let json = ctx.get_global_str("JSON")?;
let stringify = json.get_property_str(ctx, "stringify")?;
let text = ctx.call_function(stringify, json, &[obj])?;
```

Call a function value with `ctx.call_function(callee, this, &args)` or the
shorthand method `callee.call(ctx, this, &args)`. Invoke a constructor with
`ctx.call_constructor(target, &args)`:

```rust
let date = ctx.get_global_str("Date")?;
let now = ctx.call_constructor(date, &[ZqValue::int32(0)])?;

let u8 = ctx.get_global_str("Uint8Array")?;
let buf = ctx.call_constructor(u8, &[ZqValue::int32(16)])?;
```

Use this whenever you need a JS built-in (`BigInt`, `Array.isArray`, `JSON`,
`Uint8Array`, ...) from native code.

---

## 4. Module-level values

Not everything is callable. Use `#[zq_export]` for constants and config
objects. The body takes only the context and runs lazily when the module is
evaluated:

```rust
use zuqe_zapi_macros::zq_export;

/// `VERSION` → `"0.1.0"`.
#[zq_export(name = "VERSION")]
fn version<'a>(ctx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    ZqValue::new_string(ctx, "0.1.0")
}
```

Add it to the export list just like a function:

```rust
#[zq_module(name = "mymod", exports = [add, describe, version])]
fn module_init() {}
```

Because the body has a real context, a value can be any JS value — string,
number, object, array, even a function you built up at load time.

---

## 5. How to define & export native classes

One class, end to end. It's shown in a single file here; split classes into
their own files once your module grows. The comments explain each part; the key
points are at the bottom.

```rust
use zuqe_zapi::*;
use zuqe_zapi_macros::{
    zq_class, zq_class_impl, zq_method, zq_module, zq_static_method, ZqGcTrace,
};

// ── The class = payload struct + a JS class name ─────────────────────────
// A native class is useful when each instance carries its own data. The
// payload is that data: your Rust struct, one per JS instance, holding the
// instance's state (a counter's value, a connection, a buffer, ...). `new
// Counter(l)` boxes a CounterPayload; the engine drops the box when the JS
// object is collected. Methods read and update it through `this` via
// payload() / payload_mut().
//
// A field holding a JS value must be a ZqRawValue and must be traced, or the
// GC will free it while you still hold it. `#[derive(ZqGcTrace)]` traces every
// field; plain i32/String fields are no-ops.
#[derive(ZqGcTrace)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,               // a JS value (here: a string) — traced
}

// `name` is the JS class name (defaults to the struct name); `payload` points at
// that struct. The macro gives you three helpers:
//   JsCounter::new_instance(ctx, payload)
//   JsCounter::payload(ctx, val)      -> Option<&CounterPayload>
//   JsCounter::payload_mut(ctx, val)  -> Option<&mut CounterPayload>
#[zq_class(name = "Counter", payload = CounterPayload)]
struct JsCounter;

// ── Define the class: constructor + methods + accessors ──────────────────
#[zq_class_impl]
impl JsCounter {
    // Recognized by name. `new Counter(label)`.
    fn constructor<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let label = cc.arg(0).raw();          // ZqValue -> ZqRawValue (store it)
        JsCounter::new_instance(ctx, CounterPayload { count: 0, label })
    }

    // #[zq_method] -> prototype method (`c.increment()`).
    // `payload_mut` returns None when `this` isn't a Counter: guard every method.
    #[zq_method]
    fn increment<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let this = cc.this();
        let p = JsCounter::payload_mut(ctx, this)
            .ok_or_else(|| ctx.throw_type("not a Counter"))?;
        p.count += 1;
        Ok(ZqValue::int32(p.count))
    }

    // Return `this` for chaining (`c.reset().increment()`).
    #[zq_method]
    fn reset<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let this = cc.this();
        let p = JsCounter::payload_mut(ctx, this).ok_or_else(|| ctx.throw_type("not a Counter"))?;
        p.count = 0;
        Ok(this)
    }

    // `get` -> property (`c.count`). Add `set` to make it writable.
    #[zq_method(name = "count", get)]
    fn count<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JsCounter::payload(ctx, cc.this()).ok_or_else(|| ctx.throw_type("not a Counter"))?;
        Ok(ZqValue::int32(p.count))
    }

    // Return the stored handle to JS: ZqRawValue -> ZqValue.
    #[zq_method]
    fn label<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JsCounter::payload(ctx, cc.this()).ok_or_else(|| ctx.throw_type("not a Counter"))?;
        Ok(ZqValue::from(p.label))
    }

    // One body, two JS names: dispatch on cc.magic().
    #[zq_method(name = "fire", magic = 1)]
    #[zq_method(name = "spark", magic = 2)]
    fn magic_demo<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let _ = ctx;
        Ok(ZqValue::int32(cc.magic()))
    }

    // #[zq_static_method] -> on the constructor (`Counter.isCounter(x)`).
    // The same payload guard doubles as a type check.
    #[zq_static_method(name = "isCounter")]
    fn is_counter<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Ok(ZqValue::bool(JsCounter::payload(ctx, cc.arg(0)).is_some()))
    }

    // Static getter -> `Counter.kind`.
    #[zq_static_method(get)]
    fn kind<'a>(ctx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        ZqValue::new_string(ctx, "native")
    }
}

// ── A payload with no JS handles opts out of tracing ─────────────────────
struct PointPayload {
    x: i32,
    y: i32,
}

// `payload_no_gc` skips the per-instance trace call. Without it, `#[zq_class]`
// wires PointPayload's ZqGcTrace into the class so the GC can follow JS handles
// stored inside it (that's why CounterPayload derives ZqGcTrace). PointPayload
// holds plain i32s, so there is nothing to follow.
//
// ⚠ DANGER: if you use `payload_no_gc` but the payload actually contains a
// ZqRawValue, the GC will not know about that reference. It may collect the
// JS object while your payload still points at it, causing undefined behavior.
// Only use `payload_no_gc` when the payload holds zero JS handles — no
// ZqRawValue, no ZqValue, nothing that refers into the JS heap.
#[zq_class(name = "Point", payload = PointPayload, payload_no_gc)]
struct JsPoint;

#[zq_class_impl]
impl JsPoint {
    fn constructor<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let x = cc.arg(0).to_i32(ctx)?;
        let y = cc.arg(1).to_i32(ctx)?;
        JsPoint::new_instance(ctx, PointPayload { x, y })
    }
}

// ── Export the classes in the module manifest ────────────────────────────
#[zq_module(name = "mymod", exports = [JsCounter, JsPoint])]
fn module_init() {}
```

Key points:

- A class is a **payload** (the per-instance data your methods read and write)
  plus a unit struct tagged `#[zq_class(name, payload)]`. Methods go in the
  paired `#[zq_class_impl]` block. The engine boxes one payload per instance
  and drops it when the JS object is collected.
- `fn constructor` is recognized by name. If the extension builds the
  instances itself (a factory returns them), make the constructor throw.
- `#[zq_method]` → prototype; `#[zq_static_method]` → constructor; `get`/`set`
  turn a body into a property. JS names default to the Rust names.
- Guard every method with `payload(...)` / `payload_mut(...)` and
  `ok_or_else(|| ctx.throw_type(...))?`.
- Return `cc.this()` for chaining; return another class with
  `OtherClass::new_instance(ctx, ...)`.
- `magic = N` + `cc.magic()` serves several JS names from one body.
- Store JS handles as `ZqRawValue` and trace them (`#[derive(ZqGcTrace)]`); a
  payload that holds no JS handles passes `payload_no_gc` to skip tracing; a
  tuple/newtype payload needs a manual `impl ZqGcTrace for Payload { ... }`.
- Classes are exported like functions: put them in
  `#[zq_module(exports = [...])]`.
- `examples/zapi_event` has more class patterns; full GC rules are in
  [gc.md](gc.md).

---

## 6. How to register the module

When the host loads your `.dylib/.so/.dll`, it calls `zq_module_register` — a symbol
`#[zq_module]` generates for you. That call hands the zuqe engine the list of
exports and runs the function you tagged, once:

```rust
#[zq_module(name = "mymod", exports = [add, describe, version, JsCounter])]
fn module_init() {
    // This body is your module initialization. It runs once, at load time;
    // put anything that doesn't need JS here: register a C library, warm a
    // cache, spawn a worker, ...
}
```

`exports` is explicit — nothing is auto-discovered. If the list gets long, point
at a `const [ZqFieldDef; _]` slice instead: `exports = MY_EXPORTS`.

The `exports` list uses **Rust identifiers**, not JS names. For functions, the JS
name is the Rust name (or `#[zq_function(name = "...")]` if renamed). For classes,
the JS name is the `name = "..."` in `#[zq_class(name = "...")]` — so `JsCounter`
in the export list becomes `Counter` in JavaScript.

### Accessing the JS context in `module_init`

The simple form takes no arguments. If you need the JS context at load time —
to prime a global object, register a GC tracer, or do anything involving JS
values — accept `&ZqCtx`:

```rust
#[zq_module(name = "mymod", exports = [...])]
fn module_init(ctx: &ZqCtx) -> Result<(), ZqError> {
    // e.g. register a GC tracer for thread-local JS handles
    ctx.gc_set_trace_func(Some(my_trace_fn), my_state_ptr)?;
    Ok(())
}
```

---

## 7. Async work off the JS thread

JS runs on one thread. If your extension does something slow (I/O, a native
library call), doing it inline blocks the whole interpreter. The pattern:
snapshot the arguments, hand them to a worker thread, return a Promise, and
build the JS result back on the JS thread when the work finishes.

### 7.1 The one-shot Promise: `promise_threaded`

The simplest form needs no channels at all:

```rust
#[zq_function(name = "doubleAsync")]
fn double_async<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let n = cc.arg(0).to_f64(ctx)?;
    ctx.promise_threaded(
        move || n * 2.0,                          // work — off the JS thread
        |ctx, result| Ok(ZqValue::number(result)), // convert — back on JS thread
    )
}
```

```js
await my.doubleAsync(21); // 42
```

`work` runs on its own thread and must not touch JS values. `convert` runs back
on the JS thread; returning `Err(ctx.throw_type(...))` rejects the Promise. The
arguments were moved into `work` as plain Rust data, so no GC concerns.

Thread-safety constraints:

- `work` must be `Send + 'static` — it runs on a dedicated OS thread.
- `convert` must be `FnOnce + 'static` — it runs on the JS thread, once.
- The return type `T` of `work` must be `Send + 'static` so it can be moved
  across threads. Do **not** put `ZqRawValue` or any GC-managed handle in `T` —
  the GC may collect the underlying object while `T` is on the worker thread.
  Snapshot values as plain Rust types (`f64`, `String`, `Vec<u8>`, etc.).

### 7.2 A long-lived native resource

Some native things must outlive a single call — a database connection, a model,
a file handle. Put it in the class payload and move a handle into
`promise_threaded`; how you guard it or which thread it runs on is your call (a
`Mutex`, a channel, a dedicated thread, ...). The payload is plain Rust data:

```rust
struct DbPayload {
    db: Arc<Mutex<Connection>>,   // no JS handles -> payload_no_gc
}

#[zq_class(name = "Db", payload = DbPayload, payload_no_gc)]
struct JsDb;
```

A method grabs the handle, snapshots its arguments, and hands the blocking call
to `promise_threaded`:

```rust
#[zq_method]
fn execute<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let p = JsDb::payload(ctx, cc.this()).ok_or_else(|| ctx.throw_type("not a Db"))?;
    let db = Arc::clone(&p.db);                        // move a handle into `work`
    let sql = cc.arg(0).coerce_string(ctx)?;           // owned Rust data
    let params = collect_params(ctx, &cc, 1)?;         // owned Rust data

    ctx.promise_threaded(
        move || db.lock().unwrap().execute(&sql, &params), // off the JS thread
        |ctx, result| match result {
            Ok(changes) => build_changes(ctx, changes),    // back on the JS thread
            Err(e) => Err(ctx.throw_type(&e)),
        },
    )
}
```

Build the resource in the constructor:

```rust
fn constructor<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let conn = Connection::open(cc.arg(0).coerce_string(ctx)?)?;
    JsDb::new_instance(ctx, DbPayload { db: Arc::new(Mutex::new(conn)) })
}
```

### 7.3 Async iterators

For many values instead of one, return an async iterator. Implement
`ZqAsyncIterable` and wrap it with `ctx.new_async_iterator` — `next` returns
`Value(v)` to yield, `Done` to end, or `Wait` to be called again next tick:

```rust
struct Rows {
    done: bool,   // plus whatever state you need to produce the next value
}

impl ZqAsyncIterable for Rows {
    fn next<'a>(&mut self, ctx: ZqCtx<'a>) -> Result<AsyncIterResult<'a>, ZqError> {
        if self.done {
            return Ok(AsyncIterResult::Done);
        }
        match fetch_next_row(ctx)? {                      // your code
            Some(values) => Ok(AsyncIterResult::Value(build_row(ctx, values)?)),
            None => {
                self.done = true;
                Ok(AsyncIterResult::Done)
            }
        }
    }
}
```

Return it from a method:

```rust
ctx.new_async_iterator(Rows { done: false })
```

`Wait` is for when the next value isn't ready but blocking would be wrong: the
loop calls `next` again on the next tick.

### 7.4 Streaming with polling jobs

For push-style streams — tokens arriving over time, progress callbacks — use
`ctx.add_job_no_gc` (or `ctx.add_job` when the payload holds JS values). The
pattern is:

1. Create a wake channel (`ctx.make_wake_channel()`) to get a sender + receiver.
2. Spawn a producer thread that sends results through the sender.
3. Register a job that drains the receiver each tick.
4. The sender automatically wakes the event loop on every `send`, so the
   polling job runs only when there is data.

The closure runs once per event-loop tick until it returns `ZqPoll::Done`;
return `ZqPoll::Again` to stay registered, or `ZqPoll::AgainNoHold` for a
fire-and-forget job that must not keep the process alive. `ZqPoll` comes from
`zuqe_zapi` (`zuqe_zapi::ctx`).

```rust
use zuqe_zapi::*;

// A simple streaming counter: sends 0..n from a worker thread,
// the polling job forwards each value to a JS callback.
fn start_stream<'a>(
    ctx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let n = cc.arg(0).to_i32(ctx)?;          // how many values to produce
    let callback = cc.arg(1);                 // JS callback to call per value
    let (tx, rx) = ctx.make_wake_channel();

    // Producer: sends 0..n from a worker thread.
    std::thread::spawn(move || {
        for i in 0..n {
            let _ = tx.send(i);  // wakes the event loop automatically
        }
        // Sender drops here — polling job sees Disconnected next tick
    });

    // Polling job: drains the receiver, calls the JS callback for each value.
    ctx.add_job_no_gc(move |cx| match rx.try_recv() {
        Ok(val) => {
            let _ = cx.call_function(
                callback,
                ZqValue::undefined(),
                &[ZqValue::int32(val)],
            );
            ZqPoll::Again  // keep going
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => ZqPoll::Again,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            // Producer done — notify JS that the stream ended.
            let _ = cx.call_function(
                callback,
                ZqValue::undefined(),
                &[],
            );
            ZqPoll::Done    // unregister this job
        }
    });

    Ok(ZqValue::undefined())
}
```

> **GC safety**: the polling closure has no GC tracer, so it must not capture
> any `ZqValue` / `ZqRawValue`. Keep JS handles in native payloads or pass
> them through the channel as raw data (`i32`, `String`, etc.). The only JS
> values touched inside the closure should be the `callback` passed via a
> separate mechanism (e.g. stored in a native class payload).
>
> `ZqWakeHandle` is `Send + Sync` and thread-safe — the worker thread can
> call `tx.send()` from any thread. The wake is a no-op when the event loop
> is not parked.

---

## 8. Checklist

Before you ship an extension:

- [ ] `crate-type = ["cdylib"]` and a `[lib] name` you're happy to see in
      imports.
- [ ] Every public member is listed in `#[zq_module(exports = [...])]` — the
      list is explicit, nothing is auto-discovered.
- [ ] Class bodies check `payload(...)` and throw a clear `TypeError` on a
      mismatched `this`.
- [ ] Every payload field holding a JS value is a `ZqRawValue` and is traced
      (derive, manual impl, or a registered global tracer). Payloads with only
      native data are marked `payload_no_gc`.
- [ ] Slow work runs off the JS thread (`promise_threaded` / async iterator) —
      never inline on the JS thread.
- [ ] Errors use `ctx.throw_*` and return `Err`; the host turns them into real
      JS exceptions.

---

## 9. Where to look next

| File                             | Why                                                                                    |
| -------------------------------- | -------------------------------------------------------------------------------------- |
| `examples/zapi_basic/src/lib.rs` | Every value type crossing the boundary, plus a GC-traced class and a value export.     |
| `examples/zapi_event/src/lib.rs` | A full class: methods, `once`, `magic` dispatch, static accessors, thread-local state. |
| `examples/zapi_raw/src/lib.rs`   | The raw ABI, only if you need to drop below the safe layer.                            |
| [gc.md](gc.md)                   | The full GC rules for native code.                                                     |
