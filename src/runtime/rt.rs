// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, ffi::c_void, process::ExitCode, sync::Arc};

#[cfg(feature = "builtin_weakref")]
use gc_lite::GcTrace;
use gc_lite::{GcHeap, GcPartitionId, GcTraceFn};

use crate::{
    atom::{Atom, AtomInfo, AtomKind, AtomRegistry, JS_ATOM_HASH_PRIVATE, JS_ATOM_HASH_SYMBOL},
    builtins::{
        array_buffer::JSSharedArrayBufferFunctions, promise::JSHostPromiseRejectionTracker,
    },
    class::{init_static_class_defs, static_class_count},
    config::{
        GC_ALLOC_CHECK_INTERVAL, GC_ARENA_CAP, GC_ARENA_MAX_ALLOC, STACK_ARENA_CAP,
        STACK_RESERVE_MARGIN,
    },
    context::ContextPtr,
    error::Exception,
    gc::gc::{GC_TYPE_REGISTRY, GcTraceCallbackFn},
    module::{ModuleLoadFn, ModuleNormalizeFn},
    object::Promise,
    object::{JS_PROP_THROW, JS_PROP_THROW_STRICT, ShapeRegistry},
    stack_frame::{ExecMode, FrameIndex, StackFrameArena},
    string::{StringData, StringRef},
    utils::js_get_stack_pointer,
    value::JSValue,
};

use super::park::ParkState;

/// Default in-process shared-memory backend for `SharedArrayBuffer`.
///
/// The backing store is a plain heap allocation (`Box<[u8]>`) wrapped by the
/// engine in `Arc<RwLock<..>>` (see `ABufData::Shared`). This makes the buffer
/// genuinely shared across all views and contexts within the same process via
/// `Arc` reference counting; it is *not* shared across processes.
///
/// `sab_free` is a no-op: the actual deallocation happens when the last
/// `Arc` strong reference drops.
fn default_sab_funcs() -> JSSharedArrayBufferFunctions {
    fn sab_alloc(_opaque: *mut c_void, capacity: usize) -> Result<Box<[u8]>, i32> {
        let v = vec![0; capacity];
        Ok(v.into_boxed_slice())
    }
    const fn sab_free(_opaque: *mut c_void, _len: usize) {}
    JSSharedArrayBufferFunctions {
        sab_alloc,
        sab_free,
        sab_opaque: std::ptr::null_mut(),
    }
}

/// interrupt returns true to break execution
pub trait InterruptFn: Fn(&RuntimeImpl) -> bool {}
impl<C> InterruptFn for C where C: Fn(&RuntimeImpl) -> bool {}

pub struct RuntimeImpl {
    /// atom registry
    pub(crate) atom_registry: AtomRegistry,
    /// Per-runtime cached values for const atoms (string and symbol).
    pub(crate) const_atom_values: Vec<(u32, StringRef)>,

    /// shape registry
    pub(crate) shape_registry: ShapeRegistry,

    /// C-stack recursion budget in bytes: recursion may consume at most this
    /// many bytes below the eval-entry SP. `0` = auto mode — the guard then
    /// derives the line from the thread's real stack bottom (see
    /// `js_check_stack_overflow`).
    pub(crate) stack_budget: usize,
    pub(crate) stack_limit: usize, // lower stack limit (strict-budget mode only)
    pub(crate) stack_top: usize,   // eval-entry SP; baseline for the strict budget

    /// Bump-arena frame storage for the stackless interpreter.
    /// All active frame headers and data buffers live here.
    pub(crate) frame_arena: StackFrameArena,
    /// Current frame being executed by the trampoline, set before each
    /// iteration. Used by GC to double-trace the active frame's func_obj.
    pub(crate) active_frame_index: FrameIndex,

    /// current exception
    pub(crate) cur_exception: Exception,
    /// Process-level interrupt flag
    pub(crate) interrupt_code: Option<ExitCode>,

    /// pre-compiled regexp in parsing state.
    #[cfg(feature = "builtin_regexp")]
    pub(crate) regexp_precompiled: Vec<crate::builtins::regexp::RegExpData>,

