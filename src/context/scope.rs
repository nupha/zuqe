#[cfg(debug_assertions)]
use std::ptr::NonNull;
use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use gc_lite::{GcError, GcHead, GcLocal, GcNode, GcRef};

use crate::{Context, context::ContextPtr};

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

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.ctx.deref()
    }
}

impl<'a> DerefMut for ContextScope<'a> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx.deref_mut()
    }
}

impl<'a> From<ContextScope<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: ContextScope<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> From<&ContextScope<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: &ContextScope<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> From<&mut ContextScope<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: &mut ContextScope<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> From<ContextScope<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: ContextScope<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> From<&ContextScope<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: &ContextScope<'a>) -> Self {
        value.ctx.into()
    }
}

impl<'a> From<&mut ContextScope<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: &mut ContextScope<'a>) -> Self {
        value.ctx.into()
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

    /// test if given node contains in `self` scope
    #[cfg(debug_assertions)]
    pub fn contains(&self, node: NonNull<GcHead>) -> bool {
        self.gc_heap()
            .scope(self.level() as _)
            .is_some_and(|s| s.contains(node))
    }

    #[cfg(debug_assertions)]
    pub const fn level(&self) -> u32 {
        self.level
    }
}

impl ContextImpl {
    /// create a sub scope
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
