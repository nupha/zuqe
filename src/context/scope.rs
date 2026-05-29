// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use gc_lite::{GcError, GcHead, GcNode, GcRef, GcScope, GcScopeState};

use crate::{ExceptionText, JSValue, context::ContextPtr};

use super::ContextImpl;

/// Trait for types that can be safely returned from `with_new_scope`.
pub trait ScopeResult: Sized {
    /// `promote` is called with the **upstream scope** (the scope that called `with_new_scope`).
    /// It registers the returned GC node as a promote node in the upstream scope via
    /// `set_return_value`. When the upstream scope is later destroyed, `flush()` will
    /// move the node to its own upstream scope's cache, keeping it alive.
    ///
    /// This works because:
    /// - The downstream scope (created by `with_new_scope`) is about to be destroyed,
    ///   so its LOCAL protection is lost
    /// - The upstream scope is still alive, so registering the node there keeps it safe
    /// - When the upstream scope is eventually destroyed, the node is promoted further up
    ///
    /// Types containing `JSValue` automatically promote. Other types (like `()`, `i32`)
    /// are returned as-is since they have no GC nodes to protect.
    fn promote(self, upstream: &mut ScopedContext<'_>) -> Self;
}

impl<T: ScopeResult, E: ScopeResult> ScopeResult for Result<T, E> {
    fn promote(self, upstream: &mut ScopedContext<'_>) -> Self {
        match self {
            Ok(t) => Ok(t.promote(upstream)),
            Err(e) => Err(e.promote(upstream)),
        }
    }
}

impl<T: ScopeResult> ScopeResult for Option<T> {
    fn promote(self, upstream: &mut ScopedContext<'_>) -> Self {
        self.map(|t| t.promote(upstream))
    }
}

impl ScopeResult for JSValue {
    fn promote(self, upstream: &mut ScopedContext<'_>) -> Self {
        if self.gc_node_ptr().is_some() {
            upstream.set_return_value(self);
        }
        self
    }
}

macro_rules! impl_scope_result_noop {
    ($($t:ty),+ $(,)?) => {
        $(
            impl ScopeResult for $t {
                fn promote(self, _: &mut ScopedContext<'_>) -> Self { self }
            }
        )+
    };
}

impl_scope_result_noop!(
    (),
    i32,
    u32,
    usize,
    u8,
    bool,
    f64,
    &str,
    String,
    ExceptionText
);

#[repr(u8)]
#[derive(Debug)]
pub(crate) enum GcScopeType<'a> {
    Owned(GcScope<'a>),
    Ref(&'a GcScopeState<'a>),
}

#[derive(Debug)]
pub struct ScopedContext<'a> {
    pub(crate) ctx_ptr: ContextPtr,
    pub(crate) gc_scope: GcScopeType<'a>,
}

#[cfg(debug_assertions)]
impl<'a> Drop for ScopedContext<'a> {
    fn drop(&mut self) {
        if let GcScopeType::Owned(s) = &self.gc_scope {
            let stack_id = self.ctx_ptr.scope_stack_id();
            debug_assert_eq!(s.depth(), self.ctx_ptr.gc_heap().scope_max_depth(stack_id));
        }
    }
}

impl PartialEq for ScopedContext<'_> {
    fn eq(&self, other: &Self) -> bool {
        let gc = self.gc();
        self.ctx_ptr == other.ctx_ptr && gc.depth() == gc.depth() && gc.stack_id() == gc.stack_id()
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

impl<'a> ScopedContext<'a> {
    /// get gc scope state
    #[inline]
    pub(crate) fn gc(&self) -> &GcScopeState<'_> {
        match &self.gc_scope {
            GcScopeType::Owned(s) => s.deref(),
            GcScopeType::Ref(s) => s,
        }
    }

    #[inline(always)]
    pub fn depth(&self) -> u8 {
        self.gc().depth()
    }

    /// Allocate a local gc node in scope to protect it within scope lifetime.
    #[inline]
    pub fn alloc_local<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc().alloc_local(payload);
        if self.gc_heap().should_gc() {
            self.rt_mut().gc_req = true;
        }
        r
    }

    /// Promote `val` to upstream scope.
    /// Prefer using `ScopeResult` trait via `with_new_scope` instead of calling this directly.
    pub(crate) fn set_return_value(&mut self, val: JSValue) {
        let s = self.gc();
        let stack_id = self.ctx_ptr.scope_stack_id();
        debug_assert!(
            s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth(stack_id),
            "depth: {}, max_depth: {}",
            s.depth(),
            self.ctx_ptr.gc_heap().scope_max_depth(stack_id),
        );

        s.set_promote(val.gc_node_ptr());
    }

    /// flush protected nodes inside.
    pub fn flush(&self) {
        let s = self.gc();
        let stack_id = self.ctx_ptr.scope_stack_id();
        debug_assert!(s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth(stack_id));
        s.flush();
    }

    /// run closure in a new child scope.
    /// The return value is automatically promoted to the upstream scope
    /// via `ScopeResult::promote` before the child scope is destroyed.
    pub fn with_new_scope<R: ScopeResult>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        #[cfg(debug_assertions)]
        {
            let _d1 = self.depth();
            let s = self.ctx_ptr.new_scope();

            let ret_val = f(s);
            debug_assert_eq!(_d1, self.depth(), "{self:p}: {self:?}");
            ret_val.promote(self)
        }

