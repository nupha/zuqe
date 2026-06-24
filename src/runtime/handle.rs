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

use gc_lite::{GcHeap, GcPartitionId};

use crate::{error::Exception, runtime::RuntimeImpl};

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

            // Register a host-provided full GC collect callback so that
            // arena-full allocations (in gc-lite `alloc_node_mem`) run the
            // complete GC cycle instead of the built-in ROOT/LOCAL-only
            // `garbage_collect`. This ensures host-side roots (shape registry,
            // contexts, stack frames, etc.) are traced, fixing the arena GC
            // path incompleteness (see bugs/parser-gc-atom-collect.md Bug 2).
            //
            // The callback recovers the `RuntimeImpl` pointer via the heap's
            // `opaque` slot (set above) and delegates to `gc_collect_in`.
            extern "C" fn arena_full_collect(
                heap: *mut GcHeap,
                pid: GcPartitionId,
            ) -> usize {
                let rt_ptr = unsafe { (*heap).opaque().cast::<RuntimeImpl>() };
                debug_assert!(!rt_ptr.is_null());
                unsafe { (*rt_ptr).gc_collect_in(pid) }
            }

            (*rt.get())
                .gc_heap_mut()
                .set_custom_gc_collect(Some(arena_full_collect));
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
            std::panic::set_hook(Box::new(|info| {
                if let Some(rt) = unsafe { PANIC_RUNTIME.load(Ordering::SeqCst).as_mut() } {
                    unsafe {
                        rt.panic_free_gc();
                    }
                }

                eprintln!("{info}");
            }));
        });
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
