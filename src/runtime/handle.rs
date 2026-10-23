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
    runtime::{DummyUserState, RuntimeImpl, RuntimeUserState, tick::TickStatus},
};

use super::tick::tick_tree;

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
pub struct Runtime(pub(super) Rc<UnsafeCell<RuntimeImpl>>);

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
    /// When false, auto mode (`thread_stack_limit == 0`) cannot arm a C-stack guard
    /// (see `js_check_thread_stack_overflow`); hosts should fall back to a strict
    /// limit, unless they want unbounded recursion to crash.
    pub const CAN_AUTO_THREAD_STACK_LIMIT: bool = cfg!(any(
        windows,
        target_os = "linux",
        target_os = "netbsd",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "macos",
    ));

    /// Build a runtime, optionally registering it in the current thread's
    /// scheduler. Roots register (so `join_each`/`tick_all_to_idle` see them);
    /// children do not — they are driven by their parent's `tick()`.
    fn create_inner(user_state: Box<dyn RuntimeUserState>) -> Result<Runtime, i32> {
        let rt = Rc::new(UnsafeCell::new(RuntimeImpl::new(user_state)?));

        let rt_ptr = rt.get();
        unsafe {
            (*rt.get()).gc_heap_mut().set_opaque(rt_ptr.cast());
        }

        Ok(Self(rt))
    }

    /// Create a runtime with the default [`NoUserState`], and register it in the
    /// current thread's scheduler.
    pub fn create() -> Result<Runtime, i32> {
        Self::create_with(DummyUserState)
    }

    /// Create a runtime carrying host-defined `state`, and register it in the
    /// current thread's scheduler. The state is fixed at creation — there is no
    /// setter afterwards; access it via
    /// [`RuntimeImpl::user_state`](crate::RuntimeImpl::user_state).
    pub fn create_with<S: RuntimeUserState>(state: S) -> Result<Runtime, i32> {
        let rt = Self::create_inner(Box::new(state))?;
        scheduler_register(&rt);
        Ok(rt)
    }

    /// Create a *child* runtime of `parent` with the default [`NoUserState`].
    /// See [`Runtime::create_child_with`] for the rest
    /// (`Runtime::create_child_with(parent, NoUserState)`).
    #[cfg(feature = "child_runtime")]
    pub fn create_child(parent: &Runtime) -> Result<Runtime, i32> {
        Self::create_child_with(parent, DummyUserState)
    }

    /// Create a *child* runtime of `parent` carrying host-defined `state`:
    /// same as [`Runtime::create`] but marked as a child (`is_root()` == false)
    /// and registered in `parent.children` (weak). The child shares the
    /// parent's park state, so a wake from the child (IO / deadline) wakes the
    /// parent's event loop. The parent's `tick()` drives the child.
    ///
    /// Ownership: the child holds its parent **strongly** and the parent holds
    /// the child **weakly**. So a parent always outlives its children (the
    /// child keeps the parent's event loop alive), while a child lives exactly
    /// as long as the host's handle to it — drop the handle and the child is
    /// gone, and the parent stops ticking it.
    ///
    /// The child's state is its own box (the parent's cannot be shared); use
    /// this when per-runtime host state must exist in children too.
    #[cfg(feature = "child_runtime")]
    pub fn create_child_with<S: RuntimeUserState>(
        parent: &Runtime,
        state: S,
    ) -> Result<Runtime, i32> {
        // Not registered in `RUNTIMES`: the parent's `tick()` drives it.
        let rt = Self::create_inner(Box::new(state))?;
        {
            let inner = unsafe { &mut *rt.0.get() };
            // The child is created here, fresh — the host supplies only the
            // parent. The parent can therefore be neither the child nor one
            // of its descendants, and the parent chain stays a tree: a
            // runtime is never its own parent, and the chain never closes
            // into a cycle.
            //
            // The assert checks the link that is actually stored, so it
            // catches a future change that lets a host hand in an existing
            // runtime as the child. The chain invariant itself is pinned by
            // a test (`the_parent_chain_is_a_tree`), which holds in release
            // builds too.
            let parent_link = parent.clone();
            debug_assert!(
                !Rc::ptr_eq(&rt.0, &parent_link.0),
                "a runtime cannot be its own parent"
            );
            inner.park = parent.park.clone();
            // The child drags its parent along: the parent must outlive it.
            // This link is also what marks the runtime as a child.
            inner.parent = Some(parent_link);
        }
        unsafe {
            (*parent.0.get()).children.push(Rc::downgrade(&rt.0));
        }
        Ok(rt)
    }

    /// Detach `child` from this runtime's children (by identity).
    ///
    /// This only removes the child from the tick list; it does not decide the
    /// child's lifetime — the host's handle does. No-op if not a child here.
    #[cfg(feature = "child_runtime")]
    pub fn remove_child(&self, child: &Runtime) {
        unsafe {
            (*self.0.get())
                .children
                .retain(|w| w.upgrade().is_none_or(|rc| !Rc::ptr_eq(&rc, &child.0)));
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

    /// Install a JIT state, failing if one is already attached. Host-facing
    /// entry point is `Jit::attach_to`. Takes `&self` because the runtime uses
    /// interior mutability; the host must be single-threaded (it is — the
    /// runtime is `!Send`).
    #[cfg(feature = "jit")]
    pub(crate) fn jit_install(
        &self,
        state: Box<crate::jit::JitState>,
    ) -> Result<(), crate::jit::JitError> {
        let inner = unsafe { &mut *self.0.get() };
        if inner.jit.is_some() {
            return Err(crate::jit::JitError::AlreadyAttached);
        }
        inner.jit = Some(state);
        Ok(())
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
                                // Children are held weakly: upgrade to reach
                                // the live ones, skip the dead.
                                let children = unsafe { (&*p).children.clone() };
                                stack.extend(
                                    children
                                        .into_iter()
                                        .filter_map(|w| w.upgrade())
                                        .map(Runtime::from_inner),
                                );
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

    /// Run background jobs for one round, including this runtime's children.
    ///
    /// The aggregate is the strongest outcome across the tree: `Progress` if
    /// any runtime made progress, else `Park` (earliest reported deadline) if
    /// any is blocked, else `Idle`. A root runtime's interrupt stops the whole
    /// tree.
    ///
    /// Children are ticked through their own [`Runtime`] handles, and this
    /// runtime's `&mut RuntimeImpl` borrow ends *before* the first child runs.
    /// That is what makes a re-entry safe: a child may call back into this
    /// runtime (e.g. an import policy living in this runtime's heap), and that
    /// must not alias a live `&mut RuntimeImpl`.
    #[inline(always)]
    pub fn tick(&mut self) -> TickStatus {
        tick_tree(RuntimePtr::from_raw(self.0.get()))
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

impl RuntimePtr {
    /// Wrap a raw runtime pointer.
    ///
    /// The pointer must be non-null and point at a live runtime.
    pub(crate) const fn from_raw(ptr: *mut RuntimeImpl) -> Self {
        Self(NonNull::new(ptr).expect("runtime pointer must not be null"))
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

#[cfg(all(test, feature = "child_runtime"))]
mod tests {
    use super::*;
    use crate::{Context, JSValue, ScopedContext};

    /// `call_in_ctx` runs the callee in the *target's* context: the argument
    /// is a value of the target's heap, the callee sees the target's globals,
    /// and the return value comes back from there.
    #[test]
    fn call_in_ctx_uses_the_target_context() -> Result<(), i32> {
        let mut rt_a = Runtime::create()?;
        let mut ctx_a: Context<'static> = rt_a.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
        let mut rt_b = Runtime::create()?;
        let mut ctx_b: Context<'static> = rt_b.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        // Each runtime has its own global: the callee must see the target's.
        for (ctx, who) in [(&mut ctx_a, "A"), (&mut ctx_b, "B")] {
            ctx.with_root_scope(|mut cx| {
                cx.eval(
                    format!("globalThis.__who = '{who}';").as_bytes(),
                    &crate::EvalName::Name("who.js".into()),
                    crate::EvalType::Global,
                    crate::EvalFlags::empty(),
                )?;
                Ok::<_, i32>(())
            })?;
        }

        let (f, arg) = ctx_a.with_root_scope(|mut cx| {
            let f = cx.eval(
                b"(name) => globalThis.__who + ':' + name",
                &crate::EvalName::Name("a.js".into()),
                crate::EvalType::Global,
                crate::EvalFlags::empty(),
            )?;
            let arg = cx
                .create_string_from_utf8(b"world".to_vec())
                .map(JSValue::make_string)?;
            Ok::<_, i32>((f, arg))
        })?;

        let out = Context::call_in_ctx(&ctx_a, f, &[arg])?;
        let text = out
            .as_string(false)
            .expect("a string comes back")
            .content()
            .encode_utf8(false)
            .to_string();
        assert_eq!(
            text, "A:world",
            "the callee must run in the target's context"
        );
        Ok(())
    }

    /// A throwing callee fails the call and leaves the exception pending in
    /// the target runtime — the caller can read it, nothing is swallowed.
    #[test]
    fn call_in_ctx_reports_the_thrown_exception() -> Result<(), i32> {
        let mut rt_a = Runtime::create()?;
        let mut ctx_a: Context<'static> = rt_a.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        let f = ctx_a.with_root_scope(|mut cx| {
            cx.eval(
                b"() => { throw new Error('boom'); }",
                &crate::EvalName::Name("a.js".into()),
                crate::EvalType::Global,
                crate::EvalFlags::empty(),
            )
        })?;

        assert!(Context::call_in_ctx(&ctx_a, f, &[]).is_err());
        let text = ctx_a.take_exception_as_text();
        assert!(
            text.message.contains("boom"),
            "the thrown message must survive: {}",
            text.message
        );
        Ok(())
    }
}
