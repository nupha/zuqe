# Write Barriers: Patterns and Tools

> **The only rule source is** [GC Safety Rules](gc-safety.md), rules 0-5.
> Rule 5 is the focus here. Rules 1 and 4 are prerequisites. This file does not
> repeat the rules. It covers barrier patterns, failure modes, the static audit,
> dynamic minor checks, and gc stress.
> Trace patterns and pitfalls are in [GC Tracing](gc-trace.md).

## 1. What happens without a write barrier

With `gc_generations`:

- A new allocation is **young**. A minor scans young nodes plus the
  **remembered set**. It does **not descend old payloads**.
- An old owner writes a young child directly and does not record the edge.
- The next minor does not see the edge. It treats the young child as garbage.
- The owner still holds the reference: **dangling reference / use-after-free**.

Debug builds check after each minor (`dbg_assert_no_stale_old_edges`). A typical
panic:

```text
a live object still points at a node freed by the last minor:
child 0x20000c7aa80 (freed dtype Some(1)), owner 0x20007001e00 (dtype Some(1))
— missing write barrier
```

Release builds have no validator. The same missing barrier does not panic. It is
a **silent dangling reference / UAF**.

Why "the script ran" does not mean safe:

- A minor runs only when young allocation passes a threshold, or on request. A
  short script may never run one.
- An owner may start young and be promoted to old later. The bug appears only
  after promotion.
- Release builds have no debug validator. Many missing barriers show up only
  under stress, long runs, or real load.
- The same bug may show `missing write barrier` in debug. In release it may show
  `gc: bad node`, random `TypeError`, a teardown crash, or a promise that never
  settles.

If the engine or host enables **incremental or concurrent marking**, the rules
are stricter. Every edge created or changed by the mutator during marking must
be visible to the collector. Otherwise a black object can point to a white
object and the mark is lost. Barriers still go through guard slots and `bind`.

## 2. Patterns

> Rules, owner constraints, the store/bind gap, and `GcBind` requirements are in
> [GC Safety Rules](gc-safety.md), Rule 5.

### 2.1 Guard slots

```rust
#[derive(Debug)]
struct FooPayload {
    result: WriteBarrier<JSValue>,          // p.result = v does not compile
    queue: WriteBarrierVec<Box<Req>>,       // queue.push(v) does not compile
    cache: WriteBarrierMap<u64, Object>,    // cache.insert(k, v) does not compile
}

impl FooPayload {
    fn set_result(&mut self, ctx: &ScopedContext, owner: JSValue, v: JSValue) {
        self.result.set(ctx, owner, v); // store and barrier in one step
    }
}
```

A guard slot only handles the barrier. The payload still needs `GcTrace` (see
[GC Tracing](gc-trace.md)). `WriteBarrierVec<T>` and `WriteBarrierMap<K, V>`
implement `GcTrace` when `T`/`V` implement `GcTrace`. If an element type only
implements `GcBind` and not `GcTrace`, the payload derive fails to compile.
Construction-time and late writes are covered by Rule 5 in
[GC Safety Rules](gc-safety.md).

### 2.2 Setter and owner

```rust
// GOOD: put the setter on the native wrapper. The owner comes from self,
// so the caller cannot pass the wrong value.
impl JSFoo {
    pub(crate) fn set_capability(
        &mut self,
        ctx: &ScopedContext,
        promise: Promise,
        funcs: [JSValue; 2],
    ) -> Result<(), i32> {
        let owner = JSValue::from(*self); // JSFoo: Copy
        let p = self.payload_mut().ok_or(-1)?;
        p.promise.set(ctx, owner, Some(promise));
        p.resolving_funcs[0].set(ctx, owner, funcs[0]);
        p.resolving_funcs[1].set(ctx, owner, funcs[1]);
        Ok(())
    }
}
```

If a setter must live on the payload and the caller passes the owner, capture
the instance before borrowing the payload:

```rust
let owner = this;                       // JSValue is Copy
let p = JSFoo::payload_mut_of(&mut this).ok_or(-1)?;
p.set_handler(ctx, owner, Some(cb));    // setter stores and binds
```

