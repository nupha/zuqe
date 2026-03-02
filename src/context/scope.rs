use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use gc_lite::{GcError, GcLocal, GcNode, GcRef};

use super::ContextImpl;

pub struct ContextScope<'a> {
    pub(crate) ctx: GcRef<ContextImpl>,

    #[cfg(debug_assertions)]
    level: u32,

    _mark: PhantomData<&'a ()>,
}

impl<'a> Drop for ContextScope<'a> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        debug_assert_eq!(self.level, self.ctx.gc_heap().scope_level() as u32);

        self.ctx.gc_heap_mut().pop_gc_scope();
    }
}

impl<'a> Deref for ContextScope<'a> {
    type Target = ContextImpl;

    fn deref(&self) -> &Self::Target {
        self.ctx.deref()
    }
}

impl<'a> DerefMut for ContextScope<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx.deref_mut()
    }
}

impl<'a> ContextScope<'a> {
    /// Allocate a node, and put it to scope cache to protect it.
    pub fn alloc<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc(payload))
            .unwrap()
    }

    pub fn alloc_root<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc_root(payload))
            .unwrap()
    }

    #[deprecated]
    pub fn alloc_local<T: GcNode>(&mut self, payload: T) -> Result<GcLocal<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc_local(payload))
            .unwrap()
    }
}

impl ContextImpl {
    /// create a context scope
    pub fn new_scope(&mut self) -> ContextScope<'_> {
        let pid = self.rt_ref().gc_part_id();
        self.gc_heap_mut().push_gc_scope(pid);

        ContextScope {
            ctx: self.gc_ref(),

            #[cfg(debug_assertions)]
            level: self.gc_heap().scope_level() as u32,

            _mark: PhantomData,
        }
    }
}
