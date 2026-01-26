use gc_lite::{GcError, GcHeap, GcPartition, GcPartitionId, GcRef, GcTracable, GcTracer};

use crate::{object::ShapeImpl, runtime::RuntimeImpl};

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

    /// gc partition
    #[inline(always)]
    pub(crate) const fn gc_part(&self) -> &GcPartition {
        debug_assert!(!self.gc_part.is_null());
        unsafe { &*self.gc_part }
        //self.gc_heap.partition(self.gc_part_id).unwrap()
    }

    /// gc partition
    #[inline(always)]
    pub(crate) const fn gc_part_mut(&mut self) -> &mut GcPartition {
        debug_assert!(!self.gc_part.is_null());
        unsafe { &mut *self.gc_part }
    }

    pub fn gc_memory_limit(&self) -> usize {
        self.gc_part().memory_limit()
    }

    pub fn gc_set_memory_limit(&mut self, limit: usize) -> usize {
        self.gc_part_mut().set_memory_limit(limit)
    }

    /// alloc data in gc
    #[inline]
    pub fn gc_alloc<T: GcTracable>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        let r = self.gc_heap.alloc(self.gc_part_id, data)?;
        if self.gc_part().should_gc() {
            self.gc_req = true;
        }
        Ok(r)
    }

    /// collect garbage.
    pub fn gc_collect(&mut self) -> usize {
        let part_id = self.gc_part_id;

        //
        // mark
        //
        {
            let mut tracer = GcTracer::new();

            self.gc_heap.mark_roots(part_id, &mut tracer);
            self.cur_exception.trace(&mut tracer);
            self.shape_registry.trace(&mut tracer);

            // mark nodes in stack-frame chain
            if let Some(sf) = self.current_stack_frame() {
                sf.trace(&mut tracer);
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

            tracer.commit_marks(&self.gc_heap);
        }

        //
        // sweep
        //
        let mut freed = 0;
        if let Some(type_shape) = self.gc_heap().type_id_of::<ShapeImpl>() {
            // GC: ShapImpl must be freed at last
            freed = self
                .gc_heap
                .sweep_ex(part_id, false, None, Some(&[type_shape]), true);
        }

        freed += self.gc_heap.sweep_ex(part_id, false, None, None, false);

        // update gc threshold
        let used = self.gc_part().memory_used();
        self.gc_part_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of

        self.gc_req = false;
        freed
    }

    // pub(crate) fn gc_set_root_value(&mut self, val: JSValue, b: bool) {
    //     match val {
    //         JSValue::Object(o) => {
    //             self.gc_heap_mut().set_root(o.gc_ref(), b);
    //         }
    //         JSValue::String(s) => {
    //             if s.rt().is_some() {
    //                 self.gc_heap_mut().set_root(s.gc_ref(), b);
    //             }
    //         }
    //         JSValue::Symbol(a) => {
    //             a.set_gc_root(b);
    //         }
    //         JSValue::Module(m) => {
    //             self.gc_heap_mut().set_root(m.gc_ref(), b);
    //         }
    //         JSValue::Bytecode(bc) => {
    //             self.gc_heap_mut().set_root(bc.gc_ref(), b);
    //         }
    //         _ => {}
    //     }
    // }

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