> `WriteBarrier::set` takes `owner: JSValue`. `set_node` is `pub(crate)` and is
> for engine code only. Host crates cannot call it. The static audit cannot
> check the owner. A wrapper setter plus review is the safeguard.

### 2.3 `GcBind` for container elements

```rust
impl zuqe::GcBind for Req {
    fn bind_node(&self, ctx: &zuqe::ScopedContext, owner: std::ptr::NonNull<zuqe::gc_lite::GcHead>) {
        zuqe::GcBind::bind_node(&self.cb, ctx, owner);
        // Skip weak references. Match #[gc(skip)] and GcTrace behavior.
    }
}
```

Element type requirements, tuple limits, and weak or `#[gc(skip)]` handling are
in [GC Safety Rules](gc-safety.md), Rule 5. Once the element type implements
`GcBind`, the container `push` and `insert` paths pass. If the element type is
also a root or is held directly, its own fields are still checked as normal
payload fields. Record an exemption when needed. Do not wrap it twice.

### 2.4 Where barriers go

| Case | Barrier point |
|---|---|
| **Function binding** | Function data uses `CallContext::replace_data`, which already binds. Long-lived host state goes in a payload, `thread_local!`, or user state. |
| **Module** | Capability, resolver, namespace, and async parent records are long-lived old containers. Use setters. |
| **Class payload** | Mutable fields use guard slots and setters. Container elements use `GcBind`. |
| **zapi** | No barrier ABI. The engine re-scans old zapi instance payloads on each minor. `gc_trace` must still report every field. |

## 3. Check flow

Barrier check order:

| Step | What to do | Tool or place |
|---|---|---|
| 1. Design review | Check each GC field's trace and late-write path against Rules 0-5 | [GC Safety Rules](gc-safety.md) |
| 2. Static audit | Check shape: bare fields, missing `GcBind`, whole guard replacement, enum variant key, declaration location, unknown types, stale exemptions, no-trace types with GC nodes | Section 4 |
| 3. Dynamic minor | Check timing: does an old-to-young write really miss a barrier? | Section 5 |
| 4. gc stress | Raise minor frequency and cover more paths | Section 6 |
| 5. Review | Check what tools cannot see: `GcTrace` body, write order, owner, inheritance, exemptions, boundaries | [Workflow and Checklist](workflow-checklist.md) |

Static audit green + a focused old-to-young path through a minor + a focused
`--gc-stress` run is a strong signal. It is not a proof. Release correctness
depends on `#[derive(GcTrace)]`, guard slots, setter rules, and review.

## 4. Static audit: `zuqe-gc-audit` (shape)

This is a **pre-commit shape check**. It scans payloads, field types, and write
code. It finds GC edges not covered by a guard or bind, and writes that bypass a
barrier. It points to `file:line`. It does not run GC and does not prove write
order. It turns easy-to-miss GC risks into a fixable report.

The tool is `zuqe-gc-audit`. The CLI and `cargo test --test gc_audit` run the
same checks. Use the test entry in daily work.

The normal command in the target crate is:

```bash
cargo test --test gc_audit
```

The failure output is the report. Passing means these shape rules pass.

### 4.1 What it checks

| Pattern | Example | Risk |
|---|---|---|
| Bare GC field | `value: JSValue`, `handler: Option<Object>` | No guard slot. Late old-to-young writes are not recorded. Hand-written trace can miss it. |
| Container element without bind | `queue: WriteBarrierVec<Req>`, but `Req` has no `impl GcBind` | A young element pushed into the container does not enter the remembered set. A minor may collect it. |
| Whole guard replacement | `self.slot = WriteBarrier::new(v)`, `*controller = WriteBarrier::new(...)`, `mem::replace` | Bypasses `set`, `push`, or `insert`. The edge is not recorded. |
| Same field name in enum variants | `Kind::Default.pending_read` and `Kind::Byte.pending_read` | The tool uses variant-level keys. An exemption for one variant does not hide the other. |
| Wrong payload location | Payload inside a `mod` or function (indented) | The payload is not audited at all. |
| No-trace type with GC nodes | `unsafe_empty_gc_trace!(FooPayload)`, but it has `Object` or `WriteBarrier<...>` fields | The whole type is not traced. A full mark never sees the edges. Wrapping in `WriteBarrier` does not help. |
| Unknown cross-crate type | `handle: ForeignHandle` | The tool cannot tell if it is a GC node. It warns you to check. |
| Stale exemption | Field renamed, wrapped, or deleted, but the exemption remains | The exemption list rots and misleads review. |

