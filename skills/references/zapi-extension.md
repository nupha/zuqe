# Writing a zapi Extension (cdylib)

> For third-party extension authors. A zapi extension is a dynamic library
> (`.so` on Linux, `.dylib` on macOS, `.dll` on Windows). The host loads it as
> an ES module. **Do not mix** zapi `Zq*` ABI types with host
> `ScopedContext`/`FieldDef`/`def_native_object!`.
>
> Implementation and ABI details are in `docs/zapi-extension.md` and
> `src/zapi/host.rs`. Runnable examples: `examples/zapi_basic`,
> `examples/zapi_event`, `examples/zapi_raw`, and the conformance extension
> `examples/zapi_ffi_test`.

## 1. Project setup

```toml
[package]
name = "mymod"
version = "0.1.0"
edition = "2024"

[lib]
name = "mymod"                 # produces libmymod.so / libmymod.dylib / mymod.dll
crate-type = ["cdylib"]        # must be a dynamic library

[dependencies]
zuqe_zapi = "0.4.0"
zuqe_zapi_macros = "0.4.0"
```

```rust
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_function, zq_module};
```

- Do not omit `crate-type = ["cdylib"]`. Otherwise the host has nothing to
  `dlopen`.
- `[lib] name` sets the imported file name. It is public API. Keep it stable.
- The extension and host zapi ABI versions must match. A mismatch fails the
  import with `zapi ABI version N mismatch (expect MIN - MAX)` (and the same
  shape for the API version).

## 2. Minimal module

```rust
#[zq_function]
fn add<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg(0).to_f64(ctx)?;
    let b = cc.arg(1).to_f64(ctx)?;
    Ok(ZqValue::number(a + b))
}

#[zq_function(name = "greet")]
fn greet<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let name = cc.arg(0).coerce_string(ctx)?;
    ZqValue::new_string(ctx, &format!("hello, {name}"))
}

#[zq_module(name = "mymod", exports = [add, greet])]
fn module_init() {}
```

- `#[zq_module]` generates `zq_module_register`. The host calls it after
  `dlopen`.
- `exports` is an explicit list of Rust identifiers. A function JS name defaults
  to the Rust name. Rename it with `#[zq_function(name = "...")]`. For classes,
  the JS name comes from `#[zq_class(name = "...")]`.
- The function marked with `#[zq_module]` is the init hook. It runs once at
  load time. Two signatures are allowed: `fn module_init()` or
  `fn module_init(ctx: &ZqCtx) -> Result<(), ZqError>`.
- Optional `finalize = my_fn` runs once per runtime at teardown. It **must not
  call any JS API**.

## 3. Arguments, return values, and errors

```rust
cc.arg(0).to_f64(ctx)?        // ToNumber
cc.arg(1).to_i32(ctx)?
cc.arg(2).to_bool(ctx)?       // ToBoolean
cc.arg(3).coerce_string(ctx)? // ToString -> String
cc.arg(4).string_read_utf8(ctx)? // strict string read; TypeError on non-string
cc.arg_f64(ctx, 0)?           // convenience alias
cc.len()                      // argument count
cc.this()                     // this
cc.magic()                    // #[zq_function(magic = N)] / #[zq_method(magic = N)]
cc.arg(0).kind()              // ZqValueType::{Undefined,Null,Bool,Number,String,Symbol,Function,Object,BigInt,Exception}
```

Build return values:

```rust
ZqValue::int32(1)
ZqValue::number(1.5)
ZqValue::bool(true)
ZqValue::null()
ZqValue::undefined()
ZqValue::new_string(ctx, "hi")?      // mid-function: `?`, then keep going
ZqValue::new_object(ctx)?
ZqValue::new_array(ctx, &[ZqValue::int32(1), ZqValue::bool(true)])?
```

`ZqValue::new_*` returns `Result<ZqValue, ZqError>`; the plain constructors
(`int32`, `number`, `bool`, `null`, `undefined`) return `ZqValue` directly.

- As a **statement**, unwrap with `?` like any other `Result`.
- As the **last expression** of the function, return it as-is: the function
  already returns `Result<ZqValue, ZqError>`, so write
  `ZqValue::new_object(ctx)` — not `Ok(ZqValue::new_object(ctx)?)`.

Throw a JS exception:

```rust
return Err(ctx.throw_type("`path` is required"));    // TypeError
return Err(ctx.throw_range("expected 2 arguments")); // RangeError
return Err(ctx.throw_reference("missing binding"));
```

`ctx.throw_*` returns `ZqError`. Propagate it with `Err(...)`. The `?` operator
also turns a lower-level error into a JS exception.

Objects, arrays, and calling JS:

