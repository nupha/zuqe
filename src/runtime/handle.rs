use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
    rc::Rc,
};

use crate::{resource::LocalResource, runtime::JSRuntime};

/// A ref counted handle to runtime.
/// When last handle dropped, associated runtime will be dropped.
#[derive(Debug, Clone, PartialEq)]
#[repr(transparent)]
pub struct RuntimeHandle<'h>(Rc<NonNull<JSRuntime<'h>>>);

impl<'h> Drop for RuntimeHandle<'h> {
    fn drop(&mut self) {
        let this_id = self.id();

        // unregister runtime
        let _rt = LocalResource::with_runtimes_mut(|slot| {
            slot.iter().position(|r| r.id() == this_id).map(|i| {
                println!("[O.o] drop idx={i}");
                slot.swap_remove(i)
            })
        });

        #[cfg(debug_assertions)]
        if let Some(r) = _rt.as_ref() {
            debug_assert_eq!(this_id, r.id());
        }

        // rt will be dropped here
    }
}

impl<'h> Deref for RuntimeHandle<'h> {
    type Target = JSRuntime<'h>;

    fn deref(&self) -> &Self::Target {
        unsafe { (*self.0).as_ref() }
    }
}

impl<'h> DerefMut for RuntimeHandle<'h> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let mut p = *self.0;
        unsafe { p.as_mut() }
    }
}

impl<'h> RuntimeHandle<'h> {
    pub(super) fn new(value: &JSRuntime<'h>) -> Self {
        Self(Rc::new(NonNull::from_ref(value)))
    }
}

/// A light-weight runtime raw ptr.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct RuntimePtr<'h>(NonNull<JSRuntime<'h>>);

impl<'h> Deref for RuntimePtr<'h> {
    type Target = JSRuntime<'h>;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<'h> DerefMut for RuntimePtr<'h> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl<'h> From<&JSRuntime<'h>> for RuntimePtr<'h> {
    fn from(value: &JSRuntime<'h>) -> Self {
        Self(NonNull::from_ref(value))
    }
}

impl<'h> RuntimePtr<'h> {
    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.as_ptr() as usize
    }

    #[inline(always)]
    pub const fn as_ref(&self) -> &'h JSRuntime<'h> {
        unsafe { self.0.as_ref() }
    }
}
