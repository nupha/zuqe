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

    /// Write barrier for one GC edge (`owner` now references `child`).
    ///
    /// Engine-internal primitive for paths that only hold `&mut RuntimeImpl`;
    /// callers with a context should use `ScopedContext::bind_value` /
    /// `bind_node_value` instead.
    #[inline]
    pub(crate) fn bind_node(&mut self, owner: NonNull<GcHead>, child: NonNull<GcHead>) {
        self.gc_heap.bind(owner, child);
    }

    /// Node-owner form for a JS value child.
    #[inline]
    pub(crate) fn bind_node_value(&mut self, owner: NonNull<GcHead>, child: JSValue) {
        let Some(child) = child.as_gc_node() else {
            return;
        };
        self.bind_node(owner, child);
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

            // Major wins: if both are due, request only the major.
            #[cfg(feature = "gc_generations")]
            {
                if self.gc_heap.should_gc() {
                    self.gc_req = true;
                } else if self.young_threshold > 0
                    && self.gc_heap.memory_used().saturating_sub(self.young_base)
                        > self.young_threshold
                {
                    self.gc_minor_req = true;
                }
            }
            #[cfg(not(feature = "gc_generations"))]
            if self.gc_heap.should_gc() {
                self.gc_req = true;
            }
        }
    }

    /// Whether a collection is due at an interpreter/JIT safepoint.
    ///
    /// Only explicit requests: the threshold is promoted to `gc_req` by the
    /// batched allocation check (and by `tick`), exactly as without the
    /// generational feature. Checking `should_gc()` here would collect earlier
    /// and change the major cadence.
    #[inline(always)]
    pub(crate) const fn gc_should_collect(&self) -> bool {
        #[cfg(feature = "gc_generations")]
        {
            self.gc_req || self.gc_minor_req
        }
        #[cfg(not(feature = "gc_generations"))]
        {
            self.gc_req
        }
    }

    /// Safepoint dispatch: a due major takes priority over a due minor.
    ///
    /// The `should_gc()` clause only matters when `tick` calls this with a due
    /// threshold but no request; the interpreter/JIT only call it when a request
    /// is already pending, so major cadence is unchanged.
    #[inline(always)]
    pub(crate) fn gc_collect_dispatch(&mut self) {
        #[cfg(feature = "gc_generations")]
        {
            if self.gc_req || self.gc_heap.should_gc() {
                self.gc_collect();
            } else if self.gc_minor_req {
                self.gc_minor();
            }
        }
        #[cfg(not(feature = "gc_generations"))]
        self.gc_collect();
    }

    /// Lower bound for the adapted budget at the current major threshold:
    /// `max(major/8, young_min)`. Halving never goes below it, which keeps a
    /// high-reclaim workload from degenerating into a minor storm.
    #[cfg(feature = "gc_generations")]
    #[inline]
    fn young_floor(&self) -> usize {
        (self.gc_heap.gc_threshold() / 8).max(self.young_min)
    }

    /// Re-clamp the minor budget after a major. A disabled automatic major
    /// threshold disables minor too; the adapted value is kept inside
    /// `[major/8, major]`.
    #[cfg(feature = "gc_generations")]
    pub(crate) fn recompute_young_threshold(&mut self) {
        let major = self.gc_heap.gc_threshold();
        if major == 0 || self.young_promote_after == 0 {
            self.young_threshold = 0;
        } else {
            let lower = self.young_floor();
            let upper = major.max(lower);
            self.young_threshold = self.young_threshold.clamp(lower, upper);
        }
    }

    /// G4.3: adapt the young budget to the last minor's reclaim rate.
    ///
    /// Low reclaim (<10%) jumps to the major threshold; <25% doubles; high
    /// reclaim (>75%) halves down to `max(major/8, young_min)`. Anything in
    /// between is left alone (avoids oscillation on mixed workloads).
    #[cfg(feature = "gc_generations")]
    fn adapt_young_threshold(&mut self, young_bytes: usize, freed: usize) {
        if self.young_threshold == 0 {
            return;
        }
        let lower = self.young_floor();
        let upper = self.gc_heap.gc_threshold().max(lower);
        self.young_threshold =
            adapted_threshold(self.young_threshold, young_bytes, freed, lower, upper);
    }

    /// collect garbage
    pub(crate) fn gc_collect_in(&mut self, partition_id: GcPartitionId) -> usize {
        log::trace!("[collect] {partition_id:?}");
        let started = std::time::Instant::now();

        // ensure marking cycle is started (this also merges the young chain)
        self.gc_heap_mut().mark(partition_id, 0);

        // --- Phase 1: Remove unused shapes (use_count == 0) ---
        self.shape_registry.remove_unused();

        // --- Phase 2/3: Trace every root into the gray list ---
        self.trace_runtime_roots(false);

        let mut freed = 0;
        if self.gc_heap_mut().mark(partition_id, usize::MAX) {
            // mark cycle done, use two-phase sweep.
            if let Some(white) = self.gc_heap_mut().sweep_unlink(partition_id) {
                freed = self.sweep_white(partition_id, white);
            }
        }

        // Drop collected zapi instance handles even when no minor runs.
        #[cfg(all(feature = "zapi", feature = "gc_generations"))]
        self.prune_zapi_instances();

        // update new gc threshold (major only; a minor must not move it)
        let used = self.gc_heap().memory_used();
        self.gc_heap_mut().set_gc_threshold(used + (used >> 1)); // x1.5 of
        // Young bytes are counted from the end of the sweep.
        #[cfg(feature = "gc_generations")]
        {
            self.young_base = used;
            self.gc_minor_req = false;
            self.recompute_young_threshold();
        }

        log::trace!("[collect] {partition_id:?} free {freed} bytes");

        self.gc_req = false;
        self.gc_major_count += 1;
        self.gc_major_freed = self.gc_major_freed.saturating_add(freed);
        self.gc_time_ns = self
            .gc_time_ns
            .saturating_add(started.elapsed().as_nanos() as u64);
        freed
    }

    /// Trace the runtime root set (shape registry, contexts, vm stack, pending
    /// promises/async states, exception, eval base URL, JIT, job, user traces,
    /// future registry, user state) into the heap's gray list.
    ///
    /// Shared by major and minor: a root added only to one of them is a
    /// use-after-free waiting to happen. `force_roots` is set by a minor so that
    /// old (Black) root containers are still descended; the old children they
    /// report are skipped by the GC.
    fn trace_runtime_roots(&mut self, force_roots: bool) {
        #[cfg(not(feature = "gc_generations"))]
        let _ = force_roots;
        // --- Trace shape registry into its own GcTraceCtx ---
        // (field-level borrows to avoid &self conflicts with later &mut self)
        let shape_nodes = {
            let heap = &self.gc_heap;
            let mut shape_gcx = heap.create_trace_ctx(128);
            self.shape_registry.trace_all(&mut shape_gcx);
            shape_gcx.take_nodes()
        };

        // --- Trace other GC roots (contexts, stack, etc.) ---
        let other_nodes = {
            let heap = &self.gc_heap;
            let mut gcx = heap.create_trace_ctx(256);

            // mark contexts
            for c in self.context_list.iter() {
                c.trace(&mut gcx);
            }

            // mark stackless arena frames (active frames in VmStack)
            self.vm_stack.trace(&mut gcx);
            // mark trampoline virtual frames (caller bytecodes not in arena prev chain).
            // Currently always null; will be wired when trampoline integration is complete.
            // Double-trace active trampoline frame's func_obj (belt-and-suspenders
            // with arena trace: covers the race window between arena state update
            // and trampoline iteration when GC triggers).
            if !self.active_frame_index.is_null() {
                let head = self.vm_stack.frame(self.active_frame_index);
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

            // FinalizationRegistry cleanups collected by the previous GC are
            // queued here until the next tick drains them. They are the only
            // owners of `callback`/`held_value` in that window, so a GC running
            // before the tick must trace them (`ContextPtr` is covered by
            // `context_list`).
            #[cfg(feature = "builtin_weakref")]
            for (callback, held_value, _) in self.finrec_pending_cleanups.iter() {
                callback.trace(&mut gcx);
                held_value.trace(&mut gcx);
            }

            // trace error value
            if let Exception::Error(err) = self.exception() {
                err.trace(&mut gcx);
            }

            // mark the eval base URL bound at the host `eval` entry point.
            // It is a scope-local (non-root) atom whose descriptor string is not
            // kept alive by any GC object, so it must be traced here directly.
            self.eval_base_url.trace(&mut gcx);

            // Keep functions pinned by an in-flight JIT compile alive until the
            // result is installed into their `jit` slot (see `jit::JitState`).
            #[cfg(feature = "jit")]
            if let Some(jit) = self.jit.as_deref() {
                jit.trace(&mut gcx);
            }

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

            // mark the host user state attached at runtime creation
            self.user_state.trace(&mut gcx);

            // zapi payloads have no ABI write barrier: re-trace the children of
            // every old instance so a young value the extension wrote into its
            // opaque payload survives the minor. The instance itself is not
            // rooted, so old garbage is still collected on the next major.
            #[cfg(all(feature = "zapi", feature = "gc_generations"))]
            if force_roots {
                for weak in &self.zapi_instances {
                    if let Some(inst) = weak.upgrade(heap) {
                        let node = inst.into_raw().node_ptr();
                        if !unsafe { node.as_ref().is_young() } {
                            heap.trace_node(node, &mut gcx);
                        }
                    }
                }
            }

            gcx.take_nodes()
        };

        // --- Add all traced nodes to gray list ---
        // Each node joins its own partition's gray list; cross-partition roots
        // must not be filtered out (a partition's collection drains its own).
        for n in shape_nodes.into_iter().chain(other_nodes) {
            #[cfg(feature = "gc_generations")]
            if force_roots {
                self.gc_heap_mut().add_gray_node_force(n);
            } else {
                self.gc_heap_mut().add_gray_node(n);
            }
            #[cfg(not(feature = "gc_generations"))]
            self.gc_heap_mut().add_gray_node(n);
        }
    }

    /// White-list teardown shared by minor and major: runtime side-effects, then
    /// trivial payload drops, then dispose.
    fn sweep_white(&mut self, partition_id: GcPartitionId, white: GcNodeLink) -> usize {
        // Pre-drop cleanup — runtime side-effects for dying nodes (atom
        // removal, object finalize). Runs with the runtime's own &mut self and
        // NO drop_fn executing, so no re-entrancy is possible by construction.
        self.pre_drop_cleanup(&white);

        // Drop payloads — Drop impls are trivial (no RuntimeImpl access), so
        // holding &mut self across this is sound. Only drops white
        // non-root/non-local nodes; root/local survivors are kept by
        // sweep_dispose and freed later.
        let registry = self.gc_heap().type_registry();
        gc_lite::GcHeap::sweep_drop_payloads(&white, registry);

        // Dispose — frees memory, updates accounting (needs &mut GcHeap).
        self.gc_heap_mut().sweep_dispose(partition_id, white)
    }

    /// Minor (young-generation) collection: only the young chain plus the
    /// remembered old→young edges is marked; survivors are promoted and the
    /// young chain ends empty (promote-once).
    #[cfg(feature = "gc_generations")]
    pub(crate) fn gc_minor(&mut self) -> usize {
        if self.young_promote_after == 0 {
            self.gc_minor_req = false;
            return 0;
        }
        let started = std::time::Instant::now();

        // Bytes allocated since the last collection: denominator for the
        // survival-rate backoff.
        let young_bytes = self.gc_heap().memory_used().saturating_sub(self.young_base);

        // Shape churn is exactly the young garbage a ctor/objlit workload
        // produces; drop unused shapes before marking.
        self.shape_registry.remove_unused();

        // A minor covers the young generation of every partition; it needs a
        // quiescent heap (no partition mid-mark from a partial major).
        if !self.gc_heap_mut().minor_prepare() {
            self.gc_minor_req = false;
            return 0;
        }
        self.trace_runtime_roots(true);

        let promote_after = self.young_promote_after as u32;
        let mut freed = 0usize;
        self.gc_heap_mut().mark_grays_all();
        if let Some(whites) = self.gc_heap_mut().minor_sweep_unlink(promote_after) {
            for (pid, white) in whites {
                freed = freed.saturating_add(self.sweep_white(pid, white));
            }
        }

        // Debug validator: no allocation may run between the dispose above and
        // this mark, so freed addresses are still unrecycled. The old-node
        // sweep runs first: it also covers old nodes the root mark will free
        // below, which would otherwise disappear before it could report them.
        #[cfg(all(debug_assertions, feature = "gc_generations"))]
        {
            self.gc_heap_mut().dbg_assert_no_stale_old_edges();
            self.dbg_verify_after_minor();
        }

        // A minor never moves the major threshold (that would make majors
        // fire more often). Only the young budget is rebased/adapted.
        self.adapt_young_threshold(young_bytes, freed);
        let used = self.gc_heap().memory_used();
        self.young_base = used;
        self.gc_minor_req = false;
        // Drop weak handles whose instance has been collected.
        #[cfg(all(feature = "zapi", feature = "gc_generations"))]
        self.prune_zapi_instances();
        self.gc_minor_count += 1;
        self.gc_minor_freed = self.gc_minor_freed.saturating_add(freed);
        self.gc_time_ns = self
            .gc_time_ns
            .saturating_add(started.elapsed().as_nanos() as u64);

        log::trace!("[minor] free {freed} bytes");
        freed
    }

    /// Drop weak handles to collected zapi instances. Called at every minor and
    /// major so the list stays bounded even when minors are disabled (`promote
    /// == 0`).
    #[cfg(all(feature = "zapi", feature = "gc_generations"))]
    fn prune_zapi_instances(&mut self) {
        let heap = &self.gc_heap;
        self.zapi_instances.retain(|w| w.upgrade(heap).is_some());
    }

    /// Debug-only validator for the generational write barriers.
    ///
    /// Runs a full mark + sweep right after a minor (before any mutator
    /// allocation), so freed addresses are still unrecycled. gc-lite panics
    /// when a live root or a traced child points at a node the minor just
    /// freed. Finishing the cycle keeps the "old nodes are Black" invariant
    /// that the next minor relies on.
    #[cfg(all(debug_assertions, feature = "gc_generations"))]
    fn dbg_verify_after_minor(&mut self) {
        let partition_id = self.gc_part_id();
        self.gc_heap_mut().mark(partition_id, 0);
        self.shape_registry.remove_unused();
        self.trace_runtime_roots(false);
        self.gc_heap_mut().mark_grays_all();
        for pid in self.gc_heap().partition_ids() {
            if let Some(white) = self.gc_heap_mut().sweep_unlink(pid) {
                self.sweep_white(pid, white);
            }
        }
        // Addresses may be recycled by the next allocation, so the check is
        // only valid for the mark that just ran.
        self.gc_heap_mut().dbg_clear_minor_freed();
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

    /// Set the promotion generation count: `0` disables minors (major-only),
    /// `1` promotes after one minor, `2` after two. Values above 2 clamp to 2.
    /// A no-op without `gc_generations`.
    ///
    /// `0` keeps the generational bookkeeping (young chain, remembered set) but
    /// never schedules or runs a minor; see `docs/gc.md`. It is not the same as
    /// compiling without the feature (that removes the bookkeeping entirely).
    #[inline]
    pub fn gc_set_promote_after(&mut self, x: u8) {
        #[cfg(feature = "gc_generations")]
        {
            let x = x.min(2);
            self.young_promote_after = x;
            if x == 0 {
                self.young_threshold = 0;
                self.gc_minor_req = false;
            } else {
                // Start the young budget from the current heap size and re-derive
                // the threshold for the new promotion mode.
                self.young_base = self.gc_heap.memory_used();
                self.recompute_young_threshold();
            }
        }
        #[cfg(not(feature = "gc_generations"))]
        let _ = x;
    }

    /// Request a minor collection. A no-op without `gc_generations` or when
    /// minors are disabled (`promote == 0`); the next safepoint runs it, unless
    /// a major is also pending.
    #[inline(always)]
    pub const fn gc_minor_request(&mut self) {
        #[cfg(feature = "gc_generations")]
        {
            if self.young_promote_after != 0 {
                self.gc_minor_req = true;
            }
        }
    }

    /// request gc collect
    #[inline(always)]
    pub const fn gc_has_request(&self) -> bool {
        self.gc_req
    }

    pub fn gc_memory_limit(&self) -> usize {
        self.gc_heap().memory_limit()
    }

    /// Bytes currently accounted as live by the GC heap.
    ///
    /// Hosts use this to observe allocation pressure; it is also the value the
    /// automatic-GC threshold is derived from.
    pub fn gc_memory_used(&self) -> usize {
        self.gc_heap().memory_used()
    }

    /// Completed minor collections since runtime creation (always 0 without the
    /// `gc_generations` feature).
    pub const fn gc_minor_count(&self) -> u64 {
        self.gc_minor_count
    }

    /// Completed major collections since runtime creation.
    pub const fn gc_major_count(&self) -> u64 {
        self.gc_major_count
    }

    /// Cumulative bytes reclaimed by minor / major collections.
    pub const fn gc_minor_freed(&self) -> usize {
        self.gc_minor_freed
    }

    pub const fn gc_major_freed(&self) -> usize {
        self.gc_major_freed
    }

    /// Cumulative wall time spent in GC collections, in nanoseconds.
    pub const fn gc_time_ns(&self) -> u64 {
        self.gc_time_ns
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

/// Pure core of the G4.3 young-budget backoff (bounded, no oscillation in the
/// 25–75% band).
#[cfg(feature = "gc_generations")]
fn adapted_threshold(
    current: usize,
    young_bytes: usize,
    freed: usize,
    lower: usize,
    upper: usize,
) -> usize {
    if young_bytes == 0 || current == 0 {
        return current;
    }
    let freed = freed as u128;
    let young = young_bytes as u128;
    if freed * 10 < young {
        // Almost nothing reclaimed: go straight to the major threshold.
        upper
    } else if freed * 4 < young {
        current.saturating_mul(2).min(upper)
    } else if freed * 4 > young * 3 {
        (current / 2).max(lower)
    } else {
        current
    }
}

#[cfg(all(test, feature = "gc_generations"))]
mod young_backoff_tests {
    use super::adapted_threshold;

    #[test]
    fn backoff_is_bounded_and_converges() {
        // 0% reclaimed -> jump to the upper bound
        assert_eq!(adapted_threshold(64, 100, 0, 32, 1000), 1000);
        // 10% reclaimed -> double
        assert_eq!(adapted_threshold(64, 100, 10, 32, 1000), 128);
        // 50% reclaimed -> unchanged (no oscillation)
        assert_eq!(adapted_threshold(64, 100, 50, 32, 1000), 64);
        // 90% reclaimed -> halve, floored
        assert_eq!(adapted_threshold(64, 100, 90, 40, 1000), 40);
        // doubled value is capped by the upper bound
        assert_eq!(adapted_threshold(900, 100, 10, 32, 1000), 1000);
        // empty young budget is a no-op
        assert_eq!(adapted_threshold(64, 0, 0, 32, 1000), 64);
    }
}
