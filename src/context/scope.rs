// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ops::Deref, ptr::NonNull};

use gc_lite::{GcError, GcHead, GcNode, GcRef, GcScope, GcScopeStackId, GcScopeState};

use crate::{ExceptionText, JSValue, context::ContextPtr};

use super::ContextImpl;

/// Trait for types that can be safely returned from `with_new_scope`.
pub trait ScopeResult: Sized {
    /// Promote GC nodes from the child scope to the parent scope.
    ///
    /// Called by `ScopedContext::return_with` before the child scope is
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
        if let Some(node) = self.as_gc_node()
            && let Some((parent, _)) = scope.gc_state().parent()
        {
            scope.gc_state().promote_node_to(node, parent);
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
    // Store `(stack_id, index)` instead and resolve reference dynamically
    // via `ctx_ptr.gc_heap()`. `index` is 0-based (index = depth - 1),
    // allowing direct `get_unchecked` access without the `depth - 1` conversion.
    Ref {
        stack_id: GcScopeStackId,
        index: u32,
    },
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
        let gc = self.gc_state();
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
    #[allow(unused_unsafe)] // debug_assert may trigger unused unsafe warning
    pub(crate) fn gc_state(&self) -> &GcScopeState<'_> {
        match &self.gc_scope {
            GcScopeType::Owned(s) => s.deref(),
            GcScopeType::Ref {
                stack_id, index, ..
            } => unsafe {
                // SAFETY: `index` is constructed as `depth - 1` by `current_scope()`,
                // ensuring it is always a valid 0-based index into the scope Vec.
                self.ctx_ptr.gc_heap().scope_unchecked(*stack_id, *index)
            },
        }
    }

    #[inline(always)]
    pub fn depth(&self) -> u32 {
        self.gc_state().depth()
    }

    /// Allocate a local gc node in scope to protect it within scope lifetime.
    #[inline]
    pub fn alloc_local<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc_state().alloc_local(payload).map(|gc| gc.into_raw());

        let rt = self.rt_mut();
        rt.gc_alloc_count += 1;
        if rt.gc_alloc_count >= rt.gc_alloc_check_interval {
            rt.gc_alloc_count = 0;
            if rt.gc_heap.should_gc() {
                rt.gc_req = true;
            }
        }

        r
    }

    /// remove the 'node' from scope protection.
    pub fn remove_local(&mut self, node: NonNull<GcHead>) {
        self.gc_state().remove_node(node);
    }

    /// clear all protected nodes in scope.
    pub fn clear_locals(&self) {
        let s = self.gc_state();
        let stack_id = self.ctx_ptr.scope_stack_id();
        debug_assert!(s.depth() <= self.ctx_ptr.gc_heap().scope_max_depth(stack_id));
        s.clear();
    }

    /// Run `f` with `active_frame_index` protected: the current value is
    /// saved before the closure runs and restored after it returns.
    ///
    /// `execute_trampoline` clears `active_frame_index` on exit, so every
    /// caller must restore the outer frame — and some call paths bypass
    /// `call_internal`'s protection (e.g. `ObjectIter::apply_next`'s
    /// IteratorNext shortcut for generator `.next()` in for-of). Manual
    /// save/call/restore is easy to break during maintenance; wrapping the
    /// call in this guard makes the restore impossible to forget.
    #[inline(always)]
    pub(crate) fn with_active_frame_guard<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved_frame = self.rt().active_frame_index;
        let r = f(self);
        self.rt_mut().active_frame_index = saved_frame;
        r
    }

    /// run closure in a new child scope.
    ///
    /// The closure receives a `&mut ScopedContext` and returns a value directly.
    /// `promote` is called automatically inside `with_new_scope` to move GC nodes
    /// from the child scope to the parent scope before the child scope is destroyed.
    pub fn with_new_scope<R: ScopeResult>(
        &mut self,
        f: impl FnOnce(&mut ScopedContext<'_>) -> R,
    ) -> R {
        #[cfg(debug_assertions)]
        {
            let _d1 = self.depth();
            let max_depth = self.gc_heap().scope_max_depth(self.scope_stack_id());
            debug_assert_eq!(
                _d1, max_depth,
                "with_new_scope: self (depth={}) must be the innermost scope (max_depth={})",
                _d1, max_depth,
            );
            let result = {
                let mut s = self.ctx_ptr.new_scope();
                let r = f(&mut s);
                let r = r.promote(&s);
                // s drops here → releases mutable borrow on self.ctx_ptr
                r
            };
            let d2 = self.depth();
            debug_assert_eq!(_d1, d2, "{self:p}: {self:?}");
            result
        }

        #[cfg(not(debug_assertions))]
        {
            let mut s = self.ctx_ptr.new_scope();
            let result = f(&mut s);
            let result = result.promote(&s);
            result
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
    pub(crate) fn new_scope(&mut self) -> ScopedContext<'_> {
        let ctx_ptr = *self;
        let stack_id = self.scope_stack_id();
        let gc_scope = self.gc_heap_mut().new_scope(stack_id);

        ScopedContext {
            ctx_ptr,
            gc_scope: GcScopeType::Owned(gc_scope),
        }
    }

    /// Create a child scope above the current (innermost) scope, run closure in it.
    pub(crate) fn with_new_scope<R: ScopeResult>(
        &mut self,
        f: impl FnOnce(&mut ScopedContext<'_>) -> R,
    ) -> R {
        let mut s = self.current_scope();
        s.with_new_scope(f)
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
                    index: s.depth() - 1,
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
