use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use gc_lite::{GcError, GcHead, GcNode, GcRef, GcScope};

use crate::{Context, JSValue, context::ContextPtr};

use super::ContextImpl;

pub struct ScopedContext<'a> {
    pub(crate) ctx: ContextPtr,
    pub(crate) depth: u8,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Drop for ScopedContext<'a> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        debug_assert_eq!(self.depth(), self.ctx.gc_heap().scope_max_depth());

        self.ctx.gc_heap_mut().pop_gc_scope();
    }
}

impl<'a> Deref for ScopedContext<'a> {
    type Target = ContextImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.ctx.deref()
    }
}

impl<'a> DerefMut for ScopedContext<'a> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx.deref_mut()
    }
}

impl<'a> From<ScopedContext<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: ScopedContext<'a>) -> Self {
        value.ctx
    }
}

impl<'a> From<&ScopedContext<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: &ScopedContext<'a>) -> Self {
        value.ctx
    }
}

impl<'a> From<&mut ScopedContext<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: &mut ScopedContext<'a>) -> Self {
        value.ctx
    }
}

impl<'a> From<ScopedContext<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: ScopedContext<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> From<&ScopedContext<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: &ScopedContext<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> From<&mut ScopedContext<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: &mut ScopedContext<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> ScopedContext<'a> {
    /// Allocate a node, and put it to scope cache to protect it.
    pub fn alloc_local<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc_local(payload))
            .unwrap()
    }

    pub fn alloc_root<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc_root(payload))
            .unwrap()
    }

    #[inline(always)]
    pub const fn depth(&self) -> u8 {
        self.depth
    }

    /// get gc scope
    pub(crate) fn gc(&self) -> &GcScope<'_> {
        debug_assert!(self.depth() <= self.ctx.gc_heap().scope_max_depth());
        self.ctx.gc_heap().scope(self.depth).unwrap()
    }

    /// Set promote value before scope dropping, after call this, scope is dropped.
    /// That means a scope can only promote value once.
    pub fn set_return_value(self, val: JSValue) {
        self.gc().set_promote(val.gc_node_ptr());
    }

    /// flush protected nodes inside.
    pub fn flush(&self) {
        self.gc().flush();
    }

    /// test if given node contains in `self` scope
    #[cfg(debug_assertions)]
    pub fn contains(&self, node: NonNull<GcHead>) -> bool {
        self.gc_heap()
            .scope(self.depth() as _)
            .is_some_and(|s| s.contains(node))
    }
}

impl ContextImpl {
    /// Create a stacked context sub scope.
    /// This scope is closed automatically at ScopedContext is dropped.
    pub fn new_scope(&mut self) -> ScopedContext<'_> {
        let pid = self.rt_ref().gc_part_id();
        let ctx = ContextPtr::from_ref(self);
        let s = self.gc_heap_mut().push_gc_scope(pid);

        ScopedContext {
            ctx,
            depth: s.depth(),
            _mark: PhantomData,
        }
    }
}
