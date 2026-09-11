// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::{
    GcError, GcHead, GcHeap, GcNode, GcNodeLink, GcPartitionId, GcRef, GcTrace, GcTraceCtx,
    GcTraceFn, gc_type_register,
};

// `GcTraceCtx` is only referenced by the feature-off `BigIntImpl` placeholder.
#[cfg(not(feature = "big_int"))]
use gc_lite::GcTraceCtx;

use crate::{
    context::{ContextImpl, ScopedContext},
    error::Exception,
    function::{AsyncStateImpl, BytecodeImpl, VarRefImpl},
    module::ModuleDefImpl,
    object::{ObjectImpl, ShapeImpl},
    runtime::RuntimeImpl,
    string::{RopeImpl, StringImpl},
    value::JSValue,
};

// The `gc_type_register!` macro expands every entry into a `GcTypeInfo` table
// slot plus a `GcNode` impl, and `#[cfg]` attributes are not accepted inside
// the entry list. To keep the `bigint` module feature-gated (`big_int`), the
// type is aliased here: when the feature is on it is the real
// `crate::bigint::BigIntImpl`, otherwise a private zero-sized placeholder so
// the registry layout is identical either way.
#[cfg(feature = "big_int")]
use crate::bigint::BigIntImpl;

#[cfg(not(feature = "big_int"))]
#[allow(dead_code)]
struct BigIntImpl;
#[cfg(not(feature = "big_int"))]
impl GcTrace for BigIntImpl {
    #[inline(always)]
    fn trace(&self, _: &mut GcTraceCtx) {}
}

// register gc data types
gc_type_register! {
    ContextImpl;
    ObjectImpl;
    ModuleDefImpl;
    VarRefImpl;
    BytecodeImpl;
    StringImpl;
    RopeImpl;
    AsyncStateImpl;
    ShapeImpl, drop_pass = 1;
    BigIntImpl;
}

/// fn-pointer GC trace callback: `f(gcx, opaque)` traces values held by the
/// registered native owner (e.g. a zapi extension's thread_local).
pub(crate) type GcTraceCallbackFn = fn(&mut GcTraceCtx, *mut std::ffi::c_void);

/// zhuque-side extension of [`GcHeap`] for [`JSValue`] write barriers.
pub(crate) trait GcHeapExt {
    /// Write barrier for a value write: if `val` is a gc node, `bind` it to
    /// `owner`; no-op for non-gc values.
    fn bind_value(&mut self, owner: NonNull<GcHead>, val: &JSValue);
}