        #[cfg(not(debug_assertions))]
        {
            let s = self.ctx_ptr.new_scope();
            let ret_val = f(s);
            ret_val.promote(self)
        }
    }

    /// test if given node contains in `self` scope
    #[cfg(debug_assertions)]
    pub fn contains(&self, node: NonNull<GcHead>) -> bool {
        let stack_id = self.ctx_ptr.scope_stack_id();
        self.gc_heap()
            .scope(stack_id, self.depth())
            .is_some_and(|s| s.contains(node))
    }
}

impl ContextPtr {
    /// push a new scope above current scope
    fn new_scope(&mut self) -> ScopedContext<'_> {
        let ctx_ptr = *self;
        let stack_id = self.scope_stack_id();
        let gc_scope = self.gc_heap_mut().new_scope(stack_id);

        ScopedContext {
            ctx_ptr,
            gc_scope: GcScopeType::Owned(gc_scope),
        }
    }

    /// Create a new scope, run closure in it, then destroy the scope.
    ///
    /// # Safety
    ///
    /// The return value may contain GC nodes that were allocated in the
    /// destroyed scope. After this call, those nodes lose LOCAL protection.
    /// The caller must ensure they are either:
    /// - Promoted to an upstream scope via `set_return_value` / `ScopeResult`
    /// - Used immediately before any GC cycle
    /// - Registered as GC roots
    ///
    /// When the new scope has a parent (i.e. this is not the root scope),
    /// the return value is automatically promoted to the parent scope via
    /// `ScopeResult::promote`. When there is no parent scope, the return
    /// value is returned as-is without promotion.
    pub(crate) unsafe fn with_new_scope_internal<R: ScopeResult>(
        &mut self,
        f: impl FnOnce(ScopedContext<'_>) -> R,
    ) -> R {
        #[cfg(debug_assertions)]
        let _d1 = self.current_scope().gc().depth();

        let s = self.new_scope();
        debug_assert!(
            s.gc().depth() > 1,
            "top level scope must exist, thus new scope must be greater than 1"
        );

        #[cfg(debug_assertions)]
        let _d2 = s.gc().depth();
        #[cfg(debug_assertions)]
        debug_assert_eq!(_d1, _d2 - 1);

        let ret = f(s);

        let mut curr = self.current_scope();
        #[cfg(debug_assertions)]
        debug_assert_eq!(_d1, curr.gc().depth());

        ret.promote(&mut curr)
    }
}

impl ContextImpl {
    /// get current (the inner-most) scope
    /// there always exists a top-level scope, so this method never returns `None`.
    pub(crate) fn current_scope(&self) -> ScopedContext<'_> {
        let stack_id = self.scope_stack_id();
        if let Some(s) = self.gc_heap().current_scope(stack_id) {
            ScopedContext {
                ctx_ptr: self.gc_ref().into(),
                gc_scope: GcScopeType::Ref(s),
            }
        } else {
            unreachable!()
        }
    }

    /// run closure with current (the inner-most) scope.
    /// there always exists a top-level scope, so this method never returns `None`.
    pub(crate) fn with_current_scope<R>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        f(self.current_scope())
    }
}

/// A context that always has a top-level GC scope.
///
/// Created by `RuntimeImpl::create_context`. Unlike `ScopedContext`, this type
/// is `Copy` and does **not** own a GC scope — the top-level scope is managed
/// by `RuntimeImpl`. Use `as_scoped()` or `with_new_scope()` to obtain a
/// `ScopedContext` for operations that require a scope.
///
/// `TopLevelContext` derefs to `ContextImpl`, so all context methods that
/// don't need a scope are available directly.
#[derive(Clone, Copy)]
pub struct TopLevelContext<'c> {
    pub(super) ctx_ptr: ContextPtr,
    pub(super) _marker: std::marker::PhantomData<&'c ()>,
}

impl<'c> Deref for TopLevelContext<'c> {
    type Target = ContextImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.ctx_ptr.deref()
    }
}

impl<'c> DerefMut for TopLevelContext<'c> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_ptr.deref_mut()
    }
}

impl<'c> From<TopLevelContext<'c>> for ContextPtr {
    #[inline(always)]
    fn from(value: TopLevelContext<'c>) -> Self {
        value.ctx_ptr
    }
}

impl<'c> From<&TopLevelContext<'c>> for ContextPtr {
    #[inline(always)]
    fn from(value: &TopLevelContext<'c>) -> Self {
        value.ctx_ptr
    }
}

impl<'c> From<&mut TopLevelContext<'c>> for ContextPtr {
    #[inline(always)]
    fn from(value: &mut TopLevelContext<'c>) -> Self {
        value.ctx_ptr
    }
}

impl<'c> TopLevelContext<'c> {
    /// Obtain a `ScopedContext` referencing the top-level GC scope.
    ///
    /// The top-level scope always exists (it is created by `create_context`
    /// and managed by `RuntimeImpl`), so this never returns `None`.
    pub fn as_scoped(&self) -> ScopedContext<'_> {
        let stack_id = self.ctx_ptr.scope_stack_id();
        let scope = self
            .ctx_ptr
            .gc_heap()
            .scope(stack_id, 1)
            .expect("top-level scope must exist");
        ScopedContext {
            ctx_ptr: self.ctx_ptr,
            gc_scope: GcScopeType::Ref(scope),
        }
    }

    /// Create a new child scope, run closure in it.
    ///
    /// Return value will be promoted to the top-level scope.
    pub fn with_new_scope<R: ScopeResult>(&mut self, f: impl FnOnce(ScopedContext<'_>) -> R) -> R {
        self.as_scoped().with_current_scope(f)
    }
}
