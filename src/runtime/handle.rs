// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#[cfg(not(debug_assertions))]
use std::sync::Once;
use std::{
    cell::{RefCell, UnsafeCell},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    error::Exception,
    runtime::{RuntimeImpl, tick::TickStatus},
};

// ── Thread-local scheduler ─────────────────────────────────────────────
//
// Every *root* Runtime created on the current thread is registered here as a
// Weak reference. Child runtimes (see `Runtime::create_child`) are deliberately
// absent: they are driven by their parent's `tick()`. This avoids preventing
// RuntimeImpl from being dropped when all user handles are gone, and lets the
// scheduler iterate roots without filtering children. Dead weak refs are
// skipped on upgrade.
thread_local! {
    pub(crate) static RUNTIMES: RefCell<Vec<Weak<UnsafeCell<RuntimeImpl>>>> = const { RefCell::new(Vec::new()) };
}

/// Register a runtime into the current thread's scheduler.
/// Reuses slots from dead weak refs when capacity is exhausted.
fn scheduler_register(rt: &Runtime) {
    RUNTIMES.with(|rts| {
        let mut rts = rts.borrow_mut();
        let weak = Rc::downgrade(&rt.0);

        if rts.len() < rts.capacity() {
            // Spare capacity — fast path, no scan.
            rts.push(weak);
        } else if let Some(slot) = rts.iter_mut().find(|w| w.upgrade().is_none()) {
            // Capacity full but a dead slot exists — reuse it.
            *slot = weak;
        } else {
            // No dead slot — must grow.
            rts.push(weak);
        }
    });
}

// ── Panic handler (release builds only) ────────────────────────────────

#[cfg(not(debug_assertions))]
static PANIC_HOOK: Once = Once::new();

/// A ref counted handle to runtime.
/// When last handle dropped, associated runtime will be dropped.
#[derive(Clone)]
#[repr(transparent)]
pub struct Runtime(Rc<UnsafeCell<RuntimeImpl>>);

impl Deref for Runtime {
    type Target = RuntimeImpl;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.get() }
    }
}

impl DerefMut for Runtime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.0.get() }
    }
}

impl Runtime {
    /// Whether the platform can report the current thread's stack bottom.
    /// When false, auto mode (`stack_budget == 0`) cannot arm a C-stack guard
    /// (see `js_check_stack_overflow`); hosts should fall back to a strict
    /// budget, e.g. 512 KiB, unless they want unbounded recursion to crash.
    pub const CAN_AUTO_STACK_BUDGET: bool = crate::utils::STACK_BOTTOM_QUERYABLE;

    /// Build a runtime, optionally registering it in the current thread's
    /// scheduler. Roots register (so `join_each`/`tick_all_to_idle` see them);
    /// children do not — they are driven by their parent's `tick()`.
    fn create_inner() -> Result<Runtime, i32> {
        let rt = Rc::new(UnsafeCell::new(RuntimeImpl::new()?));

        let rt_ptr = rt.get();
        unsafe {
            (*rt.get()).gc_heap_mut().set_opaque(rt_ptr.cast());
        }

        Ok(Self(rt))
    }

    /// Create a runtime and register it in the current thread's scheduler.
    pub fn create() -> Result<Runtime, i32> {
        let rt = Self::create_inner()?;
        scheduler_register(&rt);
        Ok(rt)
    }

    /// Create a *child* runtime of `parent`: same as [`Runtime::create`] but
    /// marked as a child (`is_root()` == false) and registered in
    /// `parent.children` (strong). The child shares the parent's park state, so
    /// a wake from the child (IO / deadline) wakes the parent's event loop.
    /// The parent's `tick()` drives the child; the parent owns its lifetime —
    /// call [`Runtime::remove_child`] to detach it.
    #[cfg(feature = "child_runtime")]
    pub fn create_child(parent: &Runtime) -> Result<Runtime, i32> {
        // Not registered in `RUNTIMES`: the parent's `tick()` drives it.
        let rt = Self::create_inner()?;
        {
            let inner = unsafe { &mut *rt.0.get() };
            inner.is_child = true;
            inner.park = parent.park.clone();
        }
        unsafe {
            (*parent.0.get()).children.push(rt.clone());
        }
        Ok(rt)
    }

    /// Detach `child` from this runtime's children (by identity). The child is
    /// dropped once all other handles are gone. No-op if not a child here.
    #[cfg(feature = "child_runtime")]
    pub fn remove_child(&self, child: &Runtime) {
        unsafe {
            (*self.0.get()).children.retain(|c| !Self::ptr_eq(c, child));
        }
    }

