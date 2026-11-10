# GC Tracing: Examples and Pitfalls

> **The only rule source is** [GC Safety Rules](gc-safety.md), rules 0-5. This
> file does not repeat the rules. It gives trace examples, pitfalls, and
> debugging steps. Barrier details and tools are in
> [Write Barriers](gc-write-barrier.md).

> **Use `#[derive(GcTrace)]` in most cases.** Types that hold GC nodes
> (payloads, element types, long-lived state) should use derive. It traces every
> field and enum variant. **Hand-write `impl GcTrace` only when you need custom
> logic** (conditional trace, special containers, special weak behavior). Do not
> hand-write without a reason. A hand-written impl that misses a field is not
> caught by the static audit.
>
> **Types without GC nodes** should not use an empty hand-written impl. Use the
> `()` no-payload form for zero-size types. Use `unsafe_empty_gc_trace!` for
> non-zero types whose fields do not implement `GcTrace`.

## 1. What happens without trace

The GC marks only from roots it knows. A `JSValue` or `Object` in a payload
field, a `thread_local!`, or an untraced Rust container is invisible. If it is
the only reference, the next full mark or major collects it. Rust keeps using
it: UAF.

- Debug builds often show `gc: bad node`.
- Release builds may crash randomly, throw `TypeError`, corrupt callbacks, or
  crash at teardown.
- A short script may never run a full mark or major. "It ran" does not prove
  trace is correct.

Trace covers full marks. Barriers cover minors. See
[GC Safety Rules](gc-safety.md).

## 2. Examples

### 2.1 Payload `GcTrace` (Rule 1)

**Use derive in most cases.** If the type holds GC nodes, start with:

```rust
use zuqe::gc_lite::GcTrace;

#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct MyPayload {
    cb: Object,
    value: Option<Object>,
    items: Vec<JSValue>,
}
```

- `#[gc(crate_path = zuqe::gc_lite)]` is for crates that **only depend on
  `zuqe`**. If you depend on `gc-lite` directly, omit it. The default expands to
  `::gc_lite::...`.
- Derive expands by field and by enum variant. New fields and variants are
  traced automatically.
- Do not derive blindly for empty or no-GC payloads. Use `()` for zero-size
  payloads. Use `unsafe_empty_gc_trace!` for non-zero fields without `GcTrace`.
  See 2.6.

Field attributes affect one field. Other fields are still traced by derive.

**`#[gc(skip)]`: do not trace this field.** Use it for weak targets or handles
not managed by GC:

```rust
use zuqe::gc_lite::{GcTrace, GcWeak};

#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct MyPayload {
    cb: Object,
    // A weak reference does not keep its target alive, so it must not be
    // traced. Without skip, derive fails because Option<GcWeak<Object>>
    // does not implement GcTrace.
    #[gc(skip)]
    weak_target: Option<GcWeak<Object>>,
}
```

**`#[gc(with = "path")]`: trace this field with a function.** Use it when the
field type has no `GcTrace`, or needs special traversal:

```rust
use zuqe::gc_lite::{GcTrace, GcTraceCtx};

struct MyHandle {
    nodes: Vec<JSValue>,
}

// Derive calls path(&self.handle, gcx).
fn trace_handle(handle: &MyHandle, gcx: &mut GcTraceCtx) {
    for node in &handle.nodes {
        node.trace(gcx);
    }
}

#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct MyPayload {
    cb: Object,
    #[gc(with = "trace_handle")]
    handle: MyHandle,
}
```

`skip` must not hide a real strong GC edge. `with` must cover every GC child of
the field. A missed child is as bad as a missing field in a hand-written trace.

**Hand-write `impl GcTrace` only when derive cannot express the logic**, for
example conditional trace, third-party containers, or special weak behavior:

```rust
use zuqe::gc_lite::{GcTrace, GcTraceCtx};

impl GcTrace for MyPayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        // Special logic. Check every field.
        self.cb.trace(gcx);
        // ...
    }
}
```

A hand-written impl has more risk. You must update it when fields change. **The
static audit does not parse the trace body**, so it cannot find a missing field.
Do not hand-write without a clear reason.

### 2.2 `thread_local!` trace (Rule 2)

See [GC Safety Rules](gc-safety.md), Rule 2.

```rust
thread_local! {
    static MY_STATE: RefCell<HashMap<u64, JSValue>> = const { RefCell::new(HashMap::new()) };
}

fn gc_trace_my_state(gcx: &mut GcTraceCtx) {
    MY_STATE.with_borrow(|map| {
        for v in map.values() { v.trace(gcx); }
    });
}

rt.gc_set_trace_handler(|gcx| {
    gc_trace_my_state(gcx);
    // One line per thread-local that holds GC nodes.
});
```

### 2.3 Keep values alive across JS calls (Rule 4)

