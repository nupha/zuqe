use gc_lite::{Collect, GcContext, GcError, GcRef, PartitionId, PartitionInfo, Tracer};

use crate::runtime::RuntimeImpl;

impl RuntimeImpl {
    #[inline(always)]
    pub const fn gc_part(&self) -> PartitionId {
        self.gc_part
    }

    pub fn set_gc_threshold(&mut self, threshold: usize) {
        self.gc_heap.set_gc_threshold(self.gc_part, threshold);
    }

    #[inline(always)]
    pub const fn gc_heap(&self) -> &GcContext {
        &self.gc_heap
    }

    #[inline(always)]
    pub const fn gc_heap_mut(&mut self) -> &mut GcContext {
        &mut self.gc_heap
    }

    #[inline(always)]
    pub fn gc_info(&self) -> &PartitionInfo {
        let part = self.gc_part;
        self.gc_heap.get_partition(part).unwrap()
    }

    /// alloc data in gc heap
    #[inline(always)]
    pub fn gc_alloc<T: Collect>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap.alloc(self.gc_part, data)
    }

    /// collect garbage
    pub fn gc_collect(&mut self) -> usize {
        let part = self.gc_part;
        let mut tracer = Tracer::new();

        // mark objects in stack - to keep function local variables alive
        if let Some(sf) = self.current_stack_frame() {
            self.gc_heap.trace_mark(sf, &mut tracer);
        }

        // mark root
        self.gc_heap.mark_roots(part, &mut tracer);

        let freed = self.gc_heap.sweep(part);
        println!("分区 {part:?} 回收 {freed} 字节");

        // next gc threshold
        let used = self.gc_info().memory_used();
        self.gc_threshold = used + (used >> 1); // x1.5 of used

        freed
    }
}

pub fn js_trigger_gc(rt: &mut RuntimeImpl, size: usize) {
    let force_gc: bool;

    #[cfg(feature = "FORCE_GC_AT_MALLOC")]
    {
        force_gc = true;
    }
    #[cfg(not(feature = "FORCE_GC_AT_MALLOC"))]
    {
        force_gc = ((*rt).malloc_state.malloc_size + size) > (*rt).gc_threshold;
    }

    if force_gc {
        todo!();

        #[cfg(feature = "DUMP_GC")]
        {
            println!("GC: size={}", (*rt).malloc_state.malloc_size);
        }

        //rt.gc();
        rt.gc_threshold = rt.malloc_state.malloc_size + (rt.malloc_state.malloc_size >> 1);
    }
}