```rust
let obj = ZqValue::new_object(ctx)?;
obj.set_property_str(ctx, "key", val)?;   // obj.key = val
obj.get_property_str(ctx, "key")?;        // undefined if missing
obj.has_property_str(ctx, "key")?;
obj.del_property_str(ctx, "key")?;
obj.set_property(ctx, 0i32, val)?;

let arr = ZqValue::new_array(ctx, &[])?;
arr.push(ctx, ZqValue::int32(1))?;
arr.length(ctx)?;
arr.get_property(ctx, 0i32)?;

let json = ctx.get_global_str("JSON")?;
let stringify = json.get_property_str(ctx, "stringify")?;
let text = ctx.call_function(stringify, json, &[obj])?;
// Or callee.call(ctx, this, &args). Construct with
// ctx.call_constructor(callee, &args).
```

The old aliases `get`, `set`, `has`, `get_str`, `set_str`, `has_str`, and
`delete_str` are deprecated — use `get_property` / `set_property` /
`has_property` / `del_property_str` and the `*_str` variants.

## 4. Module values: `#[zq_export]`

```rust
use zuqe_zapi_macros::zq_export;

#[zq_export(name = "VERSION")]
fn version<'a>(ctx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    ZqValue::new_string(ctx, "0.1.0")
}

#[zq_module(name = "mymod", exports = [add, version])]
fn module_init() {}
```

The body runs at module evaluation. It can build any JS value: string, object,
array, or function.

## 5. Native class

```rust
use zuqe_zapi_macros::{zq_class, zq_class_impl, zq_method, zq_static_method, ZqGcTrace};

#[derive(ZqGcTrace)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,   // JS handle; must be traced
}

#[zq_class(name = "Counter", payload = CounterPayload)]
struct JsCounter;

#[zq_class_impl]
impl JsCounter {
    // Recognized by name. `new Counter(label)`.
    fn constructor<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let label = cc.arg(0).raw();
        JsCounter::new_instance(ctx, CounterPayload { count: 0, label })
    }

    #[zq_method]
    fn increment<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JsCounter::payload_mut(ctx, cc.this())
            .ok_or_else(|| ctx.throw_type("not a Counter"))?;
        p.count += 1;
        Ok(ZqValue::int32(p.count))
    }

    #[zq_method(name = "count", get)]
    fn count<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JsCounter::payload(ctx, cc.this())
            .ok_or_else(|| ctx.throw_type("not a Counter"))?;
        Ok(ZqValue::int32(p.count))
    }

    #[zq_static_method(name = "isCounter")]
    fn is_counter<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Ok(ZqValue::bool(JsCounter::payload(ctx, cc.arg(0)).is_some()))
    }
}
```

- `#[zq_method]` installs on the prototype. `#[zq_static_method]` installs on
  the constructor. `get` and `set` make a property.
- Every method must check `this` with `payload` or `payload_mut`. Throw
  `TypeError` on failure.
- Return `cc.this()` for chaining. Return another class with
  `OtherClass::new_instance(ctx, payload)`.
- `magic = N` plus `cc.magic()` lets one body serve several JS names.
- `extends = Base` must name a `#[zq_class]` in the **same extension**.
  Registration order does not matter. A class from another `.dylib`, or a
  builtin such as `Array`, cannot be a parent. Parent lookup is member lookup,
  not `super`. Child payloads are independent.
- `#[zq_class(prototype_members = MY_MEMBERS)]` adds a `&'static [ZqMemberDef]`
  const to the prototype — the raw-ABI way to bulk-add members without writing
  a method for each.
- **GC**: every `ZqRawValue`/`ZqValue` in the payload must be traceable. Use
  `#[derive(ZqGcTrace)]` to cover all fields. Use `payload_no_gc` only when no
  field holds a JS handle. `payload_no_gc` with a real `ZqRawValue` causes a
  use-after-free. Tuple and newtype payloads need a manual `impl ZqGcTrace`.
- Classes must also be in `#[zq_module(exports = [...])]`.

## 6. Async and background work

JS runs on one thread. Move slow work off the JS thread.

### 6.1 `promise_threaded`: one-shot thread work

```rust
#[zq_function(name = "doubleAsync")]
fn double_async<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let n = cc.arg(0).to_f64(ctx)?;
    ctx.promise_threaded(
        move || n * 2.0,                            // runs on a separate OS thread
        |ctx, result| Ok(ZqValue::number(result)),  // back on the JS thread
    )
}
// JS: await my.doubleAsync(21); // 42
```

`work` must be `Send + 'static`. It **must not** capture or return
`ZqRawValue`, `ZqValue`, or other GC handles. Capture plain Rust data (`f64`,
`String`, `Vec<u8>`, `Arc<Mutex<_>>`). `convert` runs once on the JS thread.
`Err(ctx.throw_type(...))` rejects the Promise.

### 6.2 `promise_fut`: run on a registered executor

