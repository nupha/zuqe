// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Write-barrier guards: the value-level trait [`GcBind`] (P1) and the payload
//! slot types [`WriteBarrier`] / [`WriteBarrierVec`] (P2).
//!
//! See `memo/gc_bind_guard.md` and `docs/gc.md` Rule 5.

use std::{ops::Deref, ptr::NonNull};

use gc_lite::{GcHead, GcTrace, GcTraceCtx};

use crate::{atom::Atom, context::ScopedContext, object::PropertyKey, value::JSValue};

/// Edge strategy: how one stored value records the GC edge(s) it contributes
/// to `owner`. Needed because container elements are often NOT GC nodes
/// themselves — e.g. `Box<JSAsyncGeneratorRequest>` is traced through the
/// payload's `GcTrace`; the real edges are owner → its four GC fields.
///
/// `GcTrace` reports the edges for a full mark; `GcBind` records them for a
/// minor. A value stored into a container that may already be old must be
/// bound — that is what `ScopedContext::bind_set` /
/// `ScopedContext::bind_push` (test-only L1 primitives) and the guard types do.
pub trait GcBind {
    /// Node-owner primitive: internal call sites hold `NonNull<GcHead>`
    /// (mdef setters, the async-generator queue), not a `JSValue`.
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>);

    /// JSValue-owner convenience for host code.
    #[inline]
    fn bind(&self, ctx: &ScopedContext, owner: JSValue) {
        if let Some(n) = owner.as_gc_node() {
            self.bind_node(ctx, n);
        }
    }
}

impl GcBind for JSValue {
    #[inline]
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        ctx.bind_node_value(owner, *self);
    }
}

impl<T: GcBind> GcBind for Option<T> {
    #[inline]
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        if let Some(v) = self {
            v.bind_node(ctx, owner);
        }
    }
}

impl<T: GcBind> GcBind for Box<T> {
    #[inline]
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        (**self).bind_node(ctx, owner);
    }
}

impl GcBind for Atom {
    #[inline]
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        ctx.bind_atom(owner, *self);
    }
}

impl GcBind for PropertyKey {
    #[inline]
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        ctx.bind_atom(owner, self.atom);
    }
}

// NOTE: the `gcref_wrapper!` / `object_cast!` families generate their own
// `GcBind` impls (crate-internal expansion — they call `bind_node_value`,
// which is `pub(crate)`).

/// Payload field that can only be written through the write barrier.
///
/// No `DerefMut`, no public inner: `field = v` does not compile; use [`WriteBarrier::set`].
/// Zero-cost: `#[repr(transparent)]`, no runtime state — **never add
/// field-level flags**, that would make the type non-transparent.
/// Deliberately not `Clone`/`Default`: copying or resetting would fabricate an
/// unbarriered edge. Whole-field replacement (`field = WriteBarrier::new(v)`)
/// still compiles — that residual hole is closed by the payload-setter
/// convention plus the `tests/gc_audit.rs` assignment rule.
#[repr(transparent)]
#[derive(Debug)] // payloads are `#[derive(Debug, GcTrace)]`; a missing Debug fails the pilot at compile time
pub struct WriteBarrier<T> {
    val: T,
}

impl<T: GcBind> WriteBarrier<T> {
    /// Initializing store: legal ONLY while the owning instance does not exist
    /// yet (payload construction); the instance is born young, so there is no
    /// barrier to record (V8's initializing-store rule). After creation, use
    /// [`set`](Self::set).
    #[inline]
    pub const fn new(v: T) -> Self {
        Self { val: v }
    }

    /// The single write path: store, then barrier. `owner` is the instance node
    /// holding this payload — passing a foreign owner is a silent-corruption
    /// bug that only the debug asserts in `GcHeap::bind` and review can catch,
    /// so call this only from the payload's own setter with
    /// `self.gc_head_ptr()`.
    #[inline]
    pub fn set(&mut self, ctx: &ScopedContext, owner: JSValue, v: T) {
        self.val = v;
        self.val.bind(ctx, owner); // bind the stored value: no `T: Copy` needed
    }

    /// Node-owner form for call sites that already hold `NonNull<GcHead>`.
    #[inline]
    pub(crate) fn set_node(&mut self, ctx: &ScopedContext, owner: NonNull<GcHead>, v: T) {
        self.val = v;
        self.val.bind_node(ctx, owner);
    }
}

impl WriteBarrier<JSValue> {
    /// `set_node` for the paths that hold `&mut RuntimeImpl` and no
    /// `ScopedContext` (the JIT/interpreter `VarRef::set_value` sites).
    /// Same two statements, same no-GC-point rule as [`set_node`](Self::set_node).
    #[inline]
    pub fn set_node_rt(
        &mut self,
        rt: &mut crate::runtime::RuntimeImpl,
        owner: NonNull<GcHead>,
        v: JSValue,
    ) {
        self.val = v;
        rt.bind_node_value(owner, v);
    }
}