"No-trace type with GC nodes" is a whole-type check
(`empty_trace_violations`). A type marked by `unsafe_empty_gc_trace!` must have
no GC nodes in its transitive closure. Both the standard test entry
(`gc_audit_test!` -> `all_violations`) and the CLI run it. **An exemption cannot
silence it.** Remove the macro and add trace, or confirm the type really has no
GC fields.

A scan with too few results also fails (for example, wrong directory or
unrecognized payload registration). This prevents an empty scan from passing.

### 4.2 Standard layout in a crate

Two files are enough:

| Path | Purpose |
|---|---|
| `tests/gc_audit.rs` | One-line test entry |
| `gc_audit_exemption.txt` | The **only** exemption file; next to `Cargo.toml` and `src` |

```rust
// tests/gc_audit.rs
zuqe::gc_audit_test!(30);
```

`30` is the minimum number of payload types to scan. It catches a wrong
directory. For the first run, estimate a value. If the failure says fewer types
were found, lower it slightly to the real count. **Do not** lower it to hide a
payload that was not recognized.

Two hard rules:

1. **Payload types must be top-level** (column 0). A payload inside a `mod` or
   function reports `payload type not parsed`. Move it out.
2. **Keep one exemption file.** The test and the tool both read
   `gc_audit_exemption.txt`. Do not keep an inline list in the test.

### 4.3 Run and read results

```bash
cargo test --test gc_audit
```

Each failure is one line:
`file:line: Type.field: type — reason`:

```text
GC payload audit failed:
  src/foo.rs:12: FooPayload.value: `JSValue` — bare GC field, not wrapped and not allowlisted
  src/foo.rs:20: FooPayload.queue: `WriteBarrierVec<Req>` — container element type has no `impl GcBind` and is not allowlisted
  src/foo.rs:31: *controller = WriteBarrier::new(Some(c)); — whole-field guard replacement (use the payload setter / set / push)
  src/foo.rs:40: FooPayload.cb: `Object` — `unsafe_empty_gc_trace!` type contains GC node `Object`; remove the macro or trace the field
```

`empty_trace_violations` entries appear in the same report. The CLI exits
non-zero for them too.

The reason tells you how to fix it. Passing output looks like:

```text
test gc_audit_payload_fields ... ok
```

This is shape-only. It does not prove write order, trace body, or owner. See
Section 5 and review for those.

### 4.4 Workflow: red to green

1. After GC-related changes, run `cargo test --test gc_audit`.
2. Fix by report keyword:

   | Keyword | Fix |
   |---|---|
   | `bare GC field` | Use `WriteBarrier`, `WriteBarrierVec`, or `WriteBarrierMap`. Late writes go through a setter. |
   | `container element type has no impl GcBind` | Implement `GcBind` for the element type. Skip weak and `#[gc(skip)]` fields. |
   | `whole-field guard replacement` | Do not use `field = WriteBarrier::new(...)`. Use the setter's `set`, `push`, or `insert`. |
   | `payload type not parsed` | Move the payload to the top level (column 0). Do not lower the count or add an exemption. |
   | `unsafe_empty_gc_trace! type contains GC node` | Remove the macro. Use `#[derive(GcTrace)]`, a manual trace, or field-level `#[gc(skip)]`. Keep the macro only when there are truly no GC nodes. |
   | `stale allowlist entries` | Remove or update the exemption. Use `Type::Variant.field` for enums. |
   | `unknown type` | Check whether it is a GC node. If yes, add a guard or trace. Otherwise ignore this non-blocking warning. |

