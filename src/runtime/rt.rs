use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    pin::Pin,
};

use gc_lite::{Collect, GcContext, GcError, GcRef, PartitionId};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOUND_FUNCTION,
    JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_MODULE_NS,
    JS_CLASS_OBJECT, JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    JS_CLASS_PROXY, JS_CLASS_STRING, STD_CLASS_DEFS, USER_CLASS_ID_START,
    array_buffer::JSSharedArrayBufferFunctions,
    callable::NativeFunctionWithData,
    class::{ClassMeta, js_arguments_exotic_methods},
    context::ContextRef,
    function::{AsyncFuncState, AsyncFunction, BoundFunction, NFuncPayload},
    generator::{AsyncGeneratorFunction, js_generator_function_call},
    mem::{JS_DEFAULT_STACK_SIZE, JSMallocFunctions, JSMallocState},
    module::{JSModuleLoaderFunc, JSModuleNormalizeFunc, js_module_ns_exotic_methods},
    object::ShapeRegistry,
    promise::{JSHostPromiseRejectionTracker, Job, PromiseResolveFunc},
    proxy::{Proxy, js_proxy_exotic_methods},
    regexp::RegExpData,
    resource::LocalResource,
    runtime::Runtime,
    stack_frame::StackFrame,
    string::js_string_exotic_methods,
    utils::js_get_stack_pointer,
    value::JSValue,
};

pub const JS_MAX_LOCAL_VARS: u16 = 65535;
pub const JS_STACK_SIZE_MAX: u16 = 65534;

pub type JSInterruptHandler = fn(rt: &RuntimeImpl, opaque: *mut c_void) -> i32;

pub struct RuntimeImpl {
    /// registered classes
    pub(crate) class_registry: Vec<ClassMeta>,

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

    pub(crate) interrupt_handler: Option<JSInterruptHandler>,
    pub(crate) interrupt_opaque: *mut c_void,

    pub(crate) host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    pub(crate) host_promise_rejection_tracker_opaque: *mut c_void,

    pub(crate) module_normalize_func: Option<JSModuleNormalizeFunc>,
    pub(crate) module_loader_func: Option<JSModuleLoaderFunc>,
    pub(crate) module_loader_opaque: *mut c_void,
    /* timestamp for internal use in module evaluation */
    pub(crate) module_async_evaluation_next_timestamp: i64,

    pub(crate) can_block: bool, /* TRUE if Atomics.wait can block */

    /// callbacks to allocate, free and clone SharedArrayBuffers
    pub(crate) sab_funcs: Option<JSSharedArrayBufferFunctions>,

    /// list of living contexts
    pub(crate) context_list: Vec<ContextRef>,

    /// queued jobs
    pub(crate) job_list: VecDeque<Job>,

    /// gc heap
    gc_heap: GcContext,
    /// gc partion id
    gc_part: PartitionId,
    pub(crate) gc_threshold: usize,
    /// gc requested mark
    pub(crate) gc_req: bool,

    pub(crate) user_opaque: *mut c_void,

    pub(crate) mf: JSMallocFunctions,
    pub(crate) malloc_state: JSMallocState,
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
    fn new(mf: JSMallocFunctions, opaque: *mut c_void) -> Result<Box<Self>, i32> {
        let ms = JSMallocState {
            opaque,
            malloc_limit: usize::MAX,
            ..Default::default()
        };

        // create gc for this runtime
        let mut gc_heap = GcContext::new();
        let gc_part = gc_heap.create_partition("runtime".to_string(), None);

        let mut rt = Box::new(RuntimeImpl {
            mf,
            malloc_state: ms,

            class_registry: Vec::with_capacity(USER_CLASS_ID_START as usize),

            cur_stack_frame: std::ptr::null_mut(),
            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            module_loader_opaque: std::ptr::null_mut(),
            sab_funcs: None,

            context_list: Vec::with_capacity(1),

            gc_heap,
            gc_part,
            gc_threshold: 256 * 1024,
            gc_req: false,

            stack_size: JS_DEFAULT_STACK_SIZE,
            stack_top: 0,
            stack_limit: 0,
            cur_exception: JSValue::NULL,
            regexp_precompiled: Vec::new(),
            in_out_of_memory: false,
            interrupt_handler: None,
            host_promise_rejection_tracker: None,
            job_list: VecDeque::with_capacity(8),
            module_normalize_func: None,
            module_loader_func: None,
            module_async_evaluation_next_timestamp: 0,
            can_block: false,

            shape_registry: ShapeRegistry::new(),

            user_opaque: std::ptr::null_mut(),
            interrupt_opaque: std::ptr::null_mut(),
        });

        // register builtin classes
        rt.register_std_classes(&STD_CLASS_DEFS, JS_CLASS_OBJECT)?;

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
                Some(AsyncFuncState::class_call);
            rt.class_registry[JS_CLASS_ASYNC_FUNCTION_REJECT as usize].call =
                Some(AsyncFuncState::class_call);

            rt.class_registry[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize].call =
                Some(AsyncGeneratorFunction::class_call);
        }