impl<U> WriteBarrier<Option<U>> {
    /// Removal: the value leaves the traced slot, so it is no longer covered by
    /// `trace`. No barrier is needed to remove, but if the value is still used
    /// after the next GC point it must be `gc_protect_transient`ed first
    /// (`docs/gc.md` Rule 4). Only `Option` guards can be emptied — a guard
    /// holding a bare `T` has no "empty" state.
    #[inline]
    pub fn take(&mut self) -> Option<U> {
        self.val.take()
    }
}

/// Read path only: `get` copies, so it needs `Copy`; the write path does not.
impl<T: Copy + GcBind> WriteBarrier<T> {
    #[inline(always)]
    pub fn get(&self) -> T {
        self.val
    }
}

impl<T: GcTrace> GcTrace for WriteBarrier<T> {
    #[inline]
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.val.trace(tr);
    }
}

/// A payload `Vec` that can only be mutated through the write barrier.
///
/// Deliberately exposes no `&mut Vec`/`DerefMut`: `vec.push(v)` and
/// `vec[i] = v` do not compile. Zero-cost: `#[repr(transparent)]`, no runtime
/// state. Not `Clone` — copying the container would fabricate unbarriered
/// edges.
#[repr(transparent)]
#[derive(Debug)]
pub struct WriteBarrierVec<T> {
    inner: Vec<T>,
}

impl<T> WriteBarrierVec<T> {
    #[inline(always)]
    pub const fn new() -> Self {
        Self { inner: Vec::new() }
    }

    #[inline(always)]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            inner: Vec::with_capacity(n),
        }
    }

    /// Construction only: the owner does not exist yet, so no edges are bound.
    #[inline(always)]
    pub fn from_vec(v: Vec<T>) -> Self {
        Self { inner: v }
    }

    /// Hand the elements back in one step. They leave the traced slot, so they
    /// lose `trace` coverage: the caller must put them into a rooted Job before
    /// the next GC point (`docs/gc.md` Rule 4) — same family as [`remove`](Self::remove).
    #[inline(always)]
    pub fn into_vec(self) -> Vec<T> {
        self.inner
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Fallible reservation for append paths that turn allocation failure into
    /// their own error instead of panicking (mirrors `Vec::try_reserve`).
    #[inline]
    pub fn try_reserve(&mut self, extra: usize) -> Result<(), std::collections::TryReserveError> {
        self.inner.try_reserve(extra)
    }

    #[inline(always)]
    pub fn first(&self) -> Option<&T> {
        self.inner.first()
    }

    #[inline(always)]
    pub fn get(&self, i: usize) -> Option<&T> {
        self.inner.get(i)
    }

    /// Removal needs no barrier, but the removed element loses `trace`
    /// coverage: protect it (`gc_protect_transient`) before any GC point if it
    /// is still used — `docs/gc.md` Rule 4.
    #[inline]
    pub fn remove(&mut self, i: usize) -> T {
        self.inner.remove(i)
    }

    /// See [`remove`](Self::remove): the popped value is no longer traced.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        self.inner.pop()
    }

    #[inline]
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// See [`remove`](Self::remove) for the removed elements' Rule 4 note.
    #[inline]
    pub fn retain(&mut self, f: impl FnMut(&T) -> bool) {
        self.inner.retain(f);
    }
}

impl<T: GcBind> WriteBarrierVec<T> {
    /// The single append path: push, then barrier. `v` has been moved, so the
    /// just-pushed element is re-fetched for the bind.
    #[inline]
    pub fn push(&mut self, ctx: &ScopedContext, owner: JSValue, v: T) {
        let idx = self.inner.len();
        self.inner.push(v);
        self.inner[idx].bind(ctx, owner);
    }

    /// Node-owner form for call sites that already hold `NonNull<GcHead>`.
    #[inline]
    pub(crate) fn push_node(&mut self, ctx: &ScopedContext, owner: NonNull<GcHead>, v: T) {
        let idx = self.inner.len();
        self.inner.push(v);
        self.inner[idx].bind_node(ctx, owner);
    }
}

impl<T> Deref for WriteBarrierVec<T> {
    type Target = [T];

    #[inline(always)]
    fn deref(&self) -> &[T] {
        &self.inner
    }
}

impl<T> Default for WriteBarrierVec<T> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: GcTrace> GcTrace for WriteBarrierVec<T> {
    #[inline]
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.inner.trace(tr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeObject, Object, Runtime};

    /// Test payload with one slot of every guarded shape.
    #[cfg(feature = "gc_generations")]
    #[derive(Debug, GcTrace)]
    struct GuardPayload {
        scalar: JSValue,
        opt: Option<Object>,
        /// L1 primitive target (`bind_push`), no type-level guard.
        raw_list: Vec<JSValue>,
        list: WriteBarrierVec<JSValue>,
    }

    #[cfg(feature = "gc_generations")]
    crate::def_native_object!("GcBindTest", GcBindTest, GuardPayload, &[], None);

