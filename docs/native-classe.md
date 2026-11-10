# Native Classes

Host native classes are declared with `def_native_object!`; the macro generates a
`#[repr(transparent)]` wrapper, payload accessors (`payload_of`, `payload`,
`payload_mut`, `take_payload`, …), `new_instance`, class-id storage, and the GC
trace/drop wiring. The payload type must implement `gc_lite::GcTrace` (usually
`#[derive(GcTrace)]`), otherwise the macro call does not compile.

The full guide — constructor configuration (`ClassCtorDef`), payload shapes,
`use_class`, `extends:`, `FieldDef::CLASS`, and GC requirements — is in:

- **[Native Classes](../skills/native-class.md)**
- **[GC Safety Rules](../skills/gc-safety.md)**

A minimal declaration:

```rust
use zuqe::{ClassCtorDef, JSResult, JSValue, ScopedContext, def_native_object, gc_lite::GcTrace};

#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct FooPayload { value: u32 }

fn js_foo_ctor(ctx: &mut ScopedContext, cc: zuqe::CallContext) -> JSResult {
    JSFoo::new_instance(ctx, cc.this(), FooPayload { value: 0 }).map(JSValue::from)
}

const FOO_PROTO: &[zuqe::FieldDef] = &[];

def_native_object!(
    "Foo", JSFoo, FooPayload, FOO_PROTO,
    Some(ClassCtorDef::new(js_foo_ctor, 0, false))
);
```

For implementation details of class registration and inheritance linking, see
`src/class/native.rs` and `src/class/using.rs`.
