// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{
    GcError, GcHeap, GcNode, GcPartition, GcPartitionId, GcRef, GcTrace, GcTraceCtx,
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

    /// Get gc parition id
    #[inline(always)]
    pub(crate) const fn gc_part_id(&self) -> GcPartitionId {
        self.gc_top_scope
    }

    /// alloc data in given gc partition_id
    pub(crate) fn gc_alloc_in<T: GcNode>(
        &mut self,
        partition_id: GcPartitionId,
        data: T,
    ) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc_heap_mut().alloc(partition_id, data)?;

        if self.gc_top_scope().should_gc() {
            self.gc_req = true;
        }
        Ok(r)
    }

    #[inline]
    pub fn gc_alloc<T: GcNode>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_alloc_in(self.gc_part_id(), data)
    }

    /// collect garbage
    pub(crate) fn gc_collect_in(&mut self, partition_id: GcPartitionId) -> usize {
        log::trace!("[collect] {partition_id:?}");

        // Tracing
        // * root nodes (Hint: Exception, Contexts are root nodes)
        // * stack frame
        // * user defined

        self.gc_heap_mut().mark(partition_id, 0);

        let mut gcx = self.gc_heap().create_trace_ctx();
        if let Some(sf) = self.current_stack_frame() {
            sf.trace(&mut gcx);
        }
        // user defined mark
        if let Some(f) = self.gc_user_trace.as_ref() {
            f(&mut gcx);
        }
        // mark currently running job
        if let Some(f) = self.gc_cur_job_trace.as_ref() {
            f(&mut gcx);
        }

        for n in gcx.take_nodes() {
            if unsafe { n.as_ref().partition_id() } == partition_id {
                self.gc_heap_mut().add_gray_node(n);
            }
        }

        let mut freed = 0;
        if self.gc_heap_mut().mark(partition_id, usize::MAX) {
            // mark cycle done, sweep whites
            freed = self
                .gc_heap_mut()
                .sweep(partition_id, GcHeap::DUMMY_DISPOSE_CALLBACK);
        }

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
        self.gc_collect_in(self.gc_part_id());
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
