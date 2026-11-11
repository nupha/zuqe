# Native Function Binding

> For **host or engine-builtin** Rust code that depends on `zuqe`.
> **Not for zapi cdylib extensions.** zapi uses `ZqBodyFn`, `ZqRawValue`,
> `ZqRawCtx`, `ZqFieldDef`, and a trampoline. That is a separate ABI path. See
> [Modules: ESM Loading and Native Modules](native-module.md) for the zapi
> boundary. Do not mix the two.

## Core types and signatures

```rust
use zuqe::{CallContext, JSResult, JSValue, ScopedContext, Callback};

// Host/builtin native callback (src/function/nfun.rs)
pub type CallbackFn = fn(ctx: &mut ScopedContext, cc: CallContext) -> JSResult;
// JSResult = Result<JSValue, ErrCode> (error.rs; ErrCode = i32).
// After a throw, return Err(-1) or the throw helper's own code.
```

Common `CallContext` methods (`src/function/call.rs`):

- `cc.this()`: the `this` value.
- `cc.arguments()` and `cc.arg(n)`: argument slice, or argument `n`. Out of
  range returns `UNDEFINED`.
- `cc.length()`: actual argument count.
- `cc.magic()`: magic from `FieldDef::FUNC`. Useful when one callback handles
  several functions.
- `cc.data(n)` and `cc.data_len()`: bound data from
  `create_native_function_with_data`.
- `cc.replace_data(ctx, n, val)`: replace bound data `n`. It stores and binds
  the edge. `NFuncPayload.data` is private, and this is the only late-write
  path.

## Arguments, return values, and errors

- **Read arguments** with `cc.arg(n)` or `cc.arguments()`. `JSValue` has no
  `to_int32`. Use `to_number(ctx)?` to get `IntOrFloat`, then `.to_i32()`,
  `.to_f64()`, or `.to_u32()`. Other conversions are `to_bool()?`,
  `to_string(ctx)?`, and `to_index(ctx)?` (`i64`, for array indexes).
- **Return values**: return `Ok(value)`. For no value, return
  `Ok(JSValue::UNDEFINED)`.
- **Throw**: set the current exception with a `ScopedContext` method such as
  `throw_type_error`, `throw_range_error`, or `throw_internal_error`. The usual
  spelling is `return Err(ctx.throw_type_error("..."));` — the throw helpers
  return the error code. Returning `Err(-1)` after setting the exception
  yourself works too. Do not return `Ok(...)` after setting an exception.
- **Check `this`**: methods should check that `cc.this()` is the expected native
  object or class instance. Throw `TypeError` on a mismatch.

## Registration (host/builtin)

### 1) Use a `FieldDef` table

`FieldDef` (`src/function/fields.rs`) declares fields and methods. It is used by
`def_native_object!` proto and ctor entries, `ClassDef`, and
`ScopedContext::create_module`.

```rust
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext};

fn js_foo_bar(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let _this = cc.this();
    let _arg0 = cc.arg(0);
    // ... check, convert, process ...
    Ok(JSValue::UNDEFINED)
}

pub const FOO_PROTO_FUNCS: &[FieldDef] = &[
    FieldDef::FUNC("bar", 1, js_foo_bar, 0), // (name, length, callback, magic)
    // FieldDef::GETSET, GETSET_ENUM, STR, INT32, ... see fields.rs
];
```

`FieldDef::FUNC` `length` is the JS `length` (`u8`). `magic` is `i16` and can
dispatch several operations from one callback.

### 2) Create a function object and install it

To create a function at runtime and install it on an object or the global
object, use the `ScopedContext` factory (`src/function/nfun.rs`):

```rust
use zuqe::{CallContext, Callback, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

fn host_hello(_ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    Ok(JSValue::UNDEFINED)
}

pub fn install_global(ctx: &mut ScopedContext) -> Result<(), i32> {
    let f = ctx.create_native_function(Callback::Func(host_hello), "hello", 1, 0)?;
    let mut g = ctx.global_obj(); // Object; ContextImpl::global_obj
    g.define_property_value_str(ctx, "hello", JSValue::from(f), JS_PROP_C_W_E)?;
    Ok(())
}
```

The exact mount method depends on the current `Object`/`ScopedContext` API. Pick
property flags that match the intended visibility.

### 3) Export from a module

Native modules export functions through `create_module` and `FieldDef`. See
[Modules: ESM Loading and Native Modules](native-module.md). Module exports and
global installation are different integration points.

### 4) Constructors

For native classes that need `new`, prefer
[Native Classes](native-class.md) with `def_native_object!` and
`ClassCtorDef`. The macro handles the prototype, constructor flag,
`new_instance`, and payload. Calling
`create_native_function(Callback::Constructor(..), ...)` only marks the function
as a constructor. Instance creation and prototype linking still need class or
engine primitives. Do not hand-write them in host code.
`ClassCtorDef::new(cb, length, call_as_func)` third argument means "can also be
called without `new`". It does not mean "is a constructor".

## `ScopedContext` notes

> Scope, roots, and keeping values alive across JS calls are covered by Rule 4
> in [GC Safety Rules](gc-safety.md). Trace examples are in
> [GC Tracing](gc-trace.md).

`ScopedContext` is a GC scope. `with_root_scope` and `with_new_scope` manage
temporary protection and result promotion. In a native callback, `ctx` is
already the current scope. When you allocate a temporary object and return it,
follow the `ScopeResult` promotion rules and the scope boundary.

Keep values alive across JS re-entry per Rule 4. See
[GC Tracing](gc-trace.md).

## Minimal working example

```rust
use zuqe::{
    CallContext, Callback, EvalFlags, EvalName, EvalType, JSResult, JSValue, Runtime,
    ScopedContext, JS_PROP_C_W_E,
};

fn js_add1(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let n = cc.arg(0).to_number(ctx)?.to_i32();
    Ok(JSValue::from(n + 1))
}

fn main() -> Result<(), i32> {
    let mut rt = Runtime::create()?;
    let mut ctx = rt.create_context(|cx| {
        ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
        Ok(())
    })?;

    ctx.with_root_scope(|mut root| {
        root.with_new_scope(|cx| {
            let f = cx.create_native_function(Callback::Func(js_add1), "add1", 1, 0)?;
            let mut g = cx.global_obj();
            g.define_property_value_str(cx, "add1", f.into(), JS_PROP_C_W_E)?;

            let v = cx.eval(
                b"add1(41)",
                &EvalName::Name("<host>".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )?;
            assert!(v.is_number());
            Ok::<(), i32>(())
        })
    })
}
```

> Current signatures: `CallbackFn = fn(&mut ScopedContext, CallContext) ->
> JSResult`, and `JSResult = Result<JSValue, ErrCode>` (`ErrCode = i32`). The
> template shows the
> standard callback, error return, install, and scope shape.

## Anti-patterns

- Use `ZqBodyFn`, `ZqRawValue`, or zapi registration in host code. Or use
  internal `ScopedContext` Rust APIs in a zapi extension unless the boundary
  explicitly allows it.
- Throw an exception and then return `Ok(...)`.
- Return `Err(-1)` without setting an exception. That is a failure with no
  error value.
- After a JS call, keep using a bare `JSValue` or `Object` that was removed from
  a traced container, without Rule 4 protection.
