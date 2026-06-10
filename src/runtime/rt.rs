// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ffi::c_void, pin::Pin, ptr::NonNull};

use gc_lite::{GcHeap, GcPartitionId, GcTraceCtx};

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
    microtask::{OneshotJob, PollingJob},
    module::{ModuleLoadFn, ModuleNormalizeFn},
    object::{JS_PROP_THROW, JS_PROP_THROW_STRICT, ShapeRegistry},
    stack_frame::{ExecMode, StackFrame},
    utils::js_get_stack_pointer,
    value::JSValue,
};

const STACK_SIZE_BYTES_DEFAULT: usize = 720 * 1024; // 512 * 1024;

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

    //
    // execution stack
    //
    pub(crate) stack_size: usize,  /* in bytes, 0 if no limit */
    pub(crate) stack_limit: usize, /* lower stack limit */
    pub(crate) stack_top: usize,
    /// current stack frame to execute bytecode or native function
    pub(crate) cur_stack_frame: *mut Pin<StackFrame>,
    /// current stack frame depth
    #[cfg(debug_assertions)]
    stack_depth: i32,

    /// current exception
    pub(super) cur_exception: Exception,

    /// pre-compiled regexp in parsing state.
    #[cfg(feature = "builtin_regexp")]
    pub(crate) regexp_precompiled: Vec<crate::builtins::regexp::RegExpData>,

    pub(crate) module_normalize_func: Option<ModuleNormalizeFn>,
    pub(crate) module_load_func: Option<ModuleLoadFn>,
    pub(crate) module_load_opaque: *mut c_void,

    /* timestamp for internal use in module evaluation */
    pub(crate) module_async_evaluation_next_timestamp: i64,

    /// callbacks to allocate, free and clone SharedArrayBuffers
    pub(crate) sab_funcs: Option<JSSharedArrayBufferFunctions>,

    /// list of living contexts
    pub(crate) context_list: Vec<ContextPtr>,

    /// gc heap
    pub(crate) gc_heap: GcHeap,
    /// gc partition id
    pub(crate) gc_part_hot: GcPartitionId,
    /// Current running oneshot job held here during tick() so GC can trace it.
    pub(crate) gc_oneshot_job: Option<OneshotJob>,
    /// Jobs temporarily held here during tick() so GC can trace them.
    /// Jobs are pushed before run(), popped on completion (Done),
    /// or kept for retry (Again) then re-enqueued to job_list at end of tick.
    pub(crate) gc_polling_jobs: Vec<PollingJob>,
    /// user defined gc trace callback
    pub(crate) gc_user_trace: Option<Box<dyn Fn(&mut GcTraceCtx)>>,
    /// gc request flag
    pub(crate) gc_req: bool,

    pub(crate) interrupt_handler: Option<Box<dyn InterruptFn>>,

    #[deprecated]
    pub(crate) host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    #[deprecated]
    pub(crate) host_promise_rejection_tracker_opaque: *mut c_void,
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

        let gc_part_id = gc_heap.create_partition();

        let mut rt = Self {
            atom_registry: AtomRegistry::new(),
            class_registry: Vec::with_capacity(USER_CLASS_ID_START as usize),
            user_class_id_serial: USER_CLASS_ID_START,
            shape_registry: ShapeRegistry::new(),

            cur_stack_frame: std::ptr::null_mut(),
            #[cfg(debug_assertions)]
            stack_depth: 0,

            module_load_opaque: std::ptr::null_mut(),
            sab_funcs: None,

            context_list: Vec::with_capacity(1),

            stack_size: STACK_SIZE_BYTES_DEFAULT,
            stack_top: 0,
            stack_limit: 0,

            cur_exception: Exception::No,

            #[cfg(feature = "builtin_regexp")]
            regexp_precompiled: Vec::new(),

            module_normalize_func: None,
            module_load_func: None,
            module_async_evaluation_next_timestamp: 0,

            gc_heap,
            gc_part_hot: gc_part_id,
            gc_user_trace: None,
            gc_polling_jobs: Vec::with_capacity(4),
            gc_oneshot_job: None,
            gc_req: false,

            interrupt_handler: None,

            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            host_promise_rejection_tracker: None,
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
        if self.stack_size == 0 {
            self.stack_limit = 0; // no limit
        } else {
            self.stack_limit = self.stack_top - self.stack_size;
        }
    }

    /// set max stack size. 0 for unlimited
    pub const fn set_stack_size(&mut self, stack_size: usize) {
        self.stack_size = stack_size;
        self.update_stack_limit();
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

    /// get stack frame for current running function
    #[inline(always)]
    pub(crate) const fn current_stack_frame(&self) -> Option<&StackFrame> {
        unsafe { self.cur_stack_frame.cast::<StackFrame>().as_ref() }
    }

    /// get stack frame for current running function
    #[inline(always)]
    pub(crate) const fn current_stack_frame_mut(&mut self) -> Option<&mut StackFrame> {
        unsafe { self.cur_stack_frame.cast::<StackFrame>().as_mut() }
    }

    /// get current running function
    pub(crate) fn current_function(&self) -> JSValue {
        self.current_stack_frame()
            .map(|s| s.func_obj.into())
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// Pushes a new stack frame onto the execution stack
    ///
    /// # Safety
    /// The caller must ensure the frame pointer is valid
    pub(crate) fn push_stack_frame(&mut self, mut stack_frame: NonNull<StackFrame>) {
        debug_assert!(unsafe { stack_frame.as_ref().parent().is_none() });
        debug_assert_ne!(
            self.cur_stack_frame.cast::<()>(),
            stack_frame.as_ptr().cast::<()>()
        );

        if self.cur_stack_frame.is_null() {
            #[cfg(debug_assertions)]
            debug_assert_eq!(self.stack_depth, 0);
            self.cur_stack_frame = stack_frame.cast::<Pin<StackFrame>>().as_ptr();
        } else {
            unsafe {
                stack_frame.as_mut().prev = self.cur_stack_frame;
            }
            self.cur_stack_frame = stack_frame.cast::<Pin<StackFrame>>().as_ptr();
        }

        #[cfg(debug_assertions)]
        {
            self.stack_depth += 1;
        }
    }

    /// Pops the current stack frame, restore current frame stack to previouse
    pub(crate) fn pop_stack_frame(&mut self) -> *mut Pin<StackFrame> {
        debug_assert!(!self.cur_stack_frame.is_null());

        let sf = self.cur_stack_frame;
        unsafe {
            self.cur_stack_frame = (*sf.cast::<StackFrame>()).prev;
            (*sf.cast::<StackFrame>()).prev = std::ptr::null_mut();
        }

        #[cfg(debug_assertions)]
        {
            self.stack_depth -= 1;
            debug_assert!(self.stack_depth >= 0);

            if self.stack_depth == 0 {
                debug_assert!(self.cur_stack_frame.is_null());
            }
        }

        sf
    }

    /// test if current stack-frame is in strict mode
    pub(crate) fn is_strict_mode(&self) -> bool {
        self.current_stack_frame()
            .is_some_and(|sf| sf.mode.contains(ExecMode::STRICT))
    }

    /// test if allowed to throw error according to `flags`
    #[inline(always)]
    pub fn can_throw(&self, flags: u32) -> bool {
        flags & JS_PROP_THROW != 0 || (flags & JS_PROP_THROW_STRICT != 0 && self.is_strict_mode())
    }

    fn finalize(&mut self) {
        log::trace!("[rt_fin] {self:p}");

        debug_assert!(self.cur_stack_frame.is_null());

        self.cur_exception = Exception::default();
        self.context_list.clear();
        self.shape_registry.clear();

        // remove root partition: dealloc all nodes live in it.
        let pid = self.gc_part_id();
        self.gc_heap_mut()
            .remove_partition(pid, GcHeap::DUMMY_DISPOSE_CALLBACK);

        // All scope stacks should have been released by ContextImpl::drop.
        // GcHeap::drop will clean up any remaining scopes as a safety net.

        // debug_assert_eq!(
        //     self.gc_heap().nodes_iter(root_scope_id).count(),
        //     0,
        //     "should not have live gc nodes"
        // );

        // class unregister callbacks
        let class_registry = std::mem::take(&mut self.class_registry);
        for c in class_registry {
            if let Some(f) = c.on_unregister {
                f(self); // Note: class_registry is already cleared; do NOT reference other ClassDef entries.
            }
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

fn JS_UpdateStackTop(rt: &mut RuntimeImpl) {
    rt.stack_top = js_get_stack_pointer();
    rt.update_stack_limit();
}