impl GcHeapExt for GcHeap {
    #[inline(always)]
    fn bind_value(&mut self, owner: NonNull<GcHead>, val: &JSValue) {
        if let Some(n) = val.as_gc_node() {
            self.bind(owner, n);
        }
    }
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
        self.gc_bump_alloc_check();
        r
    }

    /// Batched should_gc() check after a GC-node allocation. All allocation
    /// entry points must route through this so the check stays unified.
    #[inline(always)]
    pub(crate) fn gc_bump_alloc_check(&mut self) {
        self.gc_alloc_count += 1;

        if self.gc_alloc_count >= self.gc_alloc_check_interval {
            self.gc_alloc_count = 0;
            if self.gc_heap.should_gc() {
                self.gc_req = true;
            }
        }
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
            // Double-trace active trampoline frame's func_obj (belt-and-suspenders
            // with arena trace: covers the race window between arena state update
            // and trampoline iteration when GC triggers).
            if !self.active_frame_index.is_null() {
                let head = self.frame_arena.frame(self.active_frame_index);
                head.func_obj.trace(&mut gcx);
            }

            // trace all pending Promises — they must survive GC even when no
            // user code holds a reference, otherwise their settle callbacks
            // (PromiseReaction handlers) are lost.
            for p in self.gc_pending_promises.iter() {
                p.trace(&mut gcx);
            }

            // trace rooted async-function states. An AsyncState escapes its
            // allocating scope as soon as the async function first suspends;
            // relying solely on trace edges through promise reactions left a
            // GC-timing window where the state (and its suspended frame) was
            // collected while still resumable ("bad node" / recycled-object
            // crashes under connection-storm workloads). States are added on
            // creation and removed at completion (AsyncState::execute).
            for s in self.gc_pending_async_states.iter() {
                s.trace(&mut gcx);
            }

            // trace error value
            if let Exception::Error(err) = self.exception() {
                err.trace(&mut gcx);
            }

            // mark the eval base URL bound at the host `eval` entry point.
            // It is a scope-local (non-root) atom whose descriptor string is not
            // kept alive by any GC object, so it must be traced here directly.
            self.eval_base_url.trace(&mut gcx);

            // NOTE: Atom registry is NOT responsible for keeping atom
            // descriptor strings alive. Atom descriptors are traced through
            // objects that reference them (BytecodeImpl via ATOM opcodes,
            // ShapeImpl via property keys etc.). See `GcTrace for Atom`.

            // trace currently runnnig job
            if let Some(f) = self.gc_job.as_ref() {
                f(&mut gcx);
            }

            // trace user defined gc (closure handlers)
            for f in &self.gc_user_trace {
                f(&mut gcx);
            }
            // trace user defined gc (fn-pointer handlers, e.g. zapi extensions'
            // thread_local traces registered via `gc_add_trace_callback`)
            for (f, opaque) in &self.gc_trace_fns {
                f(&mut gcx, *opaque);
            }

            // trace promise future registry entries (JSValues awaiting settlement)
            #[cfg(feature = "async_support")]
            self.promise_future_registry.trace_all(&mut gcx);

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
            // mark cycle done, use two-phase sweep.
            //
            // Phase 1: Unlink white nodes (needs &mut GcHeap).
            if let Some(white) = self.gc_heap_mut().sweep_unlink(partition_id) {
                // Phase 2: Pre-drop cleanup — runtime side-effects for dying
                // nodes (atom removal, object finalize). Runs with the
                // runtime's own &mut self and NO drop_fn executing, so no
                // re-entrancy is possible by construction.
                self.pre_drop_cleanup(&white);

                // Phase 3: Drop payloads — Drop impls are trivial (no
                // RuntimeImpl access), so holding &mut self across this is
                // sound. Only drops white non-root/non-local nodes; root/local
                // survivors are kept by sweep_dispose and freed later.
                let registry = self.gc_heap().type_registry();
                gc_lite::GcHeap::sweep_drop_payloads(&white, registry);

                // Phase 4: Dispose — frees memory, updates accounting (needs &mut GcHeap).
                freed = self.gc_heap_mut().sweep_dispose(partition_id, white);
            }
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

    /// Runtime side-effects for dying white nodes, run BEFORE their payloads
    /// are dropped.
    ///
    /// This replaces logic that used to live in payload `Drop` impls
    /// (`StringImpl::drop` atom removal, `ObjectImpl::finalize`). Payload
    /// `Drop` impls MUST stay trivial (no `RuntimePtr` deref): a deref here
    /// would create a second non-derived access path to `RuntimeImpl` while
    /// the caller's `&mut self` is live — a `noalias` violation that LTO
    /// exploits into memory corruption (SEGV in release).
    ///
    /// Soundness: runs with the runtime's own `&mut self` and no `drop_fn`
    /// executing, so all accesses derive from `self`. White-node payloads
    /// are still alive (unlinked but not yet dropped/disposed), so reading
    /// `atom_info` / running `finalize` sees the same state the old `Drop`
    /// impls did.
    pub(crate) fn pre_drop_cleanup(&mut self, white: &GcNodeLink) {
        let registry = self.gc_heap().type_registry();

        for node in white.iter() {
            // SAFETY: nodes in the white list are live (unlinked from the
            // partition but not yet dropped or disposed). The payload layout
            // matches the registered type info.
            let (dtype, payload) = unsafe {
                let head = node.as_ref();
                let info = &registry.type_info_list[head.dtype() as usize];
                (head.dtype(), info.payload_ptr(node).as_ptr())
            };

            if dtype == StringImpl::GC_TYPE_ID {
                // SAFETY: dtype matches; payload is a valid StringImpl.
                let s = unsafe { &*(payload as *const StringImpl) };
                if let Some(a) = s.atom_info()
                    && !a.is_const()
                    && !a.is_small_int()
                {
                    self.remove_atom(a);
                }
            } else if dtype == ObjectImpl::GC_TYPE_ID {
                // SAFETY: dtype matches; payload is a valid ObjectImpl.
                let o = unsafe { &mut *(payload as *mut ObjectImpl) };
                o.finalize(self);
            }
        }
    }

    #[inline(always)]
    pub fn gc_set_trace_handler(&mut self, f: impl GcTraceFn + 'static) {
        // Multiple handlers are supported; each runs at every GC. The handler is
        // a closure (aike registers one that fans out to its thread_locals).
        self.gc_user_trace.push(Box::new(f));
    }

    /// Register a fn-pointer GC trace callback `(f, opaque)`; `f(gcx, opaque)`
    /// is invoked at every GC. Used by native extensions (zapi
    /// `set_gc_trace_func`) to trace JS values held outside the GC heap.
    #[inline(always)]
    pub fn gc_add_trace_callback(&mut self, f: GcTraceCallbackFn, opaque: *mut std::ffi::c_void) {
        self.gc_trace_fns.push((f, opaque));
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
    #[cfg(not(feature = "gc_protect_transient"))]
    pub(crate) const fn gc_protect_transient(&self, _: &JSValue) {}
    #[inline(always)]
    #[cfg(feature = "gc_protect_transient")]
    pub(crate) fn gc_protect_transient(&self, val: &JSValue) {
        self.gc_protect_transient_force(val)
    }

    /// Force-version gc_protect_transient that always protects, regardless of
    /// feature flag. Use this for C-stack-local values that were previously
    /// traced by StackFrame::trace (legacy model) but, in the arena/trampoline
    /// execution model, lack GC root coverage between the point they leave a
    /// traced location and the point they are stored in a new traced location.
    #[inline(always)]
    pub(crate) fn gc_protect_transient_force(&self, val: &JSValue) {
        if let Some(n) = val.as_gc_node() {
            self.gc_state().add_non_local(n);
        }
    }
}
