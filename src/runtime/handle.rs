use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use crate::{resource::LocalResource, runtime::JSRuntime};

/// runtime handle is a light-weight reference to runtime.
/// it's equivelent to runtime pointer
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct RuntimeHandle<'h>(NonNull<JSRuntime<'h>>);

impl<'h> Deref for RuntimeHandle<'h> {
    type Target = JSRuntime<'h>;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<'h> DerefMut for RuntimeHandle<'h> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl<'h> From<&JSRuntime<'h>> for RuntimeHandle<'h> {
    fn from(value: &JSRuntime<'h>) -> Self {
        Self(NonNull::from_ref(value))
    }
}

impl<'h> RuntimeHandle<'h> {
    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.as_ptr() as usize
    }

    #[inline(always)]
    pub(crate) const fn as_ptr(self) -> *mut JSRuntime<'h> {
        self.0.as_ptr()
    }

    #[inline(always)]
    pub const fn as_ref(self) -> &'h JSRuntime<'h> {
        unsafe { self.0.as_ref() }
    }

    /// Manually drop a runtime by its handle.
    ///
    /// If a runtime is not freed manually, it will be dropped automatically when thread exits.
    /// you choose to call this method because you want the runtime be dropped now, not till thread exit.
    ///
    /// ### SAFETY
    /// After calling this method, any accessing to the related `RuntimeHandle` of this runtime
    /// may cause UNDEFINED BEHAVIOUR!
    pub fn free(handle: Self) {
        let this_id = handle.id();

        let rt = LocalResource::with_runtimes_mut(|slot| {
            slot.iter()
                .position(|r| r.id() == this_id)
                .map(|i| slot.swap_remove(i))
        });
        drop(rt);
    }
}

impl RuntimeHandle<'static> {
    pub fn active_list() -> Vec<Self> {
        LocalResource::with_runtimes(|slot| slot.iter().map(|x| x.handle()).collect::<Vec<_>>())
    }
}

impl<'r> JSRuntime<'r> {
    /// get handle of runtime
    pub fn handle(&self) -> RuntimeHandle<'r> {
        RuntimeHandle::from(self)
    }
}