    pub(crate) module_normalize_func: Option<ModuleNormalizeFn>,
    pub(crate) module_load_func: Option<ModuleLoadFn>,
    pub(crate) module_load_opaque: *mut c_void,

    // timestamp for internal use in module evaluation
    pub(crate) module_async_evaluation_next_timestamp: i64,

    /// Base URL of the script being evaluated by the host `eval` entry point.
    ///
    /// Unlike `debug.filename` (only recoverable by walking the live frame
    /// stack), this is bound to the runtime for the duration of a host eval so
    /// that async callbacks (promise reactions, timers) — which run as
    /// independent root frames detached from the caller's frame chain — can
    /// still resolve relative URLs against the correct base. Set in
    /// `Context::eval` and never reset (the next host eval overwrites it), since
    /// async callbacks from the previous eval have already drained by then.
    pub(crate) eval_base_url: Atom,

    /// callbacks to allocate, free and clone SharedArrayBuffers
    pub(crate) sab_funcs: Option<JSSharedArrayBufferFunctions>,

    /// list of living contexts
    pub(crate) context_list: Vec<ContextPtr>,

    /// gc heap
    pub(crate) gc_heap: GcHeap,
    /// gc partition id
    pub(crate) gc_part_hot: GcPartitionId,
    /// Hold currently running job gc tracer, so that GC can trace it.
    pub(crate) gc_job: Option<Box<dyn GcTraceFn>>,
    /// user defined gc trace callbacks (closures, registered via `gc_set_trace_handler`).
    /// Multiple callbacks are supported; each runs at every GC.
    pub(crate) gc_user_trace: Vec<Box<dyn GcTraceFn>>,
    /// fn-pointer gc trace callbacks `(fn, opaque)` — used by native extensions
    /// (zapi `set_gc_trace_func`) to trace JS values held in thread_locals.
    pub(crate) gc_trace_fns: Vec<(GcTraceCallbackFn, *mut std::ffi::c_void)>,
    /// allocation counter for batched should_gc() check
    pub(crate) gc_alloc_count: u32,
    /// number of allocations between should_gc() checks
    pub(crate) gc_alloc_check_interval: u32,
    /// gc request flag
    pub(crate) gc_req: bool,

    /// Rooted pending Promises that must survive GC even if no user code holds a reference.
    pub(crate) gc_pending_promises: Vec<Promise>,
    /// Rooted pending async-function execution states. An AsyncState escapes its
    /// allocating scope at the first suspend; rooting it here (added on
    /// creation, removed at completion in `AsyncState::execute`) guarantees
    /// the resumable state survives GC regardless of promise-reaction edges.
    pub(crate) gc_pending_async_states: Vec<crate::function::AsyncState>,

    /// Per-runtime promise future registry (was thread_local, moved here
    /// to support multiple runtimes per thread).
    #[cfg(feature = "async_support")]
    pub(crate) promise_future_registry: crate::builtins::promise::PromiseFutureRegistry,

    #[deprecated]
    pub(crate) host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    #[deprecated]
    pub(crate) host_promise_rejection_tracker_opaque: *mut c_void,

    pub(crate) interrupt_handler: Option<Box<dyn InterruptFn>>,

    /// Event-loop park state: lets `Parked` rounds suspend the driving thread
    /// (async parker thread / sync condvar) instead of hot-spinning. Shared
    /// externally through [`crate::WakeHandle`] for IO producers to wake us.
    pub(crate) park: Arc<ParkState>,

    /// Monotonically increasing counter for ShapeImpl::uid.
    /// Incremented on every shape allocation to guarantee unique shape IDs.
    /// Used by inline cache to detect ABA reuse of shape memory addresses.
    pub(crate) next_shape_uid: Cell<u32>,

    /// Pending FinalizationRegistry cleanups collected during GC.
    /// Each entry is (callback, held_value, context).
    #[cfg(feature = "builtin_weakref")]
    pub(crate) finrec_pending_cleanups: Vec<(JSValue, JSValue, ContextPtr)>,

    /// Host-controlled per-agent `AgentCanSuspend()` flag. `Atomics.wait`
    /// throws a TypeError when this is false (e.g. the main host thread under
    /// test262's `CanBlockIsFalse` flag), and blocks otherwise.
    pub(crate) agent_can_suspend: bool,

