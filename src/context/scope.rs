// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ops::Deref, ptr::NonNull};

use gc_lite::{GcError, GcHead, GcNode, GcRef, GcScope, GcScopeStackId, GcScopeState};

use crate::{ExceptionText, JSValue, context::ContextPtr};

use super::ContextImpl;

/// A wrapper type that ensures the return value of `with_new_scope` is
/// properly returned via `ScopedContext::return_value`.
///
/// The only way to construct a `ScopeReturn` is through
/// `ScopedContext::return_value`, which moves GC nodes from the child
/// scope to the parent scope before the child scope is destroyed.
///
/// The `#[must_use]` attribute ensures the caller cannot accidentally
/// discard the return value.
#[must_use = "return value must be propagated from with_new_scope"]
pub struct ScopeReturn<R> {
    value: R,
}

impl<R> ScopeReturn<R> {
    const fn new(value: R) -> Self {
        Self { value }
    }

    pub fn into_inner(self) -> R {
        self.value
    }
}

/// Trait for types that can be safely returned from `with_new_scope`.
pub trait ScopeResult: Sized {
    /// Promote GC nodes from the child scope to the parent scope.
    ///
    /// Called by `ScopedContext::return_value` before the child scope is
    /// destroyed. This ensures GC nodes retain their LOCAL protection
    /// across scope boundaries.
    fn promote(self, scope: &ScopedContext<'_>) -> Self;
}

impl<T: ScopeResult, E: ScopeResult> ScopeResult for Result<T, E> {
    fn promote(self, scope: &ScopedContext<'_>) -> Self {
        match self {
            Ok(t) => Ok(t.promote(scope)),
            Err(e) => Err(e.promote(scope)),
        }
    }
}

impl<T: ScopeResult> ScopeResult for Option<T> {
    fn promote(self, scope: &ScopedContext<'_>) -> Self {
        self.map(|t| t.promote(scope))
    }
}

impl ScopeResult for JSValue {
    fn promote(self, scope: &ScopedContext<'_>) -> Self {
        if let Some(node) = self.gc_node_ptr()
            && let Some((parent, _)) = scope.gc().parent()
        {
            scope.gc().promote_node_to(node, parent);
        }
        self
    }
}

macro_rules! impl_scope_result_noop {
    ($($t:ty),+ $(,)?) => {
        $(
            impl ScopeResult for $t {
                fn promote(self, _: &ScopedContext<'_>) -> Self { self }
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

    // Note: Do NOT store `&'a GcScopeState<'a>` here. A direct reference
    // into the scope Vec would become dangling if the Vec is reallocated
    // (via `ContextPtr::new_scope` -> `GcHeap::new_scope`). This is not
    // caught by Rust's borrow checker because `ContextPtr` is `Copy` and
    // the scope access goes through `unsafe` code.
    //
    // Store `(stack_id, depth)` instead and resolve reference dynamically
    // via `ctx_ptr.gc_heap()` is safer.
    Ref { stack_id: GcScopeStackId, depth: u8 },
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

// SAFETY: ContextImpl is a LOCAL GC node (allocated via alloc_local).
// LOCAL nodes are reset to GRAY at the start of every GC cycle (mark_prepare),
// so they are never BLACK. Write barrier (bind/with_mut) is only needed when
// a BLACK master references a WHITE slave — since ContextImpl is never BLACK,
// it has no write barrier requirement. DerefMut is safe to use directly.
impl<'a> std::ops::DerefMut for ScopedContext<'a> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.ctx_ptr.gc_ref().as_ptr().as_ptr() }
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
            GcScopeType::Ref {
                stack_id, depth, ..
            } => self
                .ctx_ptr
                .gc_heap()
                .scope(*stack_id, *depth)
                .expect("Ref scope must exist"),
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

    /// Return a value from a `with_new_scope` closure.
    ///
    /// This method consumes the `ScopedContext`, ensuring the closure
    /// cannot use the scope after returning. Before the scope is destroyed,
    /// GC nodes in the return value are moved to the parent scope's cache,
    /// preserving their LOCAL protection.
    ///
    /// The returned `MustReturn<R>` wrapper (with `#[must_use]`) ensures
    /// the value is properly propagated through `with_new_scope`.
    #[inline]
    pub fn return_value<R: ScopeResult>(self, value: R) -> ScopeReturn<R> {
        let res = value.promote(&self);
        ScopeReturn::new(res)
    }

    /// clear protected nodes inside.
    pub(crate) fn clear(&self) {
        let s = self.gc();
        let stack_id = self.ctx_ptr.scope_stack_id();
        debug_assert!(s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth(stack_id));
        s.clear();
    }

    /// run closure in a new child scope.
    ///
    /// The closure receives a `ScopedContext` and must return its value
    /// via `scope.return_value(val)`. The `MustReturn<R>` wrapper (with
    /// `#[must_use]`) ensures the value is properly propagated.
    pub fn with_new_scope<R: ScopeResult>(
        &mut self,
        f: impl FnOnce(ScopedContext<'_>) -> ScopeReturn<R>,
    ) -> R {
        #[cfg(debug_assertions)]
        {
            let _d1 = self.depth();
            debug_assert_eq!(
                _d1,
                self.gc_heap().scope_max_depth(self.scope_stack_id()),
                "with_new_scope: self (depth={}) must be the innermost scope (max_depth={})",
                _d1,
                self.gc_heap().scope_max_depth(self.scope_stack_id()),
            );
            let s = self.ctx_ptr.new_scope();

            let ret_val = f(s);
            debug_assert_eq!(_d1, self.depth(), "{self:p}: {self:?}");
            ret_val.into_inner()
        }

        #[cfg(not(debug_assertions))]
        {
            let s = self.ctx_ptr.new_scope();
            let ret_val = f(s);
            ret_val.into_inner()
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
    /// The closure receives a `ScopedContext` and must return its value
    /// via `scope.return_value(val)`. Before the scope is destroyed,
    /// GC nodes in the return value are moved to the parent scope's cache,
    /// preserving their LOCAL protection.
    ///
    /// # Safety
    ///
    /// The caller must ensure the context is in a valid state for creating
    /// a new scope (i.e., there is a current scope to serve as parent).
    pub(crate) fn with_new_scope_internal<R: ScopeResult>(
        &mut self,
        f: impl FnOnce(ScopedContext<'_>) -> ScopeReturn<R>,
    ) -> R {
        #[cfg(debug_assertions)]
        let _d1 = self.current_scope().gc().depth();
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            _d1,
            self.gc_heap().scope_max_depth(self.scope_stack_id()),
            "with_new_scope_internal: current scope (depth={}) must be innermost (max_depth={})",
            _d1,
            self.gc_heap().scope_max_depth(self.scope_stack_id()),
        );

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
        ret.into_inner()
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
                gc_scope: GcScopeType::Ref {
                    stack_id,
                    depth: s.depth(),
                },
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

/// A context that represents top-level GC scope.
///
/// Created by `RuntimeImpl::create_context`. Unlike `ScopedContext`, this type
/// is `Copy` and does **not** own a GC scope — the top-level scope is managed
/// by `RuntimeImpl`. Use `as_scoped()` or `with_new_scope()` to obtain a
/// `ScopedContext` for operations that require a scope.
///
/// `TopLevelContext` derefs to `ContextImpl`, so all context methods that
/// don't need a scope are available directly.
pub struct Context<'c> {
    pub(super) ctx_ptr: ContextPtr,
    pub(super) _marker: std::marker::PhantomData<&'c ()>,
}

impl<'c> Deref for Context<'c> {
    type Target = ContextImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.ctx_ptr.deref()
    }
}

// SAFETY: ContextImpl is a LOCAL GC node (allocated via alloc_local).
// LOCAL nodes are reset to GRAY at the start of every GC cycle (mark_prepare),
// so they are never BLACK. Write barrier (bind/with_mut) is only needed when
// a BLACK master references a WHITE slave — since ContextImpl is never BLACK,
// it has no write barrier requirement. DerefMut is safe to use directly.
impl<'c> std::ops::DerefMut for Context<'c> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.ctx_ptr.gc_ref().as_ptr().as_ptr() }
    }
}

impl<'c> From<Context<'c>> for ContextPtr {
    #[inline(always)]
    fn from(value: Context<'c>) -> Self {
        value.ctx_ptr
    }
}

impl<'c> From<&Context<'c>> for ContextPtr {
    #[inline(always)]
    fn from(value: &Context<'c>) -> Self {
        value.ctx_ptr
    }
}

impl<'c> From<&mut Context<'c>> for ContextPtr {
    #[inline(always)]
    fn from(value: &mut Context<'c>) -> Self {
        value.ctx_ptr
    }
}

impl<'c> Context<'c> {
    /// Obtain a `ScopedContext` representing the top-level GC scope.
    pub fn root_scope(&self) -> ScopedContext<'_> {
        let stack_id = self.ctx_ptr.scope_stack_id();
        ScopedContext {
            ctx_ptr: self.ctx_ptr,
            gc_scope: GcScopeType::Ref { stack_id, depth: 1 },
        }
    }

    /// Create a child scope, run closure in it.
    pub fn with_new_scope<R: ScopeResult>(
        &mut self,
        f: impl FnOnce(ScopedContext<'_>) -> ScopeReturn<R>,
    ) -> R {
        self.root_scope().with_new_scope(f)
    }
}
