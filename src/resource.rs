use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    thread::AccessError,
};

use once_cell::unsync::Lazy;

use crate::{atom::AtomRegistry, runtime::JSRuntime};

/// the thread local global resources
pub(crate) struct LocalResource {
    //
    // DON'T CHANGE FIELD ORDER. THIS IS THE ORDER OF DROP
    //
    /// list of active runtime raw pointers
    pub(super) runtime_slot: RefCell<Vec<Box<JSRuntime<'static>>>>,
    /// atom registry
    pub(crate) atom_registry: Lazy<RefCell<AtomRegistry>>,
}

#[cfg(debug_assertions)]
impl Drop for LocalResource {
    fn drop(&mut self) {
        println!("[O.o] LocalResource::drop()");
    }
}

impl LocalResource {
    pub(crate) fn with_runtimes<R>(f: impl FnOnce(&[Box<JSRuntime<'static>>]) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.runtime_slot.borrow().as_slice()))
    }

    pub(crate) fn with_runtimes_mut<R>(
        f: impl FnOnce(&mut Vec<Box<JSRuntime<'static>>>) -> R,
    ) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.runtime_slot.borrow_mut().deref_mut()))
    }

    pub(crate) fn try_with_runtimes<R>(
        f: impl FnOnce(&[Box<JSRuntime<'static>>]) -> R,
    ) -> Result<R, AccessError> {
        LOCAL_RESOURCE.try_with(|rcs| f(rcs.runtime_slot.borrow().as_slice()))
    }

    pub(crate) fn try_with_runtimes_mut<R>(
        f: impl FnOnce(&mut Vec<Box<JSRuntime<'static>>>) -> R,
    ) -> Result<R, AccessError> {
        LOCAL_RESOURCE.try_with(|rcs| f(rcs.runtime_slot.borrow_mut().deref_mut()))
    }

    pub(crate) fn with_atoms<R>(f: impl FnOnce(&AtomRegistry) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.atom_registry.borrow().deref()))
    }

    pub(crate) fn with_atoms_mut<R>(f: impl FnOnce(&mut AtomRegistry) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.atom_registry.borrow_mut().deref_mut()))
    }

    pub(crate) fn try_with_atoms<R>(f: impl FnOnce(&AtomRegistry) -> R) -> Result<R, AccessError> {
        LOCAL_RESOURCE.try_with(|rcs| f(rcs.atom_registry.borrow().deref()))
    }

    pub(crate) fn try_with_atoms_mut<R>(
        f: impl FnOnce(&mut AtomRegistry) -> R,
    ) -> Result<R, AccessError> {
        LOCAL_RESOURCE.try_with(|rcs| f(rcs.atom_registry.borrow_mut().deref_mut()))
    }
}

thread_local! {
    pub(crate) static LOCAL_RESOURCE: LocalResource = LocalResource {
        runtime_slot: RefCell::new(Vec::with_capacity(1)),
        atom_registry: Lazy::new(|| {
            let reg=AtomRegistry::new();
            RefCell::new(reg)
        })
    };
}