    // ── zapi native extension state ──
    #[cfg(feature = "zapi")]
    pub(crate) zapi_next_serial: std::sync::atomic::AtomicI32,
    #[cfg(feature = "zapi")]
    pub(crate) zapi_body_registry:
        std::sync::Mutex<std::collections::HashMap<i32, (zuqe_zapi::ffi::ZqBodyFn, i32)>>,
    /// Per-runtime zapi class table: each entry pairs the engine-native
    /// `ClassDef` with the extension's ABI `ZqClassDef`. Class ids are assigned
    /// as `static_class_count() + index`, independently per runtime.
    #[cfg(feature = "zapi")]
    pub(crate) zapi_class_defs: Vec<crate::zapi::ZapiClassDef>,

    //
    // ── MULTI-RUNTIME JOIN ──
    //
    pub(crate) join_state: JoinState,
}

/// State of a runtime within a [`JoinNextFuture`](super::JoinNextFuture) stream.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum JoinState {
    /// Being ticked, not yet idle.
    Active = 0,
    /// `tick()` returned Idle, waiting to be yielded.
    Idle = 1,
    /// Already yielded to the caller.
    Yielded = 2,
}

impl Drop for RuntimeImpl {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl RuntimeImpl {
    /// Clonable handle for external producers (IO threads / tokio tasks) to
    /// wake a parked event loop — call `wake()` after pushing new work into a
    /// channel or result slot so the next `tick()` sees it promptly.
    pub fn wake_handle(&self) -> crate::runtime::WakeHandle {
        crate::runtime::WakeHandle(self.park.clone())
    }

    /// Return the cached value (String or Symbol) for a const atom,
    /// minting and caching one on first use. The node is a GC root with
    /// AtomInfo carrying the const atom id, so `to_atom` round-trips and
    /// symbol `===` (id-based) work correctly. Strings use content-based
    /// hash; symbols use `JS_ATOM_HASH_SYMBOL`/`JS_ATOM_HASH_PRIVATE`.
    pub(crate) fn const_atom_value(&mut self, id: u32) -> Result<StringRef, i32> {
        debug_assert!(
            crate::atom::is_const_atom(id),
            "const_atom_value called with non-const atom id {id}"
        );
        if let Some((_, s)) = self.const_atom_values.iter().find(|(k, _)| *k == id) {
            return Ok(*s);
        }

        let atom = Atom::from_id(id);
        let kind = atom.const_kind();
        // The const table only produces String (text_hash) and Symbol kinds.
        // Symbols carry a type-marker atom_hash so generic descriptor
        // inspection (`is_normal_desc_string`) treats the node as a normal
        // well-known symbol; Private_brand keeps its brand marker.
        let atom_hash = match kind {
            AtomKind::Symbol => {
                if atom == Atom::CONST_Private_brand {
                    JS_ATOM_HASH_PRIVATE
                } else {
                    JS_ATOM_HASH_SYMBOL
                }
            }
            _ => atom.const_text_hash(),
        };
        let s = StringRef::create_root(
            self,
            StringData::from_static(atom.const_str()),
            Some(AtomInfo::new(
                std::num::NonZeroU32::new(id).unwrap(),
                kind,
                atom_hash,
            )),
        )?;

        self.const_atom_values.try_reserve(1).map_err(|_| -1)?;
        self.const_atom_values.push((id, s));
        Ok(s)
    }

    /// Enable/disable the fallback quantum poll for the parked event loop.
    ///
    /// `ms > 0`: every park is capped at this interval — a producer that
    /// misses its wake degrades to a cheap periodic poll instead of a lost
    /// wakeup. `ms = 0` (the default): purely event driven — every `Again`
    /// waiting point must have a wake source (wake channel, slot
    /// commit/drop, timer deadline, signal) or the loop sleeps forever.
    /// Call on the runtime's own thread; takes effect on the next park.
    #[cfg(feature = "quantum_poll")]
    pub fn use_quantum_poll(&self, ms: u32) {
        self.park.set_quantum_ms(ms);
    }

