// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use gc_lite::{GcError, GcHead, GcNode, GcRef, GcScope, GcScopeState};

use crate::{Context, JSValue, context::ContextPtr};

use super::ContextImpl;

#[repr(u8)]
#[derive(Debug)]
enum GcScopeType<'a> {
    Owned(GcScope<'a>),
    Ref(&'a GcScopeState<'a>),
}

#[derive(Debug)]
pub struct ScopedContext<'a> {
    pub(crate) ctx_ptr: ContextPtr,
    gc_scope: GcScopeType<'a>,
}

#[cfg(debug_assertions)]
impl<'a> Drop for ScopedContext<'a> {
    fn drop(&mut self) {
        if let GcScopeType::Owned(s) = &self.gc_scope {
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
        let r = self.gc().alloc_local(payload);

        if self.gc_heap().should_gc() {
            self.rt_mut().gc_req = true;
        }
        r
    }

    pub fn alloc_root<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc().alloc_root(payload);

        if self.gc_heap().should_gc() {
            self.rt_mut().gc_req = true;
        }
        r
    }

    /// get gc scope state
    #[inline]
    fn gc(&self) -> &GcScopeState<'_> {
        match &self.gc_scope {
            GcScopeType::Owned(s) => s.deref(),
            GcScopeType::Ref(s) => s,
        }
    }

    #[inline(always)]
    pub fn depth(&self) -> u8 {
        self.gc().depth()
    }

    /// Set promote value
    pub fn set_return_value(&mut self, val: JSValue) {
        let s = self.gc();
        debug_assert!(
            s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth(),
            "depth: {}, max_depth: {}",
            s.depth(),
            self.ctx_ptr.gc_heap().scope_max_depth(),
        );
        s.set_promote(val.gc_node_ptr());
    }

    /// flush protected nodes inside.
    pub fn flush(&self) {
        let s = self.gc();
        debug_assert!(s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth());
        s.flush();
    }

    /// create new scope over current scope
    fn new_scope(&mut self) -> ScopedContext<'_> {
        let pid = self.rt_ref().gc_part_id();
        let ctx = ContextPtr::from(self.gc_ref());
        let gc_scope = self.gc_heap_mut().new_scope(pid);

        ScopedContext {
            ctx_ptr: ctx,
            gc_scope: GcScopeType::Owned(gc_scope),
        }
    }

    /// run closure in a new sub scope.
    #[inline]
    pub fn with_new_scope<R>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        #[cfg(debug_assertions)]
        {
            let _d1 = self.depth();
            let s = self.new_scope();
            let _d2 = s.depth();
            let r = f(s);
            debug_assert_eq!(_d1, self.depth(), "{self:p}: {self:?}");
            r
        }
        #[cfg(not(debug_assertions))]
        {
            let s = self.new_scope();
            f(s)
        }
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
    /// get current (the inner-most) scope
    pub fn current_scope(&self) -> Option<ScopedContext<'_>> {
        if let Some(s) = self.gc_heap().current_scope() {
            let sc = ScopedContext {
                ctx_ptr: self.gc_ref().into(),
                gc_scope: GcScopeType::Ref(s),
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

impl ContextPtr {
    pub(crate) fn new_scope(&mut self) -> ScopedContext<'_> {
        let pid = self.rt_ref().gc_part_id();
        let ctx = ContextPtr::from(self.gc_ref());
        let gc_scope = self.gc_heap_mut().new_scope(pid);

        ScopedContext {
            ctx_ptr: ctx,
            gc_scope: GcScopeType::Owned(gc_scope),
        }
    }

    pub fn with_new_scope<R>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        let s = self.new_scope();
        f(s)
    }
}

impl<'c> Context<'c> {
    pub fn new_scope(&mut self) -> ScopedContext<'_> {
        let pid = self.rt_ref().gc_part_id();
        let ctx = ContextPtr::from(self.gc_ref());
        let gc_scope = self.gc_heap_mut().new_scope(pid);

        ScopedContext {
            ctx_ptr: ctx,
            gc_scope: GcScopeType::Owned(gc_scope),
        }
    }

    pub fn with_new_scope<R>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        let s = self.new_scope();
        f(s)
    }
}