3. **A missing trace field is not found by this tool.** Use derive:

   ```rust
   use zuqe::gc_lite::GcTrace;

   #[derive(GcTrace)]
   #[gc(crate_path = zuqe::gc_lite)]
   struct FooPayload { /* ... */ }
   ```

   New fields and variants are traced automatically. A hand-written
   `impl GcTrace` that misses a field does not turn the audit red.

4. Add an exemption only for a real structural exception. Write the reason.
   **Do not** use exemptions to hide unfinished migration.
5. Rerun until green. Green means shape only. Real old-to-young timing checks
   are in Section 5.

### 4.5 Exemption format

One entry per line. `#` comments and blank lines are allowed.

```text
# Type.field — reason
FooPayload.cache — construction only
FooPayload.handler — weak target; not traced
ReadableControllerKind::Byte.pending_read — bound by the byte-controller setter
CommandArgs::Func.0 — captured before new_instance
```

Field keys:

- Plain struct: `Type.field`
- Enum variant: `Type::Variant.field`
- Enum tuple variant: `Type::Variant.N`

Enum keys are **variant-level**. An exemption for `Kind::Byte.pending_read` does
not cover `Kind::Default.pending_read`. The old form `Kind.pending_read` becomes
stale.

Valid categories (pick one and be clear):

1. **construction only**: never written after construction, and the value is set
   while the instance is young.
2. **root container**: a minor force-descends it, so a barrier has no effect.
3. **bound elsewhere**: the only write path already binds (a setter, or element
   `GcBind` on push).
4. **structural non-goal**: raw pointer mirror, weak target, single-setter
   `Cell` slot, and similar.

Requirements:

- Reasons must be specific and reviewable. Do not write "for now" or "not
  migrated".
- Stale entries fail the test. Keep the list short and in sync with the code.
- `payload type not parsed` cannot be exempted. The payload never entered the
  audit. Move it to top level.
- `unsafe_empty_gc_trace!` type with GC nodes cannot be exempted. Remove the
  macro and add trace, or confirm the type has no GC fields.
- `unknown type` is not a GC risk verdict. An exemption does not remove it. It
  only asks you to check whether the type is a GC node.

### 4.6 When to run

- After adding or changing payload fields.
- After adding or removing `unsafe_empty_gc_trace!`, or adding fields to a
  marked type.
- After adding a container element type or changing `GcBind`.
- After changing setters, late-write paths, or `WriteBarrier` construction or
  replacement.
- After adding enum variants.
- Before commit and in CI.

Treat `cargo test --test gc_audit` as the **minimum** for GC changes. Combine it
with the focused minor check in Section 5, `--gc-stress`, and review for a full
GC safety check.

## 5. Dynamic minor check (timing)

### 5.1 Prerequisites

| Condition | Note |
|---|---|
| **Debug build** | The validator and `gc_minor_now` exist only in debug. Release has no runtime check. |
| **`gc_generations`** | Enables old/young generations and the remembered set. It is in the default feature set. |
| **You can trigger a minor** | Pure native paths do not reach a safepoint. Tests must call `gc_minor_now`. |

Release or feature-off builds: `gc_set_stress` and `gc_minor_now` do not exist
or are no-ops. Do not disable `gc_generations` or set `gc_set_promote_after(0)`
to "remove the panic".

### 5.2 Key API

| API | Purpose | Visibility |
|---|---|---|
| `gc_set_promote_after(n)` | Set the young promotion age. Debug default is 1, release default is 2. `0` means majors only. | public |
| `gc_minor_request()` | **Request** a minor at the next safepoint. Pure native paths do not reach a safepoint. | public |
| `gc_minor_now()` | Run a minor **now** (for tests). | public, `debug_assertions` |
| `gc_minor_count()` | Number of minors run. Use it to assert the test ran one. | public |

> `gc_minor_now` is a host test API (for example `Aike.gcMinor`). Release hosts
> must not depend on it. Internal tests use the crate method `gc_minor()`;
> external crates use `gc_minor_now()`.

