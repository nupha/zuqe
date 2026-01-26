use std::{ffi::c_void, pin::Pin, ptr::NonNull};

use gc_lite::{GcHeap, GcPartitionId, GcTracer};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOUND_FUNCTION,
    JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_MODULE_NS,
    JS_CLASS_OBJECT, JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    JS_CLASS_PROXY, JS_CLASS_STRING, STD_CLASS_DEFS, USER_CLASS_ID_START,
    builtins::{
        array_buffer::JSSharedArrayBufferFunctions,
        generator::{AsyncGeneratorFunction, js_generator_function_call},
        promise::{JSHostPromiseRejectionTracker, PromiseResolveFunc},
        proxy::{Proxy, js_proxy_exotic_methods},
        regexp::RegExpData,
    },
    class::{ClassMeta, js_arguments_exotic_methods},
    context::ContextPtr,
    function::{AsyncFunction, AsyncState, BoundFunction, NFuncPayload, NativeFunctionWithData},
    module::{ModuleLoadFn, ModuleNormalizeFn, js_module_ns_exotic_methods},
    object::{ShapeImpl, ShapeRegistry},
    resource::LocalResource,
    stack_frame::StackFrame,
    string::js_string_exotic_methods,
    utils::js_get_stack_pointer,
    value::JSValue,
};

const STACK_SIZE_BYTES_DEFAULT: usize = 720 * 1024; // 512 * 1024;

/// interrupt returns true to break execution
pub trait InterruptFn: Fn(&RuntimeImpl) -> bool {}
impl<C> InterruptFn for C where C: Fn(&RuntimeImpl) -> bool {}

pub struct RuntimeImpl {
    /// registered classes
    pub(crate) class_registry: Vec<ClassMeta>,
    /// registered shapes
    pub(crate) shape_registry: ShapeRegistry,

    //
    // execution stack
    //
    pub(crate) stack_size: usize,  /* in bytes, 0 if no limit */
    pub(crate) stack_limit: usize, /* lower stack limit */
    pub(crate) stack_top: usize,
    pub(crate) cur_stack_frame: *mut Pin<StackFrame>,

    /// current exception value
    pub(crate) cur_exception: JSValue,
    /* true if inside an out of memory error, to avoid recursing */
    pub(crate) in_out_of_memory: bool,

    /// pre-compiled regexp in parsing state.
    pub(crate) regexp_precompiled: Vec<RegExpData>,

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
    /// gc partion id
    pub(crate) gc_part: GcPartitionId,
    pub(crate) gc_threshold: usize,
    /// user defined gc trace callback
    pub(crate) gc_user_trace: Option<Box<dyn Fn(&mut GcTracer)>>,
    /// gc trace of current running job
    pub(crate) gc_cur_job_trace: Option<Box<dyn Fn(&mut GcTracer)>>,
    /// gc request flag
    pub(crate) gc_req: bool,
    /// tick stop flag
    pub(crate) stop: Option<i32>,

    pub(crate) interrupt_handler: Option<Box<dyn InterruptFn>>,

    #[deprecated]
    pub(crate) host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    #[deprecated]
    pub(crate) host_promise_rejection_tracker_opaque: *mut c_void,
}

impl Drop for RuntimeImpl {
    fn drop(&mut self) {
        self.finalize();

        let this = self.id();

        // unregister rt
        if let Ok(has_live) = LocalResource::try_with_runtimes_mut(|slot| {
            if let Some(i) = slot.iter().position(|x| x.id() == this) {
                slot.swap_remove(i);
            }
            !slot.is_empty()
        }) && !has_live
        {
            // no runtime alive, clear all user atoms.
            // SAFETY: use try_with_atom to avoid corrupt in case thread local is destroyed when runtime drop is calling.
            let _ = LocalResource::try_with_atoms_mut(|reg| {
                reg.clear_user_atoms();
            });
        }
    }
}

