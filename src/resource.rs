use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    thread::AccessError,
};

use gc_lite::{Collect, GcError, GcHeap, GcRef, PartitionId};
use once_cell::unsync::Lazy;

use crate::{atom::AtomRegistry, runtime::RuntimeImpl};

/// the thread local global resources
pub(crate) struct LocalResource {
    //
    // DON'T CHANGE FIELD ORDER. THIS IS THE ORDER OF DROP
    //
    /// list of active runtime raw pointers
    #[allow(clippy::vec_box)]
    pub(super) runtime_slot: RefCell<Vec<Box<RuntimeImpl>>>,

    /// atom registry
    pub(crate) atom_registry: Lazy<RefCell<AtomRegistry>>,

    /// gc heap
    pub(crate) gc_heap: RefCell<GcHeap>,
    /// gc default parition
    pub(crate) gc_default_partition: PartitionId,
}

impl Drop for LocalResource {
    fn drop(&mut self) {
        self.gc_heap
            .borrow_mut()
            .remove_partition(self.gc_default_partition);
    }
}

impl LocalResource {
    fn new() -> Self {
        let mut heap = GcHeap::new();
        let p0 = heap.create_partition("default".to_string(), None);

        Self {
            runtime_slot: RefCell::new(Vec::with_capacity(1)),
            atom_registry: Lazy::new(|| {
                let reg = AtomRegistry::new();
                RefCell::new(reg)
            }),
            gc_heap: RefCell::new(heap),
            gc_default_partition: p0,
        }
    }

    /// alloc a gc object to partition. Partion None means use default partition
    pub(crate) fn gc_alloc<T: Collect>(
        part_id: Option<PartitionId>,
        obj: T,
    ) -> Result<GcRef<T>, (GcError, T)> {
        LOCAL_RESOURCE.with(|rcs| {
            let mut heap = rcs.gc_heap.borrow_mut();

            let gc_ref = heap.alloc(part_id.unwrap_or(rcs.gc_default_partition), obj)?;
            // everything allocated in static heap is root obj
            heap.set_root(gc_ref, true);
            Ok(gc_ref)
        })
    }

    pub(crate) fn with_static_gc_heap<R>(f: impl FnOnce(&mut GcHeap) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.gc_heap.borrow_mut().deref_mut()))
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

    pub(crate) fn with_atoms<R>(f: impl FnOnce(&AtomRegistry) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.atom_registry.borrow().deref()))
    }

    pub(crate) fn with_atoms_mut<R>(f: impl FnOnce(&mut AtomRegistry) -> R) -> R {
        LOCAL_RESOURCE.with(|rcs| f(rcs.atom_registry.borrow_mut().deref_mut()))
    }

    pub(crate) fn try_with_atoms_mut<R>(
        f: impl FnOnce(&mut AtomRegistry) -> R,
    ) -> Result<R, AccessError> {
        LOCAL_RESOURCE.try_with(|rcs| f(rcs.atom_registry.borrow_mut().deref_mut()))
    }
}

thread_local! {
    static LOCAL_RESOURCE: LocalResource = LocalResource::new();
}