See [GC Safety Rules](gc-safety.md), Rule 4.

```rust
// BAD: the value left a traced container. The JS call may collect it.
let chunk = queue.borrow_mut().remove(0);
call_js_that_may_gc(ctx)?;
use(chunk); // possible UAF

// GOOD: put chunk in an object-owned traced slot (and bind it) first.
// in_flight is a WriteBarrierVec<JSValue> field on the payload.
// JSFoo is the native wrapper generated by def_native_object! (illustrative).
let owner = this; // JSValue: Copy
{
    let p = JSFoo::payload_mut_of(&mut this).ok_or(-1)?;
    p.in_flight.push(ctx, owner, chunk); // traced and bound
}
call_js_that_may_gc(ctx)?;
{
    let p = JSFoo::payload_mut_of(&mut this).ok_or(-1)?;
    p.in_flight.clear(); // no longer needed; clear needs no barrier
}
```

If the held value is in a payload slot, its write path must use a Rule 5
barrier. See [Write Barriers](gc-write-barrier.md). Do not hold a `payload_mut`
Rust borrow across a JS call. The example uses two short scopes.

### 2.4 Scope and root (Rule 4 scope part)

```rust
ctx.with_root_scope(|mut root| {
    root.with_new_scope(|scope| {
        let tmp = Array::create(scope)?; // protected only in the child scope
        Ok(tmp.into())                   // promoted to the parent scope
    })
});
```

Root-scope values live until the Context is dropped. Use `with_new_scope` for
temporary work. Call `remove_local` when a root value is no longer needed.

### 2.5 Raw pointers into `Vec` (Rule 3)

See [GC Safety Rules](gc-safety.md), Rule 3.

```rust
// BAD: cache an element pointer. A later push may reallocate.
let p: *mut Item = &mut items[0];
items.push(Item::new()); // may reallocate
unsafe { (*p).touch(); } // dangling pointer

// GOOD: re-index after each change, or keep addresses stable.
items.push(Item::new());
let p = &mut items[0];
```

Use `Vec<Box<T>>` when addresses must be stable.

### 2.6 Payload with no GC nodes

Do not default to an empty hand-written `impl GcTrace`. Choose the smallest form:

1. **Zero-size or stateless:** use the `()` no-payload form and remove the
   payload type.

   ```rust
   def_native_object!(
       "TextEncoder",
       JSTextEncoder,
       (),
       TEXT_ENCODER_PROTO,
       Some(ctor),
   );
   // JSTextEncoder::new_instance(ctx, new_target)
   ```

2. **Non-zero size, all fields implement `GcTrace`:** use
   `#[derive(GcTrace)]`. Even if all fields are no-ops today, new GC fields are
   traced automatically.

3. **Non-zero size, fields do not implement `GcTrace`** (plain Rust handles,
   third-party containers, tokio sync primitives): use
   `zuqe::gc_lite::unsafe_empty_gc_trace!`:

   ```rust
   pub(crate) struct BlobPayload {
       bytes: Arc<[u8]>,
       type_: String,
   }
   // Explicit assertion: the payload's transitive closure has no GC nodes.
   zuqe::gc_lite::unsafe_empty_gc_trace!(BlobPayload);
   ```

4. **Any field, including a `#[gc(skip)]` field, may hold a GC node:** this is
   not a no-trace case. Trace it, or handle it explicitly as a weak or external
   handle.

`unsafe_empty_gc_trace!` expands an empty `trace` only. It does not change
payload lifetime. `payload_drop` is still wired by `def_native_object!`. The
`unsafe_` prefix is a contract: the type's transitive closure must have no GC
nodes. If you add a GC field, remove the macro and add trace. `gc_audit`
recursively checks these types, including GC fields inside `WriteBarrier`.

## 3. Pitfalls

### 3.1 Missing one field in trace

Violates Rule 1 in [GC Safety Rules](gc-safety.md).

```rust
struct MyPayload {
    cb: Object,
    value: Option<Object>,
}

impl GcTrace for MyPayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.cb.trace(gcx); // missing self.value
    }
}
```

If `value` is reachable only from the payload, a full mark collects it. Later
use is UAF.

If the type uses `#[derive(GcTrace)]`, a new `value` field is traced
automatically. This bug cannot happen at the type level.

### 3.2 `extends:` child reuses parent payload or trace

Violates Rule 1 in [GC Safety Rules](gc-safety.md).

```rust
struct ParentPayload { p: Object }
struct ChildPayload { c: Object } // no GcTrace of its own
```

GC looks up trace by the object's own `class_id`. The parent trace does not
cover child fields. Every child payload needs its own `#[derive(GcTrace)]` or a
manual impl.

### 3.3 Lazy cached JS object is not traced

Violates Rule 1 in [GC Safety Rules](gc-safety.md).