    /// Wrap an existing `Rc<RuntimeImpl>` into a `Runtime` handle.
    /// Used by the scheduler to create handles from upgraded `Weak` refs.
    pub(crate) const fn from_inner(inner: Rc<UnsafeCell<RuntimeImpl>>) -> Self {
        Self(inner)
    }

    /// Returns `true` if both handles point to the same runtime.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    #[cfg(debug_assertions)]
    pub const fn install_panic_handler(&self) {}

    #[cfg(not(debug_assertions))]
    pub fn install_panic_handler(&self) {
        PANIC_HOOK.call_once(|| {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                // Release builds abort on panic (`panic = "abort"`), so unwinding
                // never runs and GC would otherwise leak. Free the GC of every
                // live runtime on this thread instead of the single runtime that
                // installed the hook. The thread-local scheduler holds weak refs
                // to exactly the runtimes created on this thread, so other
                // threads' runtimes are never touched. If the registry is
                // mid-borrow (e.g. a panic during runtime creation) we skip the
                // cleanup rather than double-panicking.
                RUNTIMES.with(|rts| {
                    if let Ok(rts) = rts.try_borrow() {
                        // Roots, plus every descendant (children are not in the
                        // root-only registry). Hold the handles so each runtime
                        // stays alive while its GC is freed.
                        let mut stack: Vec<Runtime> = rts
                            .iter()
                            .filter_map(|w| w.upgrade())
                            .map(Runtime::from_inner)
                            .collect();
                        while let Some(rt) = stack.pop() {
                            let p = rt.0.get();
                            unsafe { (&mut *p).panic_free_gc() };
                            #[cfg(feature = "child_runtime")]
                            {
                                let children = unsafe { (&*p).children.clone() };
                                stack.extend(children);
                            }
                        }
                    }
                });

                eprintln!("{info}");
                prev(info);
            }));
        });
    }

    /// Drive the event loop to completion *synchronously* (without a tokio
    /// executor). Used by the test262 agent harness, where an agent runs on a
    /// plain OS thread and must drain microtasks queued by callbacks (e.g. the
    /// `await Atomics.waitAsync(...).value` continuation inside an agent
    /// broadcast callback) before reporting back to the parent.
    ///
    /// Note: `tick()` returns `Progress` only when real progress was made
    /// (a job finished with `Done`/`Err`, a tick handler reported progress, or
    /// a oneshot job was left queued). A polling job that merely returns `Again`
    /// yields `Parked`, NOT `Progress` — so this loop terminates once all
    /// *settle-able* work is drained, even if a polling job is still spinning on
    /// `Again`. That polling job is simply left in the queue and handed back to
    /// the next `tick()` caller (e.g. the main thread's event loop); it is never
    /// lost or interrupted.
    ///
    /// Consequently this function always terminates — it cannot busy-loop.
    pub fn drain_jobs(&mut self) {
        while self.tick() == TickStatus::Progress {}
    }
}

impl RuntimeImpl {
    unsafe fn panic_free_gc(&mut self) {
        self.cur_exception = Exception::default();
        self.interrupt_code = None;

        // Pop gc_top_scope first so ContextImpl::drop skips the
        // gc_heap_mut() access during the drop phase (same as
        // RuntimeImpl::finalize).
        for ctx_ptr in &mut self.context_list {
            drop(ctx_ptr.gc_top_scope.take());
        }
        self.context_list.clear();
        self.shape_registry.clear();

        // Same sequence as RuntimeImpl::finalize: prepare (clear scope
        // caches + weak slots) → pre-drop cleanup (runtime side-effects) →
        // trivial payload drops → dealloc. Payload Drop impls must not
        // re-enter the runtime (LTO aliasing) — panics are no exception.
        let pid = self.gc_part_id();
        if let Some(link) = self.gc_heap().finalize_partition_prepare(pid) {
            self.pre_drop_cleanup(&link);

            let registry = self.gc_heap().type_registry();
            gc_lite::GcHeap::sweep_drop_payloads(&link, registry);

            self.gc_heap_mut().dealloc_partition(pid, link);
        }
    }
}

/// A light-weight runtime raw ptr.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
pub(crate) struct RuntimePtr(NonNull<RuntimeImpl>);

impl Deref for RuntimePtr {
    type Target = RuntimeImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for RuntimePtr {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl From<&RuntimeImpl> for RuntimePtr {
    #[inline(always)]
    fn from(r: &RuntimeImpl) -> Self {
        Self(NonNull::from_ref(r))
    }
}

impl From<&mut RuntimeImpl> for RuntimePtr {
    #[inline(always)]
    fn from(r: &mut RuntimeImpl) -> Self {
        Self(NonNull::from_ref(r))
    }
}