    /// Inject the host's deadline waker: called on the JS thread whenever the
    /// event loop parks with a deadline (timer / quantum fallback); the
    /// implementation must arrange for the loop to be woken at (or after)
    /// that instant, e.g. `tokio::spawn(sleep_until(at)).await` +
    /// `WakeHandle::wake`. Async hosts must install one, or timer deadlines
    /// never fire. Call before running any script.
    pub fn set_deadline_waker(&mut self, waker: std::sync::Arc<crate::runtime::DeadlineWaker>) {
        *self.park.deadline_waker.lock().unwrap() = Some(waker);
    }

    pub(super) fn new() -> Result<Self, i32> {
        // create gc heap and partition for this runtime
        let mut gc_heap = GcHeap::new(&GC_TYPE_REGISTRY);
        gc_heap.set_memory_limit(0); // no memory limit
        gc_heap.set_gc_threshold(8); // small threshould to trigger gc ASAP

        let gc_part_id = gc_heap.create_partition(GC_ARENA_CAP, GC_ARENA_MAX_ALLOC);

        // `STACK_ARENA_CAP` is expressed in bytes, calc (bytes / sizeof(JSValue)) for slots capacity
        let arena_cap = STACK_ARENA_CAP / std::mem::size_of::<JSValue>();

        let mut rt = Self {
            atom_registry: AtomRegistry::new(),
            const_atom_values: Vec::new(),
            shape_registry: ShapeRegistry::new(),

            frame_arena: StackFrameArena::with_capacity(arena_cap),
            active_frame_index: FrameIndex::NULL,

            module_load_opaque: std::ptr::null_mut(),
            sab_funcs: Some(default_sab_funcs()),

            eval_base_url: Atom::NULL,

            context_list: Vec::with_capacity(1),

            // No strict budget by default: auto mode, derived from the real
            // thread stack bottom at check time.
            stack_budget: 0,
            stack_top: 0,
            stack_limit: 0,

            cur_exception: Exception::No,
            interrupt_code: None,

            #[cfg(feature = "builtin_regexp")]
            regexp_precompiled: Vec::new(),

            module_normalize_func: None,
            module_load_func: None,
            module_async_evaluation_next_timestamp: 0,

            gc_heap,
            gc_part_hot: gc_part_id,
            gc_job: None,
            gc_req: false,
            gc_alloc_count: 0,
            gc_alloc_check_interval: GC_ALLOC_CHECK_INTERVAL,
            gc_user_trace: Vec::new(),
            gc_trace_fns: Vec::new(),
            gc_pending_promises: Vec::new(),
            gc_pending_async_states: Vec::new(),

            interrupt_handler: None,

            park: Arc::new(ParkState::new()),

            next_shape_uid: Cell::new(1),
            #[cfg(feature = "builtin_weakref")]
            finrec_pending_cleanups: Vec::new(),

            agent_can_suspend: true,

            #[cfg(feature = "async_support")]
            promise_future_registry: crate::builtins::promise::PromiseFutureRegistry::new(),

            #[cfg(feature = "zapi")]
            zapi_next_serial: std::sync::atomic::AtomicI32::new(1),
            #[cfg(feature = "zapi")]
            zapi_body_registry: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "zapi")]
            zapi_class_defs: Vec::new(),

            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            host_promise_rejection_tracker: None,

            join_state: JoinState::Active,
        };

        init_static_class_defs()?;

        rt.refresh_stack_top();

        Ok(rt)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// Total number of classes known to this runtime (static + dynamic).
    #[inline(always)]
    pub(crate) fn class_count(&self) -> usize {
        #[cfg(feature = "zapi")]
        {
            static_class_count() + self.zapi_class_defs.len()
        }
        #[cfg(not(feature = "zapi"))]
        {
            static_class_count()
        }
    }