```rust
struct UrlPayload {
    search_params: RefCell<Option<Object>>, // reachable from payload, not traced
}
```

`searchParams`, parse results, namespaces, `import.meta`, and other lazy caches
can hit this.

### 3.4 `thread_local!` holds nodes but has no trace

Violates Rule 2 in [GC Safety Rules](gc-safety.md).

```rust
thread_local! {
    static CACHE: RefCell<Vec<Object>> = const { RefCell::new(Vec::new()) };
}
// No gc_set_trace_handler and no trace fn for CACHE.
```

The collector cannot see these nodes. They may be collected at any time.

### 3.5 Using a value after `remove`, `pop`, or `take` across JS

Violates Rule 4 in [GC Safety Rules](gc-safety.md).

```rust
let cb = list.borrow_mut().pop().unwrap();
cb.call(ctx, JSValue::UNDEFINED, &[])?; // may GC; cb left the traced container
```

Put `cb` back in a traced slot first, then call JS.

### 3.6 `RefCell` borrow across JS

Violates Rule 4 in [GC Safety Rules](gc-safety.md).

```rust
let mut list = cbs.borrow_mut();
while let Some(cb) = list.pop() {
    cb.call(ctx, JSValue::UNDEFINED, &[])?; // borrow_mut held across JS
}
```

GC traces the same cell during the JS call and calls `borrow()`. That conflicts
with the live `borrow_mut()` and panics with `already borrowed`. Release the
borrow after each value, then call JS.

### 3.7 Relying on `gc_protect_transient`

Violates Rule 4 in [GC Safety Rules](gc-safety.md).

```rust
gc_protect_transient(value); // no-op when the feature is off; still scope-bound
call_js(ctx)?;
use(value);
```

The correct fix is a Rule 4 traced slot.

### 3.8 Stacking temporary values in the root scope

Violates the scope and root part of Rule 4.

```rust
ctx.with_root_scope(|mut root| {
    let tmp = root.create_base_object()?;
    // not removed; leaks until the Context is dropped
    Ok(())
});
```

Use `with_new_scope` for temporary work. If a value really belongs in the root
scope, call `remove_local` when done.

### 3.9 Trace but no barrier

Violates Rule 5.

```rust
// trace covers the slot, but the instance may already be old
self.cache = Some(obj);
```

A full mark sees it, but a minor does not descend old payloads. `obj` may still
be collected. See [Write Barriers](gc-write-barrier.md).

### 3.10 Trace a weak target

Violates the weak and `#[gc(skip)]` part of Rule 1.

```rust
impl GcTrace for Payload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.weak_target.trace(gcx); // weak target should not be traced
    }
}
```

Use the same skip behavior in `GcBind`. With derive, mark weak fields
`#[gc(skip)]`. Do not hand-trace a weak target.

### 3.11 Hide GC fields with `unsafe_empty_gc_trace!`

Violates Rule 1.

```rust
struct MyPayload {
    cb: Object,
}

// Wrong: Object is not traced.
zuqe::gc_lite::unsafe_empty_gc_trace!(MyPayload);
```

A no-trace type is never visited by a full mark. Wrapping fields in
`WriteBarrier` does not help, because no code reaches them. `gc_audit`
`empty_trace_violations` recursively checks types marked by the macro. If it
finds a reachable GC node, it fails. Use the macro only for types with no GC
nodes. When unsure, use derive or `#[gc(skip)]`.

## 4. Debug trace problems

1. **Is it derived?** Most payloads should use `#[derive(GcTrace)]`. Only with a
   hand-written `impl GcTrace` do you need to check every `JSValue`, `Object`,
   wrapper, and callback handle against the trace body.
2. **Lifetime:** is the value reachable only from a payload or `thread_local!`?
   Then check Rule 1 or 2.
3. **`extends:`**: does the child have its own payload and `GcTrace`?
4. **Lazy cache:** are `RefCell<Option<Object>>`, `Vec<JSValue>`, and maps
   covered?
5. **Weak:** is a weak target traced or bound by mistake?
6. **Across JS:** does a value from a traced container stay in use after a JS
   call? Does a `RefCell` borrow cross JS?
7. **No-GC decision:** if you use `unsafe_empty_gc_trace!`, confirm the type's
   transitive closure has no GC nodes. `gc_audit` reports
   `unsafe_empty_gc_trace! type contains GC node`.
8. **Cross-check with the static audit:** `cargo test --test gc_audit` reports
   bare GC fields by shape. It does not prove trace is correct, but it can show
   a missed traceable field. See [Write Barriers](gc-write-barrier.md).
9. **Debug full or major:** use a host GC API (for example `std.gc()` in zjs)
   to force collection and look for `gc: bad node`. Long or stress runs also
   expose problems.

For the rules themselves, use [GC Safety Rules](gc-safety.md), Rules 1, 2, and
4. This file does not repeat them.