```rust
// The executor is a plain fn and cannot capture host state.
// Register it once during module init.
// fn spawn_on_host_executor(fut: Pin<Box<dyn Future<Output = ()> + Send + 'static>>)
ctx.set_default_executor(spawn_on_host_executor);

let p = ctx.promise_fut(async move { do_io().await }, |ctx, result| {
    Ok(ZqValue::new_string(ctx, &result)?)
});
```

`promise_fut` needs `set_default_executor` first. Otherwise it returns an
internal error. To capture a tokio handle, use
`promise_fut_on(fut, spawn_fn, convert)`. Its `spawn_fn` is `FnOnce`, so it can
capture:

```rust
let p = ctx.promise_fut_on(
    async move { do_io().await },
    |fut| { tokio_handle.spawn(fut); },
    |ctx, result| Ok(ZqValue::new_string(ctx, &result)?),
);
```

### 6.3 Async iterator

```rust
struct Rows { done: bool }

impl ZqAsyncIterable for Rows {
    fn next<'a>(&mut self, ctx: ZqCtx<'a>) -> Result<AsyncIterResult<'a>, ZqError> {
        if self.done { return Ok(AsyncIterResult::Done); }
        match fetch_next_row(ctx)? {
            Some(v) => Ok(AsyncIterResult::Value(build_row(ctx, v)?)),
            None => { self.done = true; Ok(AsyncIterResult::Done) }
        }
    }
}

ctx.new_async_iterator(Rows { done: false })
```

`AsyncIterResult::Wait` means "not ready now, try next tick". It does not block
JS.

### 6.4 Channel jobs: prefer `spawn_channel_job` / `spawn_event_job`

These wrap `make_wake_channel` + `add_job` for you. The zapi source calls them
the recommended spelling over the raw pieces.

```rust
// Request/response: holds the loop while registered (empty = parked).
let tx = ctx.spawn_channel_job("file-read", |cx, data: Vec<u8>| {
    // deliver `data` to JS ...
    ZqChannelPoll::Done // or Continue to keep waiting
});

// Long-lived event stream: does NOT hold the loop (empty = idle), and each
// tick drains the whole channel instead of one message.
let tx = ctx.spawn_event_job("on-log", |cx, line: String| {
    // forward `line` to JS ...
});
```

Use `spawn_event_job` for listeners that live for the whole program; a holding
job on such a stream would keep the process from ever exiting.

### 6.5 Raw polling jobs: `add_job` and `add_job_no_gc`

```rust
let (tx, rx) = ctx.make_wake_channel::<i32>();

std::thread::spawn(move || {
    for i in 0..10 { let _ = tx.send(i); } // send/drop wakes the event loop
});

ctx.add_job_no_gc(move |_cx| match rx.try_recv() {
    Ok(v) => { println!("stream: {v}"); ZqPoll::Again }
    Err(std::sync::mpsc::TryRecvError::Empty) => ZqPoll::Again,
    Err(std::sync::mpsc::TryRecvError::Disconnected) => ZqPoll::Done,
});
```

- `ZqPoll::Done` ends the job. `Again` keeps it registered.
  `AgainNoHold` keeps it registered but does not keep the process alive.
- The `add_job_no_gc` closure must not capture any `ZqValue` or `ZqRawValue`.
  To capture JS values (for example, a callback for each value), use `add_job`
  and implement `ZqJob` (`run` plus `ZqGcTrace::trace`). Put the callback in a
  traced payload.
- `ZqWakeSender` is `Send + Sync` when `T` is (the waker handle is
  explicitly `Send + Sync`). Clones share one wake token. Drop also
  wakes, so a consumer cannot park forever.

## 7. Host loading

The host must:

1. build with `--features zapi`;
2. install a `Runtime::set_module_loader` that recognizes `.so`, `.dylib`, and
   `.dll`;
3. call `ScopedContext::load_zapi_extension(path)`. The zjs loader does this.

```js
import * as my from "./libmymod.so";
my.add(2, 3); // 5
```

No loader means no `import`. The host caches extension libraries for the
process and keeps them mapped. It does not unload them, so exported function
pointers do not dangle.

## 8. Checklist

- [ ] `crate-type = ["cdylib"]`, and `[lib] name` is stable.
- [ ] Every public member is in `#[zq_module(exports = [...])]`.
- [ ] Every class method checks `this` with `payload` or `payload_mut`.
- [ ] Payloads with JS handles have `ZqGcTrace`. `payload_no_gc` is only for
      zero JS handles.
- [ ] Slow work uses `promise_threaded`, `promise_fut`, an async iterator, or a
      channel job (`spawn_channel_job` / `spawn_event_job`). It does not block
      the JS thread.
- [ ] Errors use `ctx.throw_*` plus `Err`. Do not swallow exceptions.
- [ ] Extension and host zapi versions match.
