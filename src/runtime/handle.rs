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
    /// create runtime
    pub fn create() -> Result<Runtime, i32> {
        let rt: Box<RuntimeImpl> = RuntimeImpl::new()?;
        let h = Runtime::from(rt.as_ref());

        // register runtime
        LocalResource::with_runtimes_mut(|slot| {
            slot.push(rt);
        });

        Ok(h)
    }

    fn from(inner: &RuntimeImpl) -> Self {
        Self(Rc::new(NonNull::from_ref(inner)))
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
