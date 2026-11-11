# Native Classes

> Covers host and engine-builtin native classes. **zapi extensions** define
> classes through the zapi ABI (`ZqClassDef`/`ZqFieldDef`, registered by the
> host). They do not use `def_native_object!`. See the boundary at the end and
> [Modules: ESM Loading and Native Modules](native-module.md).

## `def_native_object!`: host classes

Source: `src/class/native.rs`. Host classes are collected at link time in
`HOST_CLASS_DEFS`. The engine assigns their class id during engine init. Before
init, `Type::class_id()` is 0. Call `use_class` before using the class in a
context.

```rust
use zuqe::{
    CallContext, ClassCtorDef, JSResult, JSValue, NativeObject, ScopedContext,
    def_native_object,
    gc_lite::GcTrace,
};

#[derive(Debug, GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
pub struct FooPayload {
    value: u32,
    // GcTrace derive covers GC node fields. Mutable GC fields also need a
    // WriteBarrier. See gc-trace.md and gc-write-barrier.md.
    cached: Option<zuqe::Object>,
}

// Use derive in most cases. It covers all fields and enum variants.
// Hand-write impl GcTrace only for special logic. See the no-GC payload
// section below for payloads without GC nodes.

fn js_foo_ctor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // Constructor new_target is cc.this() by engine convention.
    JSFoo::new_instance(ctx, cc.this(), FooPayload { value: 0, cached: None })
        .map(JSValue::from)
}

pub const FOO_PROTO: &[zuqe::FieldDef] = &[
    // FieldDef::FUNC, GETSET, ... see native-function.md
];

def_native_object!(
    "Foo",
    JSFoo,        // #[repr(transparent)] wrapper over Object
    FooPayload,   // use () for no payload; always use () for zero-size payloads
    FOO_PROTO,
    Some(ClassCtorDef::new(js_foo_ctor, 0, false)), // (ctor, length, call_as_func)
    // extends: JSBase::class_id,                    // optional parent (see inheritance)
);
```

`ClassCtorDef::new(callback, length, call_as_func)`:

- `callback`: constructor callback, same signature as `CallbackFn`.
- `length`: `Function.length`.
- `call_as_func`: `true` allows a plain call without `new`
  (`Callback::ConstructorOrFunc`). `false` allows `new` only
  (`Callback::Constructor`).
- Chain methods: `.expose_to_global()` installs it on the global object.
  `.magic(i16)` sets the magic. `.entries(&'static [FieldDef])` adds static
  members.

For a host class with a payload, `def_native_object!` wires
`drop_payload_fn::<P>` and `user_payload_trace::<P>`. The latter requires
`P: GcTrace`. Therefore, **a payload type without `GcTrace` fails to compile at
the macro call**. `extends:` only declares the parent. A child payload does not
share the parent trace.

### Context setup

```rust
ctx.use_class(JSFoo::class_id())?;
// Then install the prototype or constructor on the global object as needed.
```

`use_class` and `use_classes` grow the class slots as needed and initialize the
class. A host class with `extends` is linked by the engine. See
`host_class_extends_links_both_chains` in `src/class/native.rs`.

### Payload access

The macro generates:

- `payload() -> Option<&Payload>` and `payload_mut() -> Option<&mut Payload>`
- `take_payload() -> Option<Box<Payload>>` (empties the payload slot)
- `payload_of(&JSValue) -> Option<&Payload>`,
  `payload_mut_of(&mut JSValue)`, and `take_payload_of(&mut JSValue)`

The macro and `NativeObject` check the class id. `payload_raw` and
`replace_payload` are low-level. Prefer the generated type-safe methods.

### Create instances

- Inside a constructor: `Self::new_instance(ctx, cc.this(), payload)`.
  `cc.this()` is the new target.
- Outside a constructor (factory or module export):
  `Self::new_instance(ctx, JSValue::UNDEFINED, payload)`. The engine uses the
  class prototype.

```rust
impl JSFile {
    pub fn create_file_instance(ctx: &mut ScopedContext, fh: std::fs::File)
        -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::File(fh)))
            .map(Into::into)
    }
}
```

`FieldDef::LAZY_VALUE` can build instances during module export:

```rust
FieldDef::LAZY_VALUE("in", |cx| {
    JSFile::create_stdin_instance(cx).map(JSValue::from)
}, JS_PROP_ENUMERABLE)
```

### Export a constructor with `FieldDef::CLASS`

```rust
static FILE_MODULE_ENTRIES: &[FieldDef] = &[
    FieldDef::CLASS("File", || JSFile::class_id(), JS_PROP_C_W_E),
];
```

`FieldDef::CLASS` takes `fn() -> ClassID`. The module init looks up the class id
and exposes the constructor.

### No payload

```rust
def_native_object!("Bar", JSBar, (), BAR_PROTO, None);
// JSBar::new_instance(ctx, new_target) -> Result<Self, i32>
```

