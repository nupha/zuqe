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

use gc_lite::GcHeap;

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