        JS_UpdateStackTop(rt.as_mut());

        Ok(rt)
    }

    /// create a new runtime
    pub fn create(mf: JSMallocFunctions, opaque: *mut c_void) -> Result<Runtime, i32> {
        let rt: Box<RuntimeImpl> = Self::new(mf, opaque)?;
        let h = Runtime::new(rt.as_ref());

        // register runtime
        LocalResource::with_runtimes_mut(|slot| {
            slot.push(rt);
        });

        Ok(h)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub const fn gc_part(&self) -> PartitionId {
        self.gc_part
    }

    #[inline(always)]
    pub const fn gc_heap(&self) -> &GcContext {
        &self.gc_heap
    }

    #[inline(always)]
    pub const fn gc_heap_mut(&mut self) -> &mut GcContext {
        &mut self.gc_heap
    }

    /// alloc data in gc heap
    #[inline(always)]
    pub fn gc_alloc<T: Collect>(&mut self, data: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap.alloc(self.gc_part, data)
    }

    /// collect garbage
    pub fn gc_collect(&mut self) -> usize {
        self.gc_heap.collect_garbage(self.gc_part)
    }

    /// get current exception value
    #[inline(always)]
    pub const fn exception(&self) -> JSValue {
        self.cur_exception
    }

    /// get current exception value, then reset current_exception to null.
    #[inline(always)]
    pub const fn take_exception(&mut self) -> JSValue {
        std::mem::replace(&mut self.cur_exception, JSValue::NULL)
    }

    /// set new obj to current_exception, returns the old exception value.
    /// Note: returned value should be freed by caller
    #[cold]
    pub(crate) const fn set_exception(&mut self, obj: JSValue) {
        self.cur_exception = obj;
    }

    /// run closure with current stack frame
    pub fn with_cur_stack_frame<R>(&self, f: impl FnOnce(&StackFrame) -> R) -> Option<R> {
        unsafe { self.cur_stack_frame.cast::<StackFrame>().as_ref().map(f) }
    }

    /// get current active stack frame
    pub const fn cur_stack_frame(&self) -> Option<&StackFrame> {
        unsafe { self.cur_stack_frame.cast::<StackFrame>().as_ref() }
    }

    /// get current active stack frame
    pub const fn cur_stack_frame_mut(&mut self) -> Option<&mut StackFrame> {
        unsafe { self.cur_stack_frame.cast::<StackFrame>().as_mut() }
    }

    fn finalize(&mut self) {
        self.cur_exception = JSValue::UNDEFINED;
        self.job_list.clear();

        // #[cfg(debug_assertions)]
        // {
        //     // self.gc();

        //     // ensure shapes and gc objects are freed.
        //     // self.gc_obj_list.retain(|_, w| {
        //     //     if let Some(o) = w.upgrade() {
        //     //         println!("[O.o] live gc: {:?} [{}]", o, o.ref_count());
        //     //         true
        //     //     } else {
        //     //         false
        //     //     }
        //     // });

        //     // debug_assert!(self.gc_obj_list.is_empty(), "{:?}", self.gc_obj_list);
        // }

        // clear all shapes in registry,
        // don't bother shape drop trait to do unregister.
        self.shape_registry.clear();

        // remove gc partition
        let part = self.gc_part();
        self.gc_heap_mut().remove_partition(part)

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

const fn update_stack_limit(rt: &mut RuntimeImpl) {
    if rt.stack_size == 0 {
        rt.stack_limit = 0; // no limit
    } else {
        rt.stack_limit = rt.stack_top - rt.stack_size;
    }
}

pub(crate) const fn JS_SetMaxStackSize(rt: &mut RuntimeImpl, stack_size: usize) {
    rt.stack_size = stack_size;
    update_stack_limit(rt);
}

fn JS_UpdateStackTop(rt: &mut RuntimeImpl) {
    rt.stack_top = js_get_stack_pointer() as usize;
    update_stack_limit(rt);
}