A zero-size or stateless payload should not exist. Use `()` directly. Do not
write `struct BarPayload;` and give it an empty `GcTrace`. A class with no
per-instance state (say, a stateless `TextEncoder` wrapper) is the case `()`
was made for: it avoids a `Box` allocation and `payload_drop` per instance.

### Payload with no GC nodes

A payload may hold data but no `JSValue`, `Object`, `GcRef`, or `WriteBarrier`
in its transitive closure. Choose by whether the fields implement `GcTrace`:

```rust
// All fields can be traced (String, Vec<u8>, Arc<[u8]>, ...): use derive.
// It generates an empty trace today, and future GC fields are covered.
#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
pub(crate) struct BlobPayload {
    bytes: Arc<[u8]>,
    type_: String,
}

// Fields do not implement GcTrace (raw handle, third-party container,
// tokio Mutex, ...): use the explicit empty trace macro instead of a
// hand-written empty impl.
pub(crate) struct IoHandlePayload {
    file: tokio::fs::File,
    kind: HandleKind,
}
zuqe::gc_lite::unsafe_empty_gc_trace!(IoHandlePayload);
```

`unsafe_empty_gc_trace!` is an assertion about the **whole type**. It is not a
way to silence the compiler:

- The type's transitive closure must contain no GC nodes. If you later add a
  `JSValue`, `Object`, or `WriteBarrier` field, remove the macro and add trace.
  Otherwise you get a use-after-free.
- If only one or two fields lack `GcTrace`, prefer `#[derive(GcTrace)]` plus
  `#[gc(skip)]`. It is smaller and safer.
- `gc_audit` recursively checks types marked by the macro. A reachable GC node,
  including one inside `WriteBarrier`, fails the audit.

## Constructors, methods, properties, prototypes

- **Prototype methods and properties**: declare with `proto_entries`
  (`&[FieldDef]`). Use `FieldDef::FUNC`, `GETSET`, constants, and so on.
- **Constructor**: `ClassCtorDef::new(fn, length, call_as_func)`.
- **`this` checks**: use `NativeObject::from_value`, `from_object`, or
  `JSFoo::payload_of(&cc.this())`. Throw `TypeError` on failure.
- **Property flags**: choose `JS_PROP_*` to match ECMA/WebIDL semantics. Do not
  widen configurable, writable, or enumerable without a reason.

## Class inheritance: one path only

This is a project rule. It keeps one linking implementation.

1. Build the root class first, then link inheritance. Do not build chains by
   hand in constructors.
2. **Host classes**: declare the parent with
   `def_native_object!(..., extends: Parent::class_id)` and initialize with
   `use_class`. The engine links both sides.
3. **Engine builtins and kernel primitives**: use
   `ScopedContext::link_class_extends`, `init_class_proto_extends`, and related
   `bind_ctor_proto*` functions. These are `pub(crate)` primitives in
   `src/class/using.rs` and are the only engine implementation. Builtin
   `class_init` code must reuse them.
4. **Do not** write `obj.set_prototype(...)` to build a class prototype or
   constructor chain. Only the engine primitives may do this.
5. **Do not expose InitFn to third parties.** zapi extensions use the zapi
   path. Do not expose or copy builtin init mechanisms to them.
6. **Do not mix host/builtin and zapi styles.** Choose the path first. See
   [workflow-checklist.md](workflow-checklist.md).

> If you are unsure about an inheritance pattern: use
> `def_native_object! extends` when possible. Otherwise read
> `src/class/using.rs` and existing builtins. Do not invent a new way to link
> classes.

## GC payload design

> Rules are in [GC Safety Rules](gc-safety.md). Trace examples are in
> [GC Tracing](gc-trace.md). Barrier patterns and tools are in
> [Write Barriers](gc-write-barrier.md).

For classes and payloads, focus on Rule 1 (trace) and Rule 5 (late-write
barrier). Apply the same rules to `extends:` subclasses, weak fields,
`#[gc(skip)]`, and container elements with `GcBind`. Host payloads have no
automatic re-trace fallback. The zapi per-minor payload re-scan is only for the
zapi path.

For payloads with no GC nodes, choose the smallest form: zero-size or
stateless uses `()`; non-zero fields without `GcTrace` use
`unsafe_empty_gc_trace!`; traceable fields use `#[derive(GcTrace)]`. Do not use
an empty hand-written `GcTrace` impl as the default.

## zapi third-party classes

zapi extensions describe classes and members with `ZqClassDef`, `ZqFieldDef`,
and body trampolines. `src/zapi/host.rs` loads and registers them. GC tracing
uses the class `gc_trace` callback. The engine re-scans the payload of every
**old zapi instance** on each minor. This compensates for the missing barrier
ABI, but the callback must still report every GC field.

This path is ABI/FFI. It does not use `ScopedContext`, `FieldDef`, or
`def_native_object!`. Third parties do not get InitFn or other builtin
privileges.

If the task is "embed zuqe in a host binary and add builtin features", use the
host path. If it is "a loadable cdylib extension", use zapi and follow its
rules.
