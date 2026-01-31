use std::{cell::RefCell, ops::DerefMut, thread::AccessError};

use crate::runtime::RuntimeImpl;

/// the thread local global resources
pub(crate) struct LocalResource {
    /// list of active runtime raw pointers
    #[allow(clippy::vec_box)]
    pub(super) runtime_slot: RefCell<Vec<Box<RuntimeImpl>>>,
}

impl LocalResource {
    fn new() -> Self {
        Self {
            runtime_slot: RefCell::new(Vec::with_capacity(1)),
        }
    }

    pub(crate) fn with_runtimes<R>(f: impl FnOnce(&[Box<RuntimeImpl>]) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.runtime_slot.borrow().as_slice()))
    }

    pub(crate) fn with_runtimes_mut<R>(f: impl FnOnce(&mut Vec<Box<RuntimeImpl>>) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.runtime_slot.borrow_mut().deref_mut()))
    }

    pub(crate) fn try_with_runtimes<R>(
        f: impl FnOnce(&[Box<RuntimeImpl>]) -> R,
    ) -> Result<R, AccessError> {
        LOCAL_RESOURCE.try_with(|rcs| f(rcs.runtime_slot.borrow().as_slice()))
    }

    pub(crate) fn try_with_runtimes_mut<R>(
        f: impl FnOnce(&mut Vec<Box<RuntimeImpl>>) -> R,
    ) -> Result<R, AccessError> {
        LOCAL_RESOURCE.try_with(|rcs| f(rcs.runtime_slot.borrow_mut().deref_mut()))
    }
}

thread_local! {
    static LOCAL_RESOURCE: LocalResource = LocalResource::new();
}