    #[cfg(feature = "gc_generations")]
    fn new_owner(cx: &mut ScopedContext<'_>) -> Result<GcBindTest, i32> {
        GcBindTest::new_instance(
            cx,
            JSValue::UNDEFINED,
            GuardPayload {
                scalar: JSValue::UNDEFINED,
                opt: None,
                raw_list: Vec::new(),
                list: WriteBarrierVec::new(),
            },
        )
    }

    /// Promote the owners, write young values through the barrier, run a minor:
    /// a missing bind leaves an old→freed edge and the debug validator panics
    /// with "missing write barrier".
    ///
    /// One owner per slot: `bind` dedups per owner (remembered set), so a second
    /// bound field on the same owner would re-trace the whole payload and mask
    /// a missing bind on another field.
    // `gc_minor`/`is_young` only exist with `gc_generations`.
    #[cfg(feature = "gc_generations")]
    #[test]
    fn guarded_slots_survive_a_minor_on_an_old_owner() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let cid = <GcBindTest as NativeObject>::class_id();
        let mut ctx = rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            cx.use_class(cid)?;
            Ok(())
        })?;

        ctx.with_root_scope(|mut cx| -> Result<(), i32> {
            cx.rt_mut().gc_set_promote_after(1);

            let mut scalar_owner = new_owner(&mut cx)?;
            let mut opt_owner = new_owner(&mut cx)?;
            let mut raw_owner = new_owner(&mut cx)?;
            let mut list_owner = new_owner(&mut cx)?;

            // Promote the (still empty) owners to old.
            cx.rt_mut().gc_minor();
            assert!(!scalar_owner.gc_node().is_young(), "owner must be old");

            // Young children whose only reference will be the payload slot.
            let scalar_child = cx.create_base_object()?;
            let opt_child = cx.create_base_object()?;
            let raw_child = cx.create_base_object()?;
            let list_child = cx.create_base_object()?;
            for c in [scalar_child, opt_child, raw_child, list_child] {
                assert!(
                    cx.remove_local(c.gc_node_ptr()),
                    "child must be scope-local before the minor"
                );
            }

            let v = JSValue::from(scalar_owner);
            let p = scalar_owner.payload_mut().expect("payload");
            cx.bind_set(v, &mut p.scalar, scalar_child.into());

            let v = JSValue::from(opt_owner);
            let p = opt_owner.payload_mut().expect("payload");
            cx.bind_set(v, &mut p.opt, Some(opt_child));

            let v = JSValue::from(raw_owner);
            let p = raw_owner.payload_mut().expect("payload");
            cx.bind_push(v, &mut p.raw_list, raw_child.into());

            let v = JSValue::from(list_owner);
            let p = list_owner.payload_mut().expect("payload");
            p.list.push(&cx, v, list_child.into());

            // Validator: one unbound edge here panics with "missing write barrier".
            cx.rt_mut().gc_minor();
            assert!(cx.rt().gc_minor_count() >= 2);
            Ok(())
        })?;
        Ok(())
    }

    /// The zero-cost claim: the guards are layout-transparent, so JIT/host
    /// code that reads payload fields by offset is unaffected.
    #[test]
    fn guards_are_layout_transparent() {
        assert_eq!(
            std::mem::size_of::<WriteBarrier<JSValue>>(),
            std::mem::size_of::<JSValue>()
        );
        assert_eq!(
            std::mem::align_of::<WriteBarrier<JSValue>>(),
            std::mem::align_of::<JSValue>()
        );
        assert_eq!(
            std::mem::size_of::<WriteBarrier<Option<Object>>>(),
            std::mem::size_of::<Option<Object>>()
        );
        assert_eq!(
            std::mem::size_of::<WriteBarrierVec<JSValue>>(),
            std::mem::size_of::<Vec<JSValue>>()
        );
        assert_eq!(
            std::mem::align_of::<WriteBarrierVec<JSValue>>(),
            std::mem::align_of::<Vec<JSValue>>()
        );
    }

    /// `#[repr(transparent)]` keeps the offset of a guarded field identical to
    /// the bare type even in a mixed layout, so swapping `JSValue` for
    /// `WriteBarrier<JSValue>` in a payload cannot move the fields the JIT
    /// reads by offset.
    #[test]
    fn a_guarded_field_keeps_its_offset() {
        #[repr(C)]
        struct Guarded {
            tag: u8,
            slot: WriteBarrier<JSValue>,
            tail: u8,
        }
        #[repr(C)]
        struct Bare {
            tag: u8,
            slot: JSValue,
            tail: u8,
        }
        assert_eq!(
            std::mem::offset_of!(Guarded, slot),
            std::mem::offset_of!(Bare, slot)
        );

        #[repr(C)]
        struct GuardedList {
            tag: u8,
            slot: WriteBarrierVec<JSValue>,
            tail: u8,
        }
        #[repr(C)]
        struct BareList {
            tag: u8,
            slot: Vec<JSValue>,
            tail: u8,
        }
        assert_eq!(
            std::mem::offset_of!(GuardedList, slot),
            std::mem::offset_of!(BareList, slot)
        );
    }
}