### 5.3 Focused old-to-young check

1. Call `gc_set_promote_after(1)` so nodes promote after one minor.
2. Create an owner (native instance plus payload). Call `gc_minor_now()` to
   promote it to old.
3. Create a young child. Remove it from scope protection with
   `remove_local(...)`, so only the payload field refers to it.
4. Write the owner field through a setter or guard slot. Prefer a native wrapper
   setter, where the owner comes from `self`.
5. Call `gc_minor_now()` again:
   - With a barrier: the minor passes and the child stays reachable.
   - Without a barrier: panic with
     `a live object still points at a node freed by the last minor ... — missing write barrier`.
6. Assert the value is still usable. Do not check only "no panic".

> Use one owner per slot in the test. `bind` deduplicates the remembered set by
> owner. Another correctly bound field on the same owner may re-trace the whole
> payload and hide a missing bind on this field.

Test template for an external crate:

```rust
use zuqe::{JSValue, Runtime, ScopedContext};

#[cfg(all(debug_assertions, feature = "gc_generations"))]
#[test]
fn late_write_is_barriered() -> Result<(), i32> {
    let mut rt = Runtime::create()?;
    let mut ctx = rt.create_context(|cx| {
        ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
        Ok(())
    })?;

    ctx.with_root_scope(|mut cx| -> Result<(), i32> {
        cx.rt_mut().gc_set_promote_after(1);

        let mut owner = /* JSFoo::new_instance(&mut cx, ...)? */;

        cx.rt_mut().gc_minor_now(); // owner -> old

        let child = cx.create_base_object()?;
        let child_val = JSValue::from(child);
        let node = std::ptr::NonNull::from(child_val.gc_node().expect("gc node"));
        assert!(cx.remove_local(node));

        // The setter is on the native wrapper. owner = JSValue::from(owner).
        owner.set_cached(&cx, Some(child));

        cx.rt_mut().gc_minor_now(); // missing barrier panics here

        assert!(owner.payload().expect("payload").cached().is_some());
        Ok(())
    })
}
```

Key points:

- **Owner comes from the setter.** Setters such as `set_controller`,
  `set_stream`, `set_algorithms`, and `set_byte_self_handle` live on the native
  wrapper. They use `JSValue::from(*self)`, so the caller cannot pass the wrong
  owner.
- If an existing wrapper or payload API still takes `owner` from the caller,
  capture the correct instance before borrowing the payload. The static audit
  cannot catch a wrong owner. Only the validator and review can.
- Only paths that reach `gc_minor_now()` are checked. Pure native code does not
  reach a safepoint. Trigger it yourself.
- Do not "bind again" in a test to hide a bug. Use the real production write
  path through the setter.
- If the field is a container, use `WriteBarrierVec::push` or
  `WriteBarrierMap::insert`. The element type needs `GcBind`.

### 5.4 Reading a failure

```text
a live object still points at a node freed by the last minor:
child 0x20000c7aa80 (freed dtype Some(1)), owner 0x20007001e00 (dtype Some(1))
— missing write barrier
```

- **owner**: the old node that is still alive.
- **child**: the young node freed by the last minor.
- The field edge between them was not recorded in the remembered set.

Check order:

1. Run `cargo test --test gc_audit` to find a bare field or whole-field
   replacement.
2. Check that every late-write path uses a setter, `set`, `push`, or `insert`.
3. Check the owner (especially shared structures and cross-module writes).
4. Check that container elements implement `GcBind`, and that weak or
   `#[gc(skip)]` fields are skipped.
5. Check whether the value was removed from another traced container and
   written directly. Check Rule 4 in [GC Safety Rules](gc-safety.md).
6. Add an exemption only for a real structural case. Do not "bind again" on the
   field to hide it.

In release, the same missing barrier does not panic. It is silent UAF. Do not
treat a debug panic as test noise.

## 6. gc stress

Stress mode requests a minor on **every GC node allocation**. The validator then
sees almost every old-to-young write. Use it with real scripts and integration
cases after the focused unit test.

If the host has `--gc-stress` (zjs, aike, and others), run a focused script or
test:

```bash
# zuqe examples
./target/debug/examples/zjs --gc-stress tests/test_gc_barriers.mjs
./target/debug/examples/zjs --gc-stress tests/test_gc_stress_regress.mjs

# Hosts with the same flag (check the host CLI)
./target/debug/aike --gc-stress path/to/targeted-test.mjs
```

You can also enable it in test or host code:

```rust
#[cfg(all(debug_assertions, feature = "gc_generations"))]
fn enable_stress(rt: &mut Runtime) {
    rt.gc_set_stress(true);
}
```

Notes:

- In non-debug or non-`gc_generations` builds, `--gc-stress` is usually
  rejected or absent. It is not a silent no-op.
- It is very slow (allocations times heap size). **Use small scripts and
  focused cases.** Do not use it as a full regression switch.
- Keep the full output. On failure, record the running case and the stack with
  `RUST_BACKTRACE=1`.
- Stress only runs minors more often. It does not replace trace or barrier
  modeling. The validator sees only edges that `trace` reports.

## 7. Tool limits

- Static audit: shape only. It does not see timing, `RefCell` across JS, or
  values that escape after `remove`. It **does not parse `GcTrace` bodies** and
  **cannot verify the owner**. A hand-written trace that misses a field, or a
  wrong owner, is not found.
- Static audit `unknown type` is a warning, not proof of safety. Cross-line
  assignments may also be missed.
- The no-trace check (`empty_trace_violations`) recurses only into local
  `struct`/`enum` fields. Unknown cross-crate types are skipped.
  `PhantomData` and function pointers are not treated as owned edges.
- A payload inside a `mod` or function is reported. It is not silently skipped.
- Dynamic minor: only checks paths that reach a minor, only edges that `trace`
  reports, and only in debug. If trace misses a field, the validator cannot see
  that edge.
- gc stress: raises minor frequency only. It does not widen what `trace` sees
  and does not prove unvisited paths.
- **Release correctness depends on `#[derive(GcTrace)]`, private fields, setters
  (owner from self), guard slots, and review. It does not depend on the
  validator.**

## 8. Common errors

Rules are in [GC Safety Rules](gc-safety.md), Rule 5. This table lists symptoms
and how to locate them.

| Symptom | How to locate |
|---|---|
| `— missing write barrier` | A late-write path bypasses the setter or guard; owner is wrong; or a container element lacks `GcBind`. |
| Static audit reports a bare field | The field has no guard slot and no valid exemption. Fix the code. |
| Static audit reports whole-field replacement | `field = WriteBarrier::new(...)`, `*binding = ...`, or `mem::replace` bypasses the barrier. |
| Static audit reports a missing `GcBind` | Implement `GcBind` for the element type. Skip weak and `#[gc(skip)]` fields. |
| Static audit reports `payload type not parsed` | The payload is inside a `mod` or function. Move it to top level. Do not lower the count. |
| Static audit reports `unsafe_empty_gc_trace! type contains GC node` | Remove the macro. Keep it only if there are truly no GC nodes. Otherwise use derive, a manual trace, or `#[gc(skip)]`. |
| Static audit reports `⚠ unknown type` | Check whether it is a GC node. If yes, add a guard or trace. Otherwise accept the non-blocking warning. |
| Static audit reports a stale exemption | The field was renamed, wrapped, or deleted. Use `Type::Variant.field` or `Type::Variant.N` for enums. |
| Static audit is green but full mark loses an object | A hand-written `GcTrace` misses a field. Use `#[derive(GcTrace)]` with `#[gc(crate_path = zuqe::gc_lite)]`. |
| Only `--gc-stress` crashes | Normal allocation frequency is too low. Use a focused minor test to narrow the path. |
| Release crashes randomly, debug does not | Silent UAF from a missing barrier. Go back to static audit, a focused minor, and review. |
| `gc: bad node` | Check trace with [GC Tracing](gc-trace.md), then check the write path. |

Before commit, check Rule 5 in [GC Safety Rules](gc-safety.md). This file does
not repeat the checklist.
