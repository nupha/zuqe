// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{
    GcError, GcHeap, GcNode, GcPartitionId, GcRef, GcTrace, GcTraceFn, gc_type_register,
};

use crate::{
    context::{ContextImpl, ScopedContext},
    error::Exception,
    function::{AsyncStateImpl, BytecodeImpl, VarRefImpl},
    module::ModuleDefImpl,
    object::{ObjectImpl, ShapeImpl},
    runtime::RuntimeImpl,
    string::StringImpl,
    value::JSValue,
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

    /// Get gc parition id
    #[inline(always)]
    pub(crate) const fn gc_part_id(&self) -> GcPartitionId {
        self.gc_part_hot
    }

    /// alloc gc root node
    pub(crate) fn gc_alloc_root<T: GcNode>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        let par = self.gc_part_id();
        let r = unsafe { self.gc_heap_mut().alloc_root_raw(par, data) };
        if self.gc_heap().should_gc() {
            self.gc_req = true;
        }
        r
    }

    /// collect garbage
    pub(crate) fn gc_collect_in(&mut self, partition_id: GcPartitionId) -> usize {
        log::trace!("[collect] {partition_id:?}");

        // ensure marking cycle is started
        self.gc_heap_mut().mark(partition_id, 0);

        // --- Phase 1: Remove unused shapes (use_count == 0) ---
        self.shape_registry.remove_unused();

        // --- Phase 2: Trace shape registry into its own GcTraceCtx ---
        // (field-level borrows to avoid &self conflicts with later &mut self)
        let shape_nodes = {
            let heap = &self.gc_heap;
            let mut shape_gcx = heap.create_trace_ctx(128);
            self.shape_registry.trace_all(&mut shape_gcx);
            shape_gcx.take_nodes()
        };

        // --- Phase 3: Trace other GC roots (contexts, stack, etc.) ---
        let other_nodes = {
            let heap = &self.gc_heap;
            let mut gcx = heap.create_trace_ctx(256);

            // mark contexts
            for c in self.context_list.iter() {
                c.trace(&mut gcx);
            }
            // mark stackless arena frames (active frames in StackFrameArena)
            self.frame_arena.trace(&mut gcx);
            // mark trampoline virtual frames (caller bytecodes not in arena prev chain).
            // Currently always null; will be wired when trampoline integration is complete.
            let vframes_ptr = self.active_trampoline_vframes;
            if !vframes_ptr.is_null() {
                // SAFETY: vframes_ptr is non-null only during trampoline execution.
                // Iteration requires length metadata; this will be expanded when
                // the trampoline stores its virtual frame slice.
            }
            // Double-trace active trampoline frame's func_obj (belt-and-suspenders
            // with arena trace: covers the race window between arena state update
            // and trampoline iteration when GC triggers).
            if !self.active_trampoline_frame.is_null() {
                let head = self.frame_arena.frame(self.active_trampoline_frame);
                head.func_obj.trace(&mut gcx);
            }
            // mark error value
            if let Exception::Error(err) = self.exception() {
                err.trace(&mut gcx);
            }

            // NOTE: Atom registry is NOT responsible for keeping atom
            // descriptor strings alive. Atom descriptors are traced through
            // objects that reference them (BytecodeImpl via ATOM opcodes,
            // ShapeImpl via property keys etc.). See `GcTrace for Atom`.

            // trace currently runnnig job
            if let Some(f) = self.gc_job.as_ref() {
                f(&mut gcx);
            }

            // trace user defined gc
            if let Some(f) = self.gc_user_trace.as_ref() {
                f(&mut gcx);
            }

            gcx.take_nodes()
        };

        // --- Phase 4: Add all traced nodes to gray list ---
        for n in shape_nodes.into_iter().chain(other_nodes) {
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

        // update new gc threshold
        let used = self.gc_heap().memory_used();
        self.gc_heap_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of

        log::trace!("[collect] {partition_id:?} free {freed} bytes");

        self.gc_req = false;
        freed
    }

    #[inline(always)]
    pub(crate) fn gc_collect(&mut self) {
        self.gc_collect_in(self.gc_part_id());
    }

    #[inline(always)]
    pub fn gc_set_trace_handler(&mut self, f: impl GcTraceFn + 'static) {
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
        self.gc_heap().memory_limit()
    }

    pub fn gc_set_memory_limit(&mut self, limit: usize) -> usize {
        self.gc_heap_mut().set_memory_limit(limit)
    }

    pub fn gc_set_threshold(&mut self, threshold: usize) {
        self.gc_heap_mut().set_gc_threshold(threshold);
    }
}

impl ScopedContext<'_> {
    /// GC transient node protection
    ///
    /// Ensure a JSValue survives collection within the current scope.
    /// Use immediately after taking a value out of a traced location (stack
    /// frame, Vec, cur_exception, etc.) before any call that might allocate.
    /// This is a no-op for primitive values or already-protected nodes.
    #[inline(always)]
    pub(crate) fn gc_protect_transient(&self, val: &JSValue) {
        #[cfg(feature = "gc_protect_transient")]
        self.gc_protect_transient_force(val)
    }

    /// Force-version gc_protect_transient that always protects, regardless of
    /// feature flag. Use this for C-stack-local values that were previously
    /// traced by StackFrame::trace (legacy model) but, in the arena/trampoline
    /// execution model, lack GC root coverage between the point they leave a
    /// traced location and the point they are stored in a new traced location.
    #[inline(always)]
    pub(crate) fn gc_protect_transient_force(&self, val: &JSValue) {
        if let Some(n) = val.gc_node_ptr() {
            self.gc_state().add_non_local(n);
        }
    }
}
