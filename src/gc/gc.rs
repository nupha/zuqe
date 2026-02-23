// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{
    GcError, GcHeap, GcNode, GcPartition, GcPartitionId, GcRef, GcTracable, GcTraceCtx,
    gc_type_register,
};

use crate::{
    context::ContextImpl,
    function::{AsyncStateImpl, BytecodeImpl, VarRefImpl},
    module::ModuleDefImpl,
    object::{ObjectImpl, ShapeImpl},
    runtime::RuntimeImpl,
    string::StringImpl,
};

// register gc data types
gc_type_register! {
    ContextImpl;
    ObjectImpl;
    ModuleDefImpl;
    VarRefImpl;
    BytecodeImpl;
    StringImpl;
    AsyncStateImpl;
    ShapeImpl, drop_pass = 1;
}

impl RuntimeImpl {
    #[inline(always)]
    pub(crate) const fn gc_heap(&self) -> &GcHeap {
        &self.gc_heap
    }

    #[inline(always)]
    pub(crate) const fn gc_heap_mut(&mut self) -> &mut GcHeap {
        &mut self.gc_heap
    }

    /// get gc top-level scope
    #[inline]
    pub(crate) fn gc_top_scope(&self) -> &GcPartition {
        self.gc_heap().partition(self.gc_top_scope).unwrap()
    }

    /// top gc partition for runtime
    #[inline]
    pub(crate) fn gc_top_scope_mut(&mut self) -> &mut GcPartition {
        let pid = self.gc_top_scope;
        self.gc_heap_mut().partition_mut(pid).unwrap()
    }

    /// Get gc scope of current running state.
    /// if current stack frame does not have gc scope and auto_init=true, then initialize and return.
    /// if not curent stack frame, returns root scope.
    pub(crate) fn gc_current_scope(&mut self, auto_init: bool) -> Option<GcPartitionId> {
        if let Some(sf) = self.current_stack_frame_mut() {
            if !sf.gc_scope_id.is_null() {
                Some(sf.gc_scope_id)
            } else if auto_init {
                Some(sf.init_gc_scope())
            } else {
                None
            }
        } else {
            Some(self.gc_top_scope)
        }
    }

    /// get upper gc scope.
    /// if parent stack frame does not have gc scope and auto_init=true, then initialize scopes.
    /// if no current stack frame, returns root scope.
    pub(crate) fn gc_upper_scope(&mut self, auto_init: bool) -> Option<GcPartitionId> {
        if let Some(up) = self
            .current_stack_frame_mut()
            .and_then(|sf| sf.parent_mut())
        {
            if !up.gc_scope_id.is_null() {
                Some(up.gc_scope_id)
            } else if auto_init {
                Some(up.init_gc_scope())
            } else {
                None
            }
        } else {
            Some(self.gc_top_scope)
        }
    }

    /// alloc data in given gc scope
    pub(crate) fn gc_alloc_in<T: GcNode>(
        &mut self,
        scope_id: GcPartitionId,
        data: T,
    ) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc_heap_mut().alloc(scope_id, data)?;

        // trace data's children nodes to mark possible cross reference
        // for n in r.node_info().gc_children(self.gc_heap()) {
        //     self.gc_heap_mut().set_xref(scope_id, n);
        // }

        if self.gc_top_scope().should_gc() {
            self.gc_req = true;
        }
        Ok(r)
    }

    #[inline]
    pub fn gc_alloc<T: GcNode>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        let pid = self.gc_current_scope(true).unwrap();
        self.gc_alloc_in(pid, data)
    }

    /// collect garbage
    pub(crate) fn gc_collect_in(&mut self, partition_id: GcPartitionId) -> usize {
        log::trace!("[collect] {partition_id:?}");

        // 1. mark
        {
            for mut n in self.gc_heap_mut().nodes(partition_id) {
                unsafe { n.as_mut().reset_color() }
            }
            let mut gcx = GcTraceCtx::new(self.gc_heap_mut(), false);

            // 1. scope root nodes (Hint: for root scope, Contexts are root nodes)
            gcx.trace_roots(partition_id);
            // 2. stack frame
            if let Some(sf) = self.current_stack_frame() {
                debug_assert_eq!(sf.gc_scope_id, partition_id);
                sf.trace(&mut gcx);
            }
            // 3. exception
            if let Some(n) = self.cur_exception.gc_node_ptr() {
                gcx.trace(n);
            }

            // user defined mark
            if let Some(f) = self.gc_user_trace.as_ref() {
                f(&mut gcx);
            }
            // mark currently running job
            if let Some(f) = self.gc_cur_job_trace.as_ref() {
                f(&mut gcx);
            }

            gcx.commit();
        }

        // 2. sweep
        let freed = self
            .gc_heap_mut()
            .sweep(partition_id, GcHeap::SWEEP_UNMARKED_FUNC, {
                #[cfg(debug_assertions)]
                {
                    |_, n| {
                        log::debug!("[sweep][{partition_id:?}] purge {n:?}");
                    }
                }
                #[cfg(not(debug_assertions))]
                GcHeap::DUMMY_DISPOSE_CALLBACK
            });

        if freed != 0 {
            // update gc threshold
            let used = self.gc_top_scope().memory_used();
            self.gc_top_scope_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of
        }

        log::trace!("[collect] {partition_id:?} free {freed} bytes");

        self.gc_req = false;
        freed
    }

    #[inline]
    pub(crate) fn gc_collect(&mut self) {
        if let Some(p) = self.gc_current_scope(false) {
            self.gc_collect_in(p);
        }
    }

    #[inline(always)]
    pub fn gc_set_trace_handler(&mut self, f: impl Fn(&mut GcTraceCtx) + 'static) {
        self.gc_user_trace.replace(Box::new(f));
    }

    /// request gc collect
    #[inline(always)]
    pub const fn gc_request(&mut self) {
        self.gc_req = true;
    }

    /// request gc collect
    #[inline(always)]
    pub const fn gc_has_request(&self) -> bool {
        self.gc_req
    }

    pub fn gc_memory_limit(&self) -> usize {
        self.gc_top_scope().memory_limit()
    }

    pub fn gc_set_memory_limit(&mut self, limit: usize) -> usize {
        self.gc_top_scope_mut().set_memory_limit(limit)
    }

    pub fn gc_set_threshold(&mut self, threshold: usize) {
        self.gc_heap.set_gc_threshold(self.gc_top_scope, threshold);
    }
}
