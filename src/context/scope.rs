use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use gc_lite::{GcError, GcHead, GcNode, GcRef, GcScope, GcScopeState};

use crate::{Context, JSValue, context::ContextPtr};

use super::ContextImpl;

enum ScopeType<'a> {
    Owned(GcScope<'a>),
    Ref(&'a GcScopeState<'a>),
}

pub struct ScopedContext<'a> {
    pub(crate) ctx_ptr: ContextPtr,
    pub(crate) gc_scope: ScopeType<'a>,
}

#[cfg(debug_assertions)]
impl<'a> Drop for ScopedContext<'a> {
    fn drop(&mut self) {
        if let ScopeType::Owned(s) = &self.gc_scope {
            debug_assert_eq!(s.depth(), self.ctx_ptr.gc_heap().scope_max_depth());
        }
    }
}

impl<'a> Deref for ScopedContext<'a> {
    type Target = ContextImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.ctx_ptr.deref()
    }
}

impl<'a> DerefMut for ScopedContext<'a> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_ptr.deref_mut()
    }
}

impl<'a> From<ScopedContext<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: ScopedContext<'a>) -> Self {
        value.ctx_ptr
    }
}

impl<'a> From<&ScopedContext<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: &ScopedContext<'a>) -> Self {
        value.ctx_ptr
    }
}

impl<'a> From<&mut ScopedContext<'a>> for ContextPtr {
    #[inline(always)]
    fn from(value: &mut ScopedContext<'a>) -> Self {
        value.ctx_ptr
    }
}

impl<'a> From<ScopedContext<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: ScopedContext<'a>) -> Self {
        value.ctx_ptr.into()
    }
}

impl<'a> From<&ScopedContext<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: &ScopedContext<'a>) -> Self {
        value.ctx_ptr.into()
    }
}

impl<'a> From<&mut ScopedContext<'a>> for Context<'a> {
    #[inline(always)]
    fn from(value: &mut ScopedContext<'a>) -> Self {
        value.ctx_ptr.into()
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
    pub fn depth(&self) -> u8 {
        match &self.gc_scope {
            ScopeType::Owned(s) => s.depth(),
            ScopeType::Ref(s) => s.depth(),
        }
    }

    #[inline]
    fn gc(&self) -> &GcScopeState<'_> {
        match &self.gc_scope {
            ScopeType::Owned(s) => s.deref(),
            ScopeType::Ref(s) => s,
        }
    }

    /// Set promote value
    pub fn set_return_value(&mut self, val: JSValue) {
        let s = self.gc();
        debug_assert!(s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth());
        s.set_promote(val.gc_node_ptr());
    }

    /// flush protected nodes inside.
    pub fn flush(&self) {
        let s = self.gc();
        debug_assert!(s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth());
        s.flush();
    }

    /// test if given node contains in `self` scope
    #[cfg(debug_assertions)]
    pub fn contains(&self, node: NonNull<GcHead>) -> bool {
        self.gc_heap()
            .scope(self.depth() as _)
            .is_some_and(|s| s.contains(node))
    }

    /// create new scope over current scope
    #[inline]
    pub fn new_scope(&mut self) -> ScopedContext<'_> {
        let pid = self.rt_ref().gc_part_id();
        let ctx = ContextPtr::from(self.gc_ref());
        let gc_scope = self.gc_heap_mut().new_scope(pid);

        ScopedContext {
            ctx_ptr: ctx,
            gc_scope: ScopeType::Owned(gc_scope),
        }
    }

    pub fn with_new_scope<R>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        let s = self.new_scope();
        f(s)
    }
}

impl ContextImpl {
    #[deprecated]
    pub(crate) fn new_scope_legacy(&mut self) -> ScopedContext<'_> {
        let pid = self.rt_ref().gc_part_id();
        let ctx = ContextPtr::from(self.gc_ref());
        let gc_scope = self.gc_heap_mut().new_scope(pid);

        ScopedContext {
            ctx_ptr: ctx,
            gc_scope: ScopeType::Owned(gc_scope),
        }
    }

    pub fn current_scope(&self) -> Option<ScopedContext<'_>> {
        if let Some(s) = self.gc_heap().current_scope() {
            let sc = ScopedContext {
                ctx_ptr: self.gc_ref().into(),
                gc_scope: ScopeType::Ref(s),
            };
            Some(sc)
        } else {
            None
        }
    }

    pub fn with_current_scope<R>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> Option<R> {
        self.current_scope().map(f)
    }
}

impl<'c> Context<'c> {
    /// Create top-level context scope.
    /// This scope is closed automatically at ScopedContext is dropped.
    pub fn new_scope(&mut self) -> ScopedContext<'_> {
        let pid = self.rt_ref().gc_part_id();
        let ctx = ContextPtr::from(self.gc_ref());
        let gc_scope = self.gc_heap_mut().new_scope(pid);

        ScopedContext {
            ctx_ptr: ctx,
            gc_scope: ScopeType::Owned(gc_scope),
        }
    }

    pub fn with_new_scope<R>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        let s = self.new_scope();
        f(s)
    }
}