    /// (Re)arm the C-stack guard line into `stack_limit`. Auto mode: the
    /// bottom of the current thread's real stack plus `STACK_RESERVE_MARGIN` —
    /// fixed for the thread's lifetime, recomputed when re-armed. Strict mode:
    /// `stack_budget` bytes below the eval-entry SP.
    fn update_stack_budget(&mut self) {
        self.stack_limit = if self.stack_budget == 0 {
            // Auto mode: derive from the thread's stack bottom.
            let bottom = crate::utils::js_get_stack_bottom();
            if bottom == 0 {
                if !crate::utils::STACK_BOTTOM_QUERYABLE {
                    // `cfg!` folds this error branch away on capable platforms.
                    log::warn!(
                        "auto mode C-stack guard unavailable: this platform can't \
                         report the thread stack bottom; unbounded recursion will crash \
                         the process. Set a strict stack budget to keep the guard."
                    );
                }
                0 // no guard line
            } else {
                bottom + STACK_RESERVE_MARGIN
            }
        } else {
            self.stack_top - self.stack_budget
        };
    }

    /// Set the C-stack recursion budget in bytes: JS recursion may consume at
    /// most this much native stack below the eval-entry SP. `0` (default) =
    /// auto mode — the guard is derived from the thread's real stack bottom,
    /// keeping `STACK_RESERVE_MARGIN` head-room for the throw path.
    ///
    /// Will throw stack overflow error when stack using exceeds this budget.
    pub fn set_stack_budget(&mut self, nbytes: usize) {
        self.stack_budget = nbytes;
        self.update_stack_budget();
    }

    /// Refresh the eval-entry stack top and re-arm the C-stack guard line.
    /// Called at the outermost eval entry point: after Rust-side init has
    /// unwound, `stack_top` anchors at the shallowest frame so the strict
    /// budget, if set, is measured from there.
    pub(crate) fn refresh_stack_top(&mut self) {
        self.stack_top = js_get_stack_pointer();
        self.update_stack_budget();
    }

    /// Resize the bump-arena used for frame storage. Returns `Err(())` when the
    /// host cannot allocate the requested size (caller raises `throw_stack_fail`).
    pub fn set_stack_arena(&mut self, size: usize) -> Result<(), ()> {
        self.frame_arena.set_arena_capacity(size)
    }

    /// Set the maximum total live virtual stack size in bytes (bump + heap).
    /// Exceeding it raises `throw_stack_depth_exceeded` on the next frame alloc.
    pub const fn set_stack_limit(&mut self, bytes: usize) {
        self.frame_arena.set_stack_limit(bytes);
    }

    /// Peek current exception
    #[inline(always)]
    pub const fn exception(&self) -> Exception {
        self.cur_exception
    }

    #[inline(always)]
    pub fn clear_exception(&mut self) {
        std::mem::take(&mut self.cur_exception);
    }

    /// put a new exception to cur_exception. The old exception is discarded.
    /// The new value must have been removed from scope protection by the caller
    /// (e.g., via `ScopedContext::throw` which calls `remove_node`).
    #[cold]
    pub(crate) fn set_exception(&mut self, exc: Exception) {
        #[cfg(debug_assertions)]
        if let Exception::Error(val) = &exc
            && let Some(n) = val.as_gc_node()
        {
            unsafe {
                debug_assert!(
                    !n.as_ref().is_local(),
                    "exception value must not be LOCAL when stored in cur_exception"
                );

                n.as_ref().debug_assert_node_valid_simple();
            }
        }

        self.cur_exception = exc;
    }

    /// Set the interrupt flag with given exit code.
    /// Unlike set_exception(), this does NOT go through the Exception mechanism.
    pub const fn interrupt(&mut self, code: ExitCode) {
        self.interrupt_code = Some(code);
    }

    /// Take the interrupt exit code, if any. Clears the flag.
    pub const fn take_interrupt(&mut self) -> Option<ExitCode> {
        self.interrupt_code.take()
    }

    /// Get mutable reference to the StackFrameArena for stackless frame ops.
    #[inline(always)]
    pub(crate) const fn stack_frame_arena_mut(&mut self) -> &mut StackFrameArena {
        &mut self.frame_arena
    }

    /// Get immutable reference to the StackFrameArena.
    #[inline(always)]
    pub(crate) const fn stack_frame_arena(&self) -> &StackFrameArena {
        &self.frame_arena
    }

