use gc_lite::{GcError, GcHeap, GcPartition, GcRef, GcTracable, GcTracer, PartitionId};

use crate::{object::ShapeImpl, runtime::RuntimeImpl, value::JSValue};

impl RuntimeImpl {
    #[inline(always)]
    pub const fn gc_part(&self) -> PartitionId {
        self.gc_part
    }

    pub fn gc_set_threshold(&mut self, threshold: usize) {
        self.gc_heap.set_gc_threshold(self.gc_part, threshold);
    }

    #[inline(always)]
    pub const fn gc_heap(&self) -> &GcHeap {
        &self.gc_heap
    }

    #[inline(always)]
    pub const fn gc_heap_mut(&mut self) -> &mut GcHeap {
        &mut self.gc_heap
    }

    #[inline(always)]
    pub fn gc_info(&self) -> &GcPartition {
        let part = self.gc_part;
        self.gc_heap.get_partition(part).unwrap()
    }

    /// alloc data in gc
    #[inline(always)]
    pub fn gc_alloc<T: GcTracable>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap.alloc(self.gc_part, data)
    }

    /// collect garbage
    pub fn gc_collect(&mut self) -> usize {
        let part = self.gc_part;

        // GC: trace & mark
        {
            let mut tracer = GcTracer::new();

            // mark roots
            self.gc_heap.mark_roots(part, &mut tracer);

            // mark nodes other than roots
            self.cur_exception.trace(&mut tracer);
            self.shape_registry.trace(&mut tracer);

            // mark nodes in stack-frame chain
            if let Some(sf) = self.current_stack_frame() {
                sf.trace(&mut tracer);
            }

            // mark jobs
            for cx in self.context_list.iter() {
                cx.job_list.iter().for_each(|x| x.trace(&mut tracer));
            }

            // mark user defined handler
            if let Some(f) = self.gc_user_trace.as_ref() {
                f(&mut tracer);
            }

            tracer.commit_marks(&self.gc_heap);
        }

        // GC: ShapImpl must be freed at last
        let mut freed = 0;
        if let Some(type_shape) = self.gc_heap().type_idx_of::<ShapeImpl>() {
            freed = self
                .gc_heap
                .sweep_ex(part, false, None, Some(&[type_shape]), true);
        }
        freed += self.gc_heap.sweep_ex(part, false, None, None, false);

        // O.o debug
        // #[cfg(debug_assertions)]
        // if freed > 0 {
        //     let used = self.gc_info().memory_used();
        //     println!(
        //         "[O.o][GC {}] Partition {part:?} collected {freed}, used {used}, threshold->{}",
        //         if self.gc_req { "req" } else { "threshold" },
        //         used + (used >> 1)
        //     );
        // }

        // GC: update gc threshold for next
        let used = self.gc_info().memory_used();
        self.gc_threshold = used + (used >> 1); // x1.5 of

        self.gc_req = false;

        freed
    }

    pub fn gc_set_root_value(&mut self, val: JSValue, b: bool) {
        match val {
            JSValue::Object(o) => {
                self.gc_heap_mut().set_root(o.gc_ref(), b);
            }
            JSValue::String(s) => {
                if s.rt().is_some() {
                    self.gc_heap_mut().set_root(s.gc_ref(), b);
                }
            }
            JSValue::Symbol(a) => {
                a.set_gc_root(b);
            }
            JSValue::Module(m) => {
                self.gc_heap_mut().set_root(m.gc_ref(), b);
            }
            JSValue::Bytecode(bc) => {
                self.gc_heap_mut().set_root(bc.gc_ref(), b);
            }
            _ => {}
        }
    }

    pub fn gc_set_trace_handler(&mut self, f: impl Fn(&mut GcTracer) + 'static) {
        self.gc_user_trace.replace(Box::new(f));
    }

    /// request gc collect
    pub const fn gc_request(&mut self) {
        self.gc_req = true;
    }
}
