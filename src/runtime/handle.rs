use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
    rc::Rc,
};

use crate::{resource::LocalResource, runtime::RuntimeImpl};

/// A ref counted handle to runtime.
/// When last handle dropped, associated runtime will be dropped.
#[derive(Debug, Clone, PartialEq)]
#[repr(transparent)]
pub struct Runtime(Rc<NonNull<RuntimeImpl>>);

impl Drop for Runtime {
    fn drop(&mut self) {
        let this_id = self.id();

        // unregister runtime
        let _rt = LocalResource::with_runtimes_mut(|slot| {
            slot.iter()
                .position(|r| r.id() == this_id)
                .map(|i| slot.swap_remove(i))
        });

        #[cfg(debug_assertions)]
        if let Some(r) = _rt.as_ref() {
            debug_assert_eq!(this_id, r.id());
        }

        // _rt will be dropped here
    }
}

impl Deref for Runtime {
    type Target = RuntimeImpl;

    fn deref(&self) -> &Self::Target {
        unsafe { (*self.0).as_ref() }
    }
}

impl DerefMut for Runtime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let mut p = *self.0;
        unsafe { p.as_mut() }
    }
}

impl Runtime {
    pub(super) fn new(value: &RuntimeImpl) -> Self {
        Self(Rc::new(NonNull::from_ref(value)))
    }
}

/// A light-weight runtime raw ptr.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct RuntimeRef(NonNull<RuntimeImpl>);

impl Deref for RuntimeRef {
    type Target = RuntimeImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for RuntimeRef {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl From<&RuntimeImpl> for RuntimeRef {
    #[inline(always)]
    fn from(value: &RuntimeImpl) -> Self {
        Self(NonNull::from_ref(value))
    }
}

impl RuntimeRef {
    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.as_ptr() as usize
    }

    #[inline(always)]
    pub const fn as_ref(&self) -> &RuntimeImpl {
        unsafe { self.0.as_ref() }
    }
}
