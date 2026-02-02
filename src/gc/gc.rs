use std::ptr::NonNull;

use gc_lite::{
    GcError, GcHead, GcHeap, GcPartition, GcPartitionId, GcRef, GcTracable, GcTraceOp, GcTracer,
};

use crate::{object::ShapeImpl, runtime::RuntimeImpl};

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
    #[inline(always)]
    pub(crate) const fn gc_root_part(&self) -> &GcPartition {
        debug_assert!(!self.gc_part.is_null());
        unsafe { &*self.gc_part }
    }

    /// top gc partition for runtime
    #[inline(always)]
    pub(crate) const fn gc_root_part_mut(&mut self) -> &mut GcPartition {
        debug_assert!(!self.gc_part.is_null());
        unsafe { &mut *self.gc_part }
    }

    #[inline(always)]
    #[deprecated]
    pub(crate) const fn gc_root_scope_id(&self) -> GcPartitionId {
        self.gc_root_scope
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

    /// get or initialize gc scope for current stack frame.
    /// if current stack frame has no scope, initialize it.
    pub(crate) fn current_gc_scope_force(&mut self) -> GcPartitionId {
        if let Some(sf) = self.current_stack_frame_mut() {
            sf.init_gc_scope()
        } else {
            self.gc_root_scope
        }
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

        // #[cfg(debug_assertions)] // O.o
        // {
        //     let ty = self.gc_heap().get_node_type(r.node_ptr());
        //     println!("[O.o][{scope_id:?}] alloc {r:?}, {}", ty.type_name);
        // }

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
    pub(crate) fn gc_collect_in(&mut self, scope: GcPartitionId) -> usize {
        //
        // mark
        //
        {
            let marker_fn = |mut h: NonNull<GcHead>, _: &GcHeap| unsafe {
                if !h.as_ref().is_marked() {
                    h.as_mut().set_marked(true);
                    true
                } else {
                    false
                }
            };

            let mut tr = GcTracer::new(NonNull::from_ref(self.gc_heap()), scope);

            // Note: for root scope, Contexts are root nodes, they have been traced in trace_root step.

            tr.trace_roots(marker_fn);
            tr.trace_iter(
                self.shape_registry.gc_nodes().map(|x| x.node_ptr()),
                marker_fn,
            );

            if let Some(n) = self.cur_exception.gc_node_ptr() {
                tr.trace(n, marker_fn);
            }
            if let Some(sf) = self.current_stack_frame() {
                tr.trace_iter(sf.gc_nodes(), marker_fn);
            }

            // user defined trace
            if let Some(f) = self.gc_user_trace.as_ref() {
                f(tr.ctx());
            }

            if scope == self.gc_root_scope {
                // currently running job
                if let Some(f) = self.gc_cur_job_trace.as_ref() {
                    f(tr.ctx());
                }
            }

            tr.commit(marker_fn);
        }

        //
        // sweep
        //
        let mut freed = 0;
        if let Some(type_shape) = self.gc_heap().type_id_of::<ShapeImpl>() {
            // Shape must be freed at last
            freed = self.gc_heap.sweep(scope, |node| {
                let gctype = node.gc_type_id();
                !node.is_marked() && (gctype != type_shape)
            });
        }

        freed += self.gc_heap.sweep(scope, GcHeap::SWEEP_UNMARKED_FUNC);

        // update gc threshold
        let used = self.gc_root_part().memory_used();
        self.gc_root_part_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of

        self.gc_req = false;
        freed
    }

    #[inline]
    pub(crate) fn gc_collect(&mut self) {
        if let Some(sc) = self.current_gc_scope() {
            self.gc_collect_in(sc);
        }

        // if let Some(sf) = unsafe { self.cur_stack_frame.cast::<StackFrame>().as_ref() } {
        //     self.gc_collect_scoped(sf);
        // } else {
        //     self.gc_collect_root();
        // }
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
