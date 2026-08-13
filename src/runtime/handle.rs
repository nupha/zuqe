// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    rc::Rc,
};

#[cfg(not(debug_assertions))]
use std::sync::{
    Once,
    atomic::{AtomicPtr, Ordering},
};

use crate::{
    error::Exception,
    runtime::{RuntimeImpl, tick::TickStatus},
};

#[cfg(not(debug_assertions))]
static PANIC_RUNTIME: AtomicPtr<RuntimeImpl> = AtomicPtr::new(std::ptr::null_mut());
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
    /// create runtime
    pub fn create() -> Result<Runtime, i32> {
        let rt = Rc::new(UnsafeCell::new(RuntimeImpl::new()?));

        let rt_ptr = rt.get();
        unsafe {
            (*rt.get()).gc_heap_mut().set_opaque(rt_ptr.cast());
        }

        Ok(Self(rt))
    }

    #[cfg(debug_assertions)]
    pub const fn install_panic_handler(&self) {}

    #[cfg(not(debug_assertions))]
    pub fn install_panic_handler(&self) {
        let rt_ptr = self.deref() as *const RuntimeImpl as *mut RuntimeImpl;
        PANIC_RUNTIME.store(rt_ptr, Ordering::SeqCst);

        PANIC_HOOK.call_once(|| {
            // Chain any previously installed hook (e.g. the test262 runner's
            // "print the crashing script" hook) so it still runs after we do our
            // panic-time GC cleanup.
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if let Some(rt) = unsafe { PANIC_RUNTIME.load(Ordering::SeqCst).as_mut() } {
                    unsafe {
                        rt.panic_free_gc();
                    }
                }

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
    /// Note: `tick()` returns `Continue` only when real progress was made
    /// (a job finished with `Done`/`Err`, a tick handler reported progress, or
    /// a oneshot job was left queued). A polling job that merely returns `Again`
    /// does NOT keep the loop going — so this loop terminates naturally once all
    /// *settle-able* work is drained. Any polling job still spinning on `Again`
    /// is simply left in the queue and handed back to the next `tick()` caller
    /// (e.g. the main thread's event loop); it is never lost or interrupted.
    ///
    /// Consequently this function always terminates — it cannot busy-loop.
    pub fn drain_jobs(&mut self) {
        while self.tick() == TickStatus::Continue {}
    }
}

impl RuntimeImpl {
    unsafe fn panic_free_gc(&mut self) {
        self.cur_exception = Exception::default();
        self.interrupt_code = None;
        self.context_list.clear();
        self.shape_registry.clear();

        let pid = self.gc_part_id();
        if let Some(link) = self.gc_heap().finalize_partition(pid) {
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

impl RuntimePtr {
    #[inline(always)]
    pub const unsafe fn from_raw(rt: *mut RuntimeImpl) -> Self {
        debug_assert!(!rt.is_null());
        Self(unsafe { NonNull::new_unchecked(rt) })
    }
}
