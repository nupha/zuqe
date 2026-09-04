// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, ffi::c_void, process::ExitCode, sync::Arc};

#[cfg(feature = "builtin_weakref")]
use gc_lite::GcTrace;
use gc_lite::{GcHeap, GcPartitionId, GcTraceFn};

use crate::{
    Context, ScopedContext, USER_CLASS_ID_START,
    atom::{Atom, AtomRegistry},
    builtins::{
        array_buffer::JSSharedArrayBufferFunctions, promise::JSHostPromiseRejectionTracker,
    },
    class::{ClassDef, ClassID},
    context::ContextPtr,
    error::Exception,
    gc::gc::{GC_TYPE_REGISTRY, GcTraceCallbackFn},
    module::{ModuleLoadFn, ModuleNormalizeFn},
    object::Promise,
    object::{JS_PROP_THROW, JS_PROP_THROW_STRICT, ShapeRegistry},
    register_all_builtin_classes,
    stack_frame::{ExecMode, FrameIndex, StackFrameArena},
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

/// recursive C stack overflow size (in bytes)
#[cfg(debug_assertions)]
const STACK_OVERFLOW_LIMIT: usize = 512 * 1024;
#[cfg(not(debug_assertions))]
const STACK_OVERFLOW_LIMIT: usize = 256 * 1024;

/// VM stack arena capacity (in bytes)
const STACK_ARENA_CAP: usize = 256 * 1024;

const GC_ARENA_CAP: usize = 64 * 1024;
const GC_AERNA_THRESHOULD: usize = 16 * 1024;
/// number of allocations between should_gc() checks.
const GC_ALLOC_CHECK_INTERVAL: u32 = 32;

/// interrupt returns true to break execution
pub trait InterruptFn: Fn(&RuntimeImpl) -> bool {}
impl<C> InterruptFn for C where C: Fn(&RuntimeImpl) -> bool {}

pub struct RuntimeImpl {
    /// atom registry
    pub(crate) atom_registry: AtomRegistry,
    /// registered classes
    pub(crate) class_registry: Vec<ClassDef>,
    pub(crate) user_class_id_serial: ClassID,
    /// shape registry
    pub(crate) shape_registry: ShapeRegistry,

    pub(crate) stack_overflow: usize, // in bytes, 0 if no limit (C-stack recursion guard)
    pub(crate) stack_limit: usize,    // lower stack limit
    pub(crate) stack_top: usize,

    /// Bump-arena frame storage for the stackless interpreter.
    /// All active frame headers and data buffers live here.
    pub(crate) frame_arena: StackFrameArena,
    /// Current frame being executed by the trampoline, set before each
    /// iteration. Used by GC to double-trace the active frame's func_obj.
    pub(crate) active_frame_index: FrameIndex,

    /// current exception
    pub(super) cur_exception: Exception,
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

    // ── zapi native extension state ──
    #[cfg(feature = "zapi")]
    pub(crate) zapi_body_registry:
        std::sync::Mutex<std::collections::HashMap<i32, (zuqe_zapi::ffi::ZapiBodyFn, i32)>>,
    #[cfg(feature = "zapi")]
    pub(crate) zapi_next_token: std::sync::atomic::AtomicI32,
    #[cfg(feature = "zapi")]
    pub(crate) zapi_class_map:
        std::sync::Mutex<std::collections::HashMap<crate::class::ClassID, usize>>,

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

        let gc_part_id = gc_heap.create_partition(GC_ARENA_CAP, GC_AERNA_THRESHOULD);

        // `STACK_ARENA_CAP` is expressed in bytes, calc (bytes / sizeof(JSValue)) for slots capacity
        let arena_cap = STACK_ARENA_CAP / std::mem::size_of::<JSValue>();

        let mut rt = Self {
            atom_registry: AtomRegistry::new(),
            class_registry: Vec::with_capacity(USER_CLASS_ID_START as usize),
            user_class_id_serial: USER_CLASS_ID_START,
            shape_registry: ShapeRegistry::new(),

            frame_arena: StackFrameArena::with_capacity(arena_cap),
            active_frame_index: FrameIndex::NULL,

            module_load_opaque: std::ptr::null_mut(),
            sab_funcs: Some(default_sab_funcs()),

            eval_base_url: Atom::NULL,

            context_list: Vec::with_capacity(1),

            stack_overflow: STACK_OVERFLOW_LIMIT,
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

            #[cfg(feature = "async_support")]
            promise_future_registry: crate::builtins::promise::PromiseFutureRegistry::new(),

            #[cfg(feature = "zapi")]
            zapi_body_registry: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "zapi")]
            zapi_next_token: std::sync::atomic::AtomicI32::new(1),
            #[cfg(feature = "zapi")]
            zapi_class_map: std::sync::Mutex::new(std::collections::HashMap::new()),

            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            host_promise_rejection_tracker: None,

            join_state: JoinState::Active,
        };

        rt.init_const_atoms();
        register_all_builtin_classes(&mut rt)?;

        JS_UpdateStackTop(&mut rt);

        Ok(rt)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    const fn update_stack_limit(&mut self) {
        self.stack_limit = if self.stack_overflow == 0 {
            0 // no limit
        } else {
            self.stack_top - self.stack_overflow
        };
    }

    /// set max C-stack recursion depth (in bytes). 0 for unlimited
    pub const fn set_stack_overflow(&mut self, stack_size: usize) {
        self.stack_overflow = stack_size;
        self.update_stack_limit();
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

        // class unregister callbacks
        let class_registry = std::mem::take(&mut self.class_registry);
        for c in class_registry {
            if let Some(f) = c.on_unregister {
                f(self); // Note: class_registry is already cleared; do NOT reference other ClassDef entries.
            }
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

impl<'c> ScopedContext<'c> {
    /// Take the current exception and add it to `self` scope for protection.
    ///
    /// After this call, the exception value (if any) becomes a LOCAL node
    /// in the `self` scope and will be protected from GC until the scope ends.
    pub fn take_exception(&mut self) -> Exception {
        let exc = std::mem::take(&mut self.rt_mut().cur_exception);
        if let Exception::Error(val) = &exc
            && let Some(n) = val.as_gc_node()
        {
            self.gc_state().add_non_local(n);
        }
        exc
    }
}

impl<'c> Context<'c> {
    /// Take the current exception and add it to root scope for protection.
    ///
    /// After this call, the exception value (if any) becomes a LOCAL node
    /// in the root scope and will be protected from GC until the scope ends.
    pub fn take_exception(&mut self) -> Exception {
        self.root_scope().take_exception()
    }
}

pub(crate) fn js_check_stack_overflow(rt: &RuntimeImpl, alloca_size: usize) -> bool {
    let sp = js_get_stack_pointer() - alloca_size;
    if rt.stack_limit > 0 {
        sp < rt.stack_limit
    } else {
        false
    }
}

pub(crate) fn JS_UpdateStackTop(rt: &mut RuntimeImpl) {
    rt.stack_top = js_get_stack_pointer();
    rt.update_stack_limit();
}