    /// get current running function
    pub(crate) fn current_function(&self) -> JSValue {
        if !self.active_frame_index.is_null() {
            let head = self.frame_arena.frame(self.active_frame_index);
            head.func_obj.into()
        } else {
            JSValue::UNDEFINED
        }
    }

    /// test if current stack-frame is in strict mode.
    pub(crate) fn is_strict_mode(&self) -> bool {
        if !self.active_frame_index.is_null() {
            let idx = self.active_frame_index.as_usize();
            self.frame_arena
                .frames
                .get(idx)
                .is_some_and(|head| head.mode.contains(ExecMode::STRICT))
        } else {
            false
        }
    }

    /// test if allowed to throw error according to `flags`
    #[inline(always)]
    pub fn can_throw(&self, flags: u32) -> bool {
        flags & JS_PROP_THROW != 0 || (flags & JS_PROP_THROW_STRICT != 0 && self.is_strict_mode())
    }

    fn finalize(&mut self) {
        log::trace!("[rt_fin] {self:p}");

        self.cur_exception = Exception::default();

        self.interrupt_code = None;

        // clear pending FinalizationRegistry cleanups (no context to queue them)
        #[cfg(feature = "builtin_weakref")]
        self.finrec_pending_cleanups.clear();

        // Partition release with a host pre-drop pass:
        //
        // Phase 1a (prepare, &GcHeap): clear scope caches + weak slots, take
        //   the node link WITHOUT dropping payloads.
        // Phase 1b (pre-drop, &mut self): run runtime side-effects for dying
        //   payloads (atom removal, object finalize) through the runtime's
        //   own &mut self — no drop_fn executes here, so no re-entrancy.
        // Phase 1c (drop): payload Drop impls are trivial (no RuntimeImpl
        //   access) — re-entrancy is impossible by construction.
        // Phase 2 (dealloc, &mut self): free the memory of all finalized
        //   nodes. No Drop callbacks run here.
        //
        // Before Phase 1 we pop gc_top_scope from all contexts so that
        // ContextImpl::drop finds gc_top_scope == None and skips the
        // gc_heap_mut() access entirely.

        // Pre-step: pop gc_top_scope from all contexts.
        for ctx_ptr in &mut self.context_list {
            drop(ctx_ptr.gc_top_scope.take());
        }

        self.context_list.clear();
        self.shape_registry.clear();

        let pid = self.gc_part_id();
        let link = self.gc_heap().finalize_partition_prepare(pid);
        if let Some(link) = link {
            self.pre_drop_cleanup(&link);

            let registry = self.gc_heap().type_registry();
            gc_lite::GcHeap::sweep_drop_payloads(&link, registry);

            self.gc_heap_mut().dealloc_partition(pid, link);
        }
    }
    /// Process pending FinalizationRegistry cleanups.
    /// Called after GC to queue oneshot jobs for collected targets.
    #[cfg(feature = "builtin_weakref")]
    pub(crate) fn process_finrec_cleanups(&mut self) {
        use crate::microtask::OneshotJob;
        use std::ops::DerefMut;

        let pending = std::mem::take(&mut self.finrec_pending_cleanups);
        if pending.is_empty() {
            return;
        }

        for (callback, held_value, mut ctx_ptr) in pending {
            // Check if the context is still alive
            if !self.context_list.contains(&ctx_ptr) {
                continue;
            }

            let job = OneshotJob {
                runnable: Box::new(move |cx| {
                    callback.call(cx, JSValue::UNDEFINED, &[held_value])?;
                    Ok(())
                }),
                gc_trace: Some(Box::new(move |gcx| {
                    callback.trace(gcx);
                    held_value.trace(gcx);
                })),
            };

            // Push the job directly to the context's oneshot queue
            let ctx_impl: &mut crate::context::ContextImpl = DerefMut::deref_mut(&mut ctx_ptr);
            ctx_impl.oneshot_jobs.push_back(job);
        }
    }
}

pub(crate) fn js_check_stack_overflow(rt: &RuntimeImpl, alloca_size: usize) -> bool {
    // `stack_limit` is the pre-armed guard line (auto or strict budget); the
    // hot path only compares.
    let sp = js_get_stack_pointer() - alloca_size;
    rt.stack_limit > 0 && sp < rt.stack_limit
}