impl RuntimeImpl {
    pub(super) fn new() -> Result<Box<Self>, i32> {
        // create gc heap for this runtime
        let mut gc_heap = GcHeap::new();
        let gc_part = gc_heap.create_partition("runtime".to_string(), None);

        let mut rt = Box::new(Self {
            class_registry: Vec::with_capacity(USER_CLASS_ID_START as usize),

            cur_stack_frame: std::ptr::null_mut(),
            module_load_opaque: std::ptr::null_mut(),
            sab_funcs: None,

            context_list: Vec::with_capacity(1),

            stack_size: STACK_SIZE_BYTES_DEFAULT,
            stack_top: 0,
            stack_limit: 0,
            cur_exception: JSValue::NULL,
            regexp_precompiled: Vec::new(),
            in_out_of_memory: false,
            interrupt_handler: None,

            module_normalize_func: None,
            module_load_func: None,
            module_async_evaluation_next_timestamp: 0,

            shape_registry: ShapeRegistry::new(),

            gc_heap,
            gc_part,
            gc_threshold: 256 * 1024,
            gc_user_trace: None,
            gc_cur_job_trace: None,
            gc_req: false,
            stop: None,

            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            host_promise_rejection_tracker: None,
        });

        // register builtin classes
        rt.register_builtin_classes(&STD_CLASS_DEFS, JS_CLASS_OBJECT)?;

        // setup common classes
        {
            rt.class_registry[JS_CLASS_ARGUMENTS as usize]
                .exotic
                .replace(Box::new(js_arguments_exotic_methods));

            rt.class_registry[JS_CLASS_STRING as usize]
                .exotic
                .replace(Box::new(js_string_exotic_methods));

            rt.class_registry[JS_CLASS_MODULE_NS as usize]
                .exotic
                .replace(Box::new(js_module_ns_exotic_methods));

            rt.class_registry[JS_CLASS_C_FUNCTION as usize]
                .call
                .replace(NFuncPayload::class_call);
            rt.class_registry[JS_CLASS_C_FUNCTION_DATA as usize]
                .call
                .replace(NativeFunctionWithData::class_call);
            rt.class_registry[JS_CLASS_BOUND_FUNCTION as usize]
                .call
                .replace(BoundFunction::class_call);
            rt.class_registry[JS_CLASS_GENERATOR_FUNCTION as usize]
                .call
                .replace(js_generator_function_call);

            // setup Proxy
            rt.class_registry[JS_CLASS_PROXY as usize]
                .exotic
                .replace(Box::new(js_proxy_exotic_methods));
            rt.class_registry[JS_CLASS_PROXY as usize]
                .call
                .replace(Proxy::class_call);
        }

        // setup async related classes
        {
            rt.class_registry[JS_CLASS_PROMISE_RESOLVE_FUNCTION as usize].call =
                Some(PromiseResolveFunc::class_call);
            rt.class_registry[JS_CLASS_PROMISE_REJECT_FUNCTION as usize].call =
                Some(PromiseResolveFunc::class_call);

            rt.class_registry[JS_CLASS_ASYNC_FUNCTION as usize].call =
                Some(AsyncFunction::class_call);
            rt.class_registry[JS_CLASS_ASYNC_FUNCTION_RESOLVE as usize].call =
                Some(AsyncState::class_call);
            rt.class_registry[JS_CLASS_ASYNC_FUNCTION_REJECT as usize].call =
                Some(AsyncState::class_call);

            rt.class_registry[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize].call =
                Some(AsyncGeneratorFunction::class_call);
        }

        JS_UpdateStackTop(rt.as_mut());

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

    /// get current exception value
    #[inline(always)]
    pub const fn exception(&self) -> JSValue {
        self.cur_exception
    }

    /// get current exception value, then reset current_exception to null.
    pub fn take_exception(&mut self) -> JSValue {
        std::mem::replace(&mut self.cur_exception, JSValue::NULL)
    }

    /// set new obj to current_exception, returns the old exception value.
    /// Note: returned value should be freed by caller
    #[cold]
    pub(crate) const fn set_exception(&mut self, obj: JSValue) {
        self.cur_exception = obj;
    }

    /// get current stack frame
    #[inline(always)]
    pub(crate) const fn current_stack_frame(&self) -> Option<&StackFrame> {
        unsafe { self.cur_stack_frame.cast::<StackFrame>().as_ref() }
    }

    /// get current stack frame
    #[inline(always)]
    pub(crate) const fn current_stack_frame_mut(&mut self) -> Option<&mut StackFrame> {
        unsafe { self.cur_stack_frame.cast::<StackFrame>().as_mut() }
    }

    /// Pushes a new stack frame onto the execution stack
    ///
    /// # Safety
    /// The caller must ensure the frame pointer is valid
    pub(crate) fn push_stack_frame(&mut self, mut stack_frame: NonNull<StackFrame>) {
        debug_assert_ne!(
            self.cur_stack_frame.cast::<()>(),
            stack_frame.as_ptr().cast::<()>()
        );

        if self.cur_stack_frame.is_null() {
            self.cur_stack_frame = stack_frame.cast::<Pin<StackFrame>>().as_ptr();
        } else {
            unsafe {
                stack_frame.as_mut().prev = self.cur_stack_frame;
            }
            self.cur_stack_frame = stack_frame.cast::<Pin<StackFrame>>().as_ptr();
        }
    }

    /// Pops the current stack frame, restore current frame stack to previouse
    pub(crate) fn pop_stack_frame(&mut self) -> *mut Pin<StackFrame> {
        if self.cur_stack_frame.is_null() {
            std::ptr::null_mut()
        } else {
            let sf = self.cur_stack_frame;
            unsafe {
                self.cur_stack_frame = (*sf.cast::<StackFrame>()).prev;
                (*sf.cast::<StackFrame>()).prev = std::ptr::null_mut();
            }
            sf
        }
    }

    fn finalize(&mut self) {
        self.cur_exception = JSValue::UNDEFINED;
        self.context_list.clear();

        // clear all shapes in registry,
        // don't bother shape drop trait to do unregister.
        self.shape_registry.clear();

        let part = self.gc_part();

        if let Some(type_shape) = self.gc_heap().type_idx_of::<ShapeImpl>() {
            // purge all gc objects except ShapeImpl
            self.gc_heap_mut()
                .sweep_ex(part, true, None, Some(&[type_shape]), true);
        }

        // purge all rest nodes
        self.gc_heap_mut().sweep_ex(part, true, None, None, false);

        debug_assert_eq!(
            self.gc_heap().partition_node_iter(part).count(),
            0,
            "should not have live gc nodes"
        );

        self.gc_heap_mut().remove_partition(part);

        // no need
        // self.class_registry.clear();
    }
}

pub(crate) fn js_check_stack_overflow(rt: &RuntimeImpl, alloca_size: usize) -> bool {
    let sp = js_get_stack_pointer() as usize - alloca_size;
    if rt.stack_limit > 0 {
        sp < rt.stack_limit
    } else {
        false
    }
}

fn JS_UpdateStackTop(rt: &mut RuntimeImpl) {
    rt.stack_top = js_get_stack_pointer() as usize;
    rt.update_stack_limit();
}
