use gc_lite::{
    GcError, GcHeap, GcPartition, GcPartitionId, GcRef, GcTracable, GcTraceOp, GcTraceRestrict,
    GcTracer,
};

use crate::runtime::RuntimeImpl;

impl RuntimeImpl {
    pub fn gc_set_threshold(&mut self, threshold: usize) {
        self.gc_heap.set_gc_threshold(self.gc_root_scope, threshold);
    }

    #[inline(always)]
    pub(crate) const fn gc_heap(&self) -> &GcHeap {
        &self.gc_heap
    }

    #[inline(always)]
    pub(crate) const fn gc_heap_mut(&mut self) -> &mut GcHeap {
        &mut self.gc_heap
    }

    /// top gc partition for runtime
    #[inline]
    pub(crate) fn gc_root_part(&self) -> &GcPartition {
        self.gc_heap().partition(self.gc_root_scope).unwrap()
    }

    /// top gc partition for runtime
    #[inline]
    pub(crate) fn gc_root_part_mut(&mut self) -> &mut GcPartition {
        let pid = self.gc_root_scope;
        self.gc_heap_mut().partition_mut(pid).unwrap()
    }

    /// get gc scope for current stack frame.
    /// if current stack frame has no scope, or has no stack frame, returns None.
    pub(crate) fn current_gc_scope(&mut self) -> Option<GcPartitionId> {
        self.current_stack_frame()
            .map_or(Some(self.gc_root_scope), |sf| {
                if sf.gc_scope_id != GcPartitionId::NONE {
                    Some(sf.gc_scope_id)
                } else {
                    None
                }
            })
    }

    pub fn gc_memory_limit(&self) -> usize {
        self.gc_root_part().memory_limit()
    }

    pub fn gc_set_memory_limit(&mut self, limit: usize) -> usize {
        self.gc_root_part_mut().set_memory_limit(limit)
    }

    /// alloc data in given gc scope
    pub(crate) fn gc_alloc_in<T: GcTracable>(
        &mut self,
        scope_id: GcPartitionId,
        data: T,
    ) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc_heap_mut().alloc(scope_id, data)?;

        if self.gc_root_part().should_gc() {
            self.gc_req = true;
        }
        Ok(r)
    }

    /// alloc data in gc
    pub fn gc_alloc<T: GcTracable>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        let part_id = if let Some(sf) = self.current_stack_frame_mut() {
            sf.init_gc_scope()
        } else {
            self.gc_root_scope
        };
        self.gc_alloc_in(part_id, data)
    }

    /// collect garbage
    pub(crate) fn gc_collect_in(&mut self, partition_id: GcPartitionId) -> usize {
        log::trace!("[collect] {partition_id:?}");

        // 1. mark
        {
            let mut tr = GcTracer::new(self.gc_heap(), GcTraceRestrict::No, true);

            // Note: for root scope, Contexts are root nodes, they have been traced in trace_root step.
            tr.trace_roots(partition_id, GcTracer::MARK_FUNC);

            if let Some(sf) = self.current_stack_frame() {
                debug_assert_eq!(sf.gc_scope_id, partition_id);
                sf.trace(tr.ctx());
                // tr.trace_iter(sf.gc_nodes(), GcTracer::MARK_FUNC);
            }
            if let Some(n) = self.cur_exception.gc_node_ptr() {
                tr.trace(n, GcTracer::MARK_FUNC);
            }

            // user defined trace
            if let Some(f) = self.gc_user_trace.as_ref() {
                f(tr.ctx());
            }
            // currently running job
            if let Some(f) = self.gc_cur_job_trace.as_ref() {
                f(tr.ctx());
            }

            tr.commit(GcTracer::MARK_FUNC);
        }

        // 2. sweep
        let freed = self
            .gc_heap
            .sweep(partition_id, GcHeap::SWEEP_UNMARKED_FUNC, {
                #[cfg(debug_assertions)]
                {
                    |n| {
                        println!("[O.o][collect] dispose node {n:?}");
                    }
                }
                #[cfg(not(debug_assertions))]
                GcHeap::DUMMY_DISPOSE_CALLBACK
            });

        if freed != 0 {
            // update gc threshold
            let used = self.gc_root_part().memory_used();
            self.gc_root_part_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of
        }

        log::trace!("[collect] {partition_id:?} free {freed} bytes");

        self.gc_req = false;
        freed
    }

    #[inline]
    pub(crate) fn gc_collect(&mut self) {
        if let Some(sc) = self.current_gc_scope() {
            self.gc_collect_in(sc);
        }
    }

    #[inline(always)]
    pub fn gc_set_trace_handler(&mut self, f: impl Fn(GcTraceOp) + 'static) {
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
}
