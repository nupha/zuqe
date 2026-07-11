// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, ffi::c_void, process::ExitCode};

use gc_lite::{GcHeap, GcPartitionId, GcTraceFn};

#[cfg(feature = "builtin_weakref")]
use gc_lite::GcTrace;

use crate::{
    BUILTIN_CLASS_DEFS, Context, JS_CLASS_OBJECT, ScopedContext, USER_CLASS_ID_START,
    atom::AtomRegistry,
    builtins::{
        array_buffer::JSSharedArrayBufferFunctions, promise::JSHostPromiseRejectionTracker,
    },
    class::{ClassDef, ClassID},
    context::ContextPtr,
    error::Exception,
    gc::gc::GC_TYPE_REGISTRY,
    module::{ModuleLoadFn, ModuleNormalizeFn},
    object::{JS_PROP_THROW, JS_PROP_THROW_STRICT, ShapeRegistry},
    stack_frame::{ExecMode, FrameIndex, StackFrameArena, VirtualFrame},
    utils::js_get_stack_pointer,
    value::JSValue,
};

/// recursive C stack overflow size (in bytes)
#[cfg(debug_assertions)]
const STACK_OVERFLOW_LIMIT: usize = 512 * 1024;
#[cfg(not(debug_assertions))]
const STACK_OVERFLOW_LIMIT: usize = 256 * 1024;

/// VM stack arena capacity (in bytes)
const STACK_ARENA_CAP: usize = 256 * 1024 / std::mem::size_of::<JSValue>();

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
    /// Trampoline virtual call stack pointer, set before each trampoline
    /// iteration and cleared afterward. GC uses this to trace caller
    /// bytecodes that are not reachable through arena heads alone.
    /// SAFETY: Only valid during trampoline execution; null otherwise.
    pub(crate) active_trampoline_vframes: *const VirtualFrame,

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
    /// user defined gc trace callback
    pub(crate) gc_user_trace: Option<Box<dyn GcTraceFn>>,
    /// allocation counter for batched should_gc() check
    pub(crate) gc_alloc_count: u32,
    /// number of allocations between should_gc() checks
    pub(crate) gc_alloc_check_interval: u32,
    /// gc request flag
    pub(crate) gc_req: bool,

    pub(crate) interrupt_handler: Option<Box<dyn InterruptFn>>,

    #[deprecated]
    pub(crate) host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    #[deprecated]
    pub(crate) host_promise_rejection_tracker_opaque: *mut c_void,

    /// Monotonically increasing counter for ShapeImpl::uid.
    /// Incremented on every shape allocation to guarantee unique shape IDs.
    /// Used by inline cache to detect ABA reuse of shape memory addresses.
    pub(crate) next_shape_uid: Cell<u32>,

    /// Pending FinalizationRegistry cleanups collected during GC.
    /// Each entry is (callback, held_value, context).
    #[cfg(feature = "builtin_weakref")]
    pub(crate) finrec_pending_cleanups: Vec<(JSValue, JSValue, ContextPtr)>,
}

impl Drop for RuntimeImpl {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl RuntimeImpl {
    pub(super) fn new() -> Result<Self, i32> {
        // create gc heap and partition for this runtime
        let mut gc_heap = GcHeap::new(&GC_TYPE_REGISTRY);
        gc_heap.set_memory_limit(0); // no memory limit
        gc_heap.set_gc_threshold(8); // small threshould to trigger gc ASAP

        let gc_part_id = gc_heap.create_partition(GC_ARENA_CAP, GC_AERNA_THRESHOULD);
        let arena_cap = STACK_ARENA_CAP / std::mem::size_of::<JSValue>();

        let mut rt = Self {
            atom_registry: AtomRegistry::new(),
            class_registry: Vec::with_capacity(USER_CLASS_ID_START as usize),
            user_class_id_serial: USER_CLASS_ID_START,
            shape_registry: ShapeRegistry::new(),

            frame_arena: StackFrameArena::with_capacity(arena_cap),
            active_trampoline_vframes: std::ptr::null::<VirtualFrame>(),
            active_frame_index: FrameIndex::NULL,

            module_load_opaque: std::ptr::null_mut(),
            sab_funcs: None,

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
            gc_user_trace: None,

            interrupt_handler: None,

            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            host_promise_rejection_tracker: None,
            next_shape_uid: Cell::new(1),
            #[cfg(feature = "builtin_weakref")]
            finrec_pending_cleanups: Vec::new(),
        };

        rt.init_const_atoms();
        rt.register_builtin_classes(BUILTIN_CLASS_DEFS, JS_CLASS_OBJECT)?;

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
    pub fn set_stack_limit(&mut self, bytes: usize) {
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
            && let Some(n) = val.gc_node_ptr()
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

        // Two-phase partition release.
        //
        // Phase 1 (finalize, &self): Drop GC node payloads without freeing
        //   memory.  Since finalize_partition only takes &GcHeap (shared ref),
        //   any reentrant GcHeap access during Drop is safe — the borrow
        //   checker prevents aliasing &mut references.
        //
        // Phase 2 (dealloc, &mut self): Free the memory of all finalized
        //   nodes.  No Drop callbacks run, so no risk of reentrancy.
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

        // Phase 1: finalize — call Drop on all GC nodes with &GcHeap.
        // This reborrows &mut self as &self; the borrow drops when the call
        // returns, restoring &mut self for Phase 2.
        let pid = self.gc_part_id();
        let link = self.gc_heap().finalize_partition(pid);

        // Phase 2: dealloc — free memory, no Drop callbacks.
        if let Some(link) = link {
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
            if !self.context_list.iter().any(|cp| *cp == ctx_ptr) {
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
            && let Some(n) = val.gc_node_ptr()
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
