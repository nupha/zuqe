use gc_lite::{GcError, GcHeap, GcPartition, GcPartitionId, GcRef, GcTracable, GcTracer};

use crate::{object::ShapeImpl, runtime::RuntimeImpl, stack_frame::StackFrame};

impl RuntimeImpl {
    pub fn gc_set_threshold(&mut self, threshold: usize) {
        self.gc_heap.set_gc_threshold(self.gc_part_id, threshold);
    }

    #[inline(always)]
    pub(crate) const fn gc_heap(&self) -> &GcHeap {
        &self.gc_heap
    }

    #[inline(always)]
    pub(crate) const fn gc_heap_mut(&mut self) -> &mut GcHeap {
        &mut self.gc_heap
    }

    /// gc partition id
    #[inline(always)]
    pub(crate) const fn gc_part_id(&self) -> GcPartitionId {
        self.gc_part_id
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

    pub fn gc_memory_limit(&self) -> usize {
        self.gc_root_part().memory_limit()
    }

    pub fn gc_set_memory_limit(&mut self, limit: usize) -> usize {
        self.gc_root_part_mut().set_memory_limit(limit)
    }

    /// alloc data in given gc partition
    pub(crate) fn gc_alloc_in<T: GcTracable>(
        &mut self,
        partition_id: GcPartitionId,
        data: T,
    ) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc_heap.alloc(partition_id, data)?;

        #[cfg(debug_assertions)] // O.o
        {
            if partition_id != GcPartitionId(1) {
                let ty = self.gc_heap().get_node_type(r.head_ptr());
                println!(
                    "[O.o][{partition_id:?}] alloc {:?}, {}",
                    r.head_ptr(),
                    ty.type_name
                );
            }
        }

        if self.gc_root_part().should_gc() {
            self.gc_req = true;
        }
        Ok(r)
    }

    /// alloc data in stack frame scoped gc partition
    fn gc_alloc_scoped<T: GcTracable>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        let part_id = if let Some(sf) = self.current_stack_frame_mut() {
            sf.gc_scoped_partition()
        } else {
            self.gc_part_id
        };
        self.gc_alloc_in(part_id, data)
    }

    /// alloc data in gc
    #[inline(always)]
    pub fn gc_alloc<T: GcTracable>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        // self.gc_alloc_in(self.gc_part_id)
        self.gc_alloc_scoped(data)
    }

    /// collect garbage root.
    pub(crate) fn gc_collect_root(&mut self) -> usize {
        let part_id = self.gc_part_id;

        //
        // mark
        //
        {
            // self.gc_heap_mut().clear_marks(part_id);

            let mut tracer = GcTracer::new(self.gc_heap(), part_id);
            tracer.trace_roots(GcTracer::MARK_FUNC);
            if let Some(x) = self.cur_exception.gc_node_ptr() {
                tracer.trace(x, GcTracer::MARK_FUNC);
            }
            tracer.trace_iter(
                self.shape_registry.gc_nodes().map(|x| x.head_ptr()),
                GcTracer::MARK_FUNC,
            );

            // self.gc_heap.mark_roots(part_id, &mut tracer);
            // self.cur_exception.trace(&mut tracer);
            // self.shape_registry.trace(&mut tracer);

            if let Some(sf) = self.current_stack_frame() {
                tracer.trace_iter(sf.gc_nodes(), GcTracer::MARK_FUNC);
                //sf.trace(&mut tracer);
            }

            // mark pending jobs, and currently running job
            for cx in self.context_list.iter() {
                cx.job_list.iter().for_each(|x| {
                    x.trace(&mut tracer);
                });
            }
            if let Some(f) = self.gc_cur_job_trace.as_ref() {
                f(&mut tracer);
            }

            // user defined marks
            if let Some(f) = self.gc_user_trace.as_ref() {
                f(&mut tracer);
            }

            tracer.commit();
        }

        //
        // sweep
        //
        let mut freed = 0;
        if let Some(type_shape) = self.gc_heap().type_id_of::<ShapeImpl>() {
            // Shape must be freed at last
            freed = self.gc_heap.sweep_with(part_id, |node| {
                let gctype = node.gc_type_id();
                if !node.is_marked() {
                    gctype != type_shape
                } else {
                    if gctype != type_shape {
                        node.set_marked(false); // clear mark
                    }
                    false
                }
            });
        }
        freed += self
            .gc_heap
            .sweep_with(part_id, GcHeap::SWEEP_UNMARKED_FUNC);

        // let mut freed = 0;
        // if let Some(type_shape) = self.gc_heap().type_id_of::<ShapeImpl>() {
        //     // GC: ShapImpl must be freed at last
        //     freed = self
        //         .gc_heap
        //         .sweep_ex(part_id, false, None, Some(&[type_shape]), true);
        // }
        // freed += self.gc_heap.sweep_ex(part_id, false, None, None, false);

        // update gc threshold
        let used = self.gc_root_part().memory_used();
        self.gc_root_part_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of

        self.gc_req = false;
        freed
    }

    pub(crate) fn gc_collect_scoped(&mut self, stack_frame: &StackFrame) -> usize {
        let part_id = stack_frame.gc_scoped_part_id;
        if part_id == GcPartitionId::NONE {
            return 0;
        }

        // mark
        let mut tr = GcTracer::new(self.gc_heap(), part_id);
        tr.trace_roots(GcTracer::MARK_FUNC);
        tr.trace_iter(stack_frame.gc_nodes(), GcTracer::MARK_FUNC);
        tr.trace_iter(
            self.shape_registry.gc_nodes().map(|x| x.head_ptr()),
            GcTracer::MARK_FUNC,
        );

        // mark pending jobs, and currently running job
        for cx in self.context_list.iter() {
            cx.job_list.iter().for_each(|x| {
                x.trace(&mut tr);
            });
        }
        if let Some(f) = self.gc_cur_job_trace.as_ref() {
            f(&mut tr);
        }

        // user defined marks
        if let Some(f) = self.gc_user_trace.as_ref() {
            f(&mut tr);
        }
        tr.commit();

        // sweep
        let freed = self
            .gc_heap_mut()
            .sweep_with(part_id, GcHeap::SWEEP_UNMARKED_FUNC);

        // let mut freed = 0;
        // if let Some(type_shape) = self.gc_heap().type_id_of::<ShapeImpl>() {
        //     // Shape must be freed at last
        //     freed = self.gc_heap.sweep_with(part_id, |node| {
        //         let gctype = node.type_id();
        //         if !node.is_marked() {
        //             gctype != type_shape
        //         } else {
        //             if gctype != type_shape {
        //                 node.set_marked(false); // clear mark
        //             }
        //             false
        //         }
        //     });
        // }
        // freed += self
        //     .gc_heap
        //     .sweep_with(part_id, GcHeap::SWEEP_UNMARKED_FUNC);

        // update gc threshold
        let used = self.gc_root_part().memory_used();
        self.gc_root_part_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of

        self.gc_req = false;
        freed
    }

    #[inline(always)]
    pub fn gc_set_trace_handler(&mut self, f: impl Fn(&mut GcTracer) + 'static) {
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
