use std::{
    cell::UnsafeCell,
    collections::{HashMap, VecDeque},
    ffi::{c_char, c_void},
    pin::Pin,
    ptr::NonNull,
    rc::Rc,
};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOUND_FUNCTION,
    JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_MODULE_NS,
    JS_CLASS_OBJECT, JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    JS_CLASS_PROXY, JS_CLASS_STRING, STD_CLASS_DEFS, USER_CLASS_ID_START,
    array_buffer::JSSharedArrayBufferFunctions,
    atom::AtomRegistry,
    callable::NativeFunctionWithData,
    class::{ClassMeta, js_arguments_exotic_methods},
    context::JSContext,
    function::{AsyncFunction, BoundFunction, NFuncPayload, js_async_function_resolve_call},
    gc::JSGCPhaseEnum,
    generator::{AsyncGeneratorFunction, js_generator_function_call},
    mem::{JS_DEFAULT_STACK_SIZE, JSMallocFunctions, JSMallocState},
    module::{JSModuleLoaderFunc, JSModuleNormalizeFunc, js_module_ns_exotic_methods},
    object::{GCObjectWeakref, JSShapeWeakref},
    promise::{JSHostPromiseRejectionTracker, Job, PromiseResolveFunc},
    proxy::{Proxy, js_proxy_exotic_methods},
    regexp::RegExpData,
    stack_frame::StackFrame,
    string::js_string_exotic_methods,
    utils::js_get_stack_pointer,
    value::JSValue,
};

pub const JS_MAX_LOCAL_VARS: u16 = 65535;
pub const JS_STACK_SIZE_MAX: u16 = 65534;

pub type JSInterruptHandler = fn(rt: &JSRuntime, opaque: *mut c_void) -> i32;

pub struct JSRuntime {
    /// registered classes
    pub(crate) class_registry: Vec<ClassMeta>,

    /// list of living contexts
    pub(crate) context_list: Vec<JSContext>,

    /// shapes weakref registry.
    /// use Vec<> to hold conflicted key values.
    pub(crate) shape_registry: HashMap<u32, Vec<JSShapeWeakref>>,

    /* stack limitation */
    pub(crate) stack_size: usize, /* in bytes, 0 if no limit */
    pub(crate) stack_top: usize,
    pub(crate) stack_limit: usize, /* lower stack limit */

    /// current exception value
    pub(crate) cur_exception: JSValue,
    /* true if inside an out of memory error, to avoid recursing */
    pub(crate) in_out_of_memory: bool,

    // stack frame of currently executing function
    pub(crate) cur_stack_frame: *mut Pin<StackFrame>,

    /// pre-compiled regexp in parsing state.
    pub(crate) regexp_precompiled: Vec<RegExpData>,

    pub(crate) interrupt_handler: Option<JSInterruptHandler>,
    pub(crate) interrupt_opaque: *mut c_void,

    pub(crate) host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    pub(crate) host_promise_rejection_tracker_opaque: *mut c_void,

    /// queued jobs
    pub(crate) job_list: VecDeque<Job>,

    pub(crate) module_normalize_func: Option<JSModuleNormalizeFunc>,
    pub(crate) module_loader_func: Option<JSModuleLoaderFunc>,
    pub(crate) module_loader_opaque: *mut c_void,
    /* timestamp for internal use in module evaluation */
    pub(crate) module_async_evaluation_next_timestamp: i64,

    pub(crate) can_block: bool, /* TRUE if Atomics.wait can block */

    /// callbacks to allocate, free and clone SharedArrayBuffers
    pub(crate) sab_funcs: Option<JSSharedArrayBufferFunctions>,

    /// requested to perform gc
    pub(crate) gc_req: bool,
    pub(crate) gc_phase: JSGCPhaseEnum,
    /// List of tracked GC objects (used by garbage collector)
    pub(crate) gc_obj_list: HashMap<usize, GCObjectWeakref>,
    /// Used during JS_FreeValueRT()
    pub(crate) gc_zero_ref_count_list: Vec<GCObjectWeakref>,
    /// used during GC
    pub(crate) tmp_obj_list: Vec<GCObjectWeakref>,

    pub(crate) malloc_gc_threshold: usize,

    pub(crate) user_opaque: *mut c_void,

    pub(crate) mf: JSMallocFunctions,
    pub(crate) malloc_state: JSMallocState,
    pub(crate) rt_info: *const c_char,
}

thread_local! {
    /// List of living runtime raw pointers
    static RUNTIME_LIST: UnsafeCell<Vec<NonNull<JSRuntime>>> = UnsafeCell::new(Vec::with_capacity(1));
}

impl Drop for JSRuntime {
    fn drop(&mut self) {
        self.finalize();

        // unregister from list
        let live_rt = RUNTIME_LIST.with(|list| {
            let list = unsafe { &mut *list.get() };
            let ptr = self as *const Self;

            if let Some(i) = list.iter().position(|p| std::ptr::eq(p.as_ptr(), ptr)) {
                list.swap_remove(i);
                list.len()
            } else {
                unreachable!()
            }
        });

        if live_rt == 0 {
            // no runtime alive, clear all atoms
            AtomRegistry::clear();
        }
    }
}

impl JSRuntime {
    pub fn new(mf: JSMallocFunctions, opaque: *mut c_void) -> Result<Box<Self>, i32> {
        let ms = JSMallocState {
            opaque,
            malloc_limit: usize::MAX,
            ..Default::default()
        };

        let mut rt = Box::new(JSRuntime {
            mf,
            malloc_state: ms,
            rt_info: std::ptr::null(),

            class_registry: Vec::with_capacity(USER_CLASS_ID_START as usize),

            cur_stack_frame: std::ptr::null_mut(),
            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            module_loader_opaque: std::ptr::null_mut(),
            sab_funcs: None,

            context_list: Vec::with_capacity(1),

            gc_req: false,
            gc_phase: JSGCPhaseEnum::JS_GC_PHASE_NONE,
            gc_obj_list: HashMap::with_capacity(128),
            gc_zero_ref_count_list: Vec::new(),
            tmp_obj_list: Vec::new(),
            malloc_gc_threshold: 256 * 1024,

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

            shape_registry: {
                let mut reg = HashMap::new();
                reg.try_reserve(16).map_err(|_| -1)?;
                reg
            },

            user_opaque: std::ptr::null_mut(),
            interrupt_opaque: std::ptr::null_mut(),
        });

        rt.shape_registry.try_reserve(32).map_err(|_| -1)?;

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
                Some(js_async_function_resolve_call);
            rt.class_registry[JS_CLASS_ASYNC_FUNCTION_REJECT as usize].call =
                Some(js_async_function_resolve_call);

            rt.class_registry[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize].call =
                Some(AsyncGeneratorFunction::class_call);
        }

        JS_UpdateStackTop(rt.as_mut());

        // register runtime raw pointer
        RUNTIME_LIST.with(|list| unsafe {
            (&mut *list.get()).push(NonNull::from(rt.as_ref()));
        });

        Ok(rt)
    }

    /// run closure with any runtime
    pub(crate) fn with_any<R>(f: impl FnOnce(&Self) -> R) -> Option<R> {
        RUNTIME_LIST.with(|list| unsafe { (&*list.get()).first().map(|p| p.as_ref()).map(f) })
    }

    /// run closure with any runtime
    pub(crate) fn with_any_mut<R>(f: impl FnOnce(&mut Self) -> R) -> Option<R> {
        RUNTIME_LIST.with(|list| unsafe { (&mut *list.get()).first_mut().map(|r| f(r.as_mut())) })
    }
}

impl JSRuntime {
    fn finalize(&mut self) {
        self.job_list.clear();
        self.cur_exception = JSValue::UNDEFINED;

        #[cfg(debug_assertions)]
        for c in self.context_list.drain(..) {
            debug_assert_eq!(
                Rc::strong_count(&c.0),
                1,
                "Context must have 1 strong ref count when Runtime is dropped"
            );
        }
        #[cfg(not(debug_assertions))]
        self.context_list.clear();

        #[cfg(debug_assertions)]
        {
            // drop runtime will drop objects automatically,
            // this gc() call is for testing purpose only.
            self.gc();
        }

        // #[cfg(feature = "dump_leaks")]
        // {
        //     // leaking objects

        //     // remove the internal refcounts to display only the object referenced externally
        //     for node in rt.gc_obj_list.iter_mut() {
        //         node.set_gc_mark(0);
        //     }
        //     rt.gc_decref();

        //     let mut header_done = false;
        //     for node in rt.gc_obj_list.iter() {
        //         if node.ref_count() != 0 {
        //             if !header_done {
        //                 println!("Object leaks:\n");
        //                 crate::gc::JS_DumpObjectHeader();
        //                 header_done = true;
        //             }
        //             node.gc_dump(rt);
        //         }
        //     }

        //     let count = rt.gc_obj_list.iter().filter(|n| n.ref_count() == 0).count();
        //     if count != 0 {
        //         println!("Secondary object leaks: {}", count);
        //     }
        // }

        // dump atom leaks
        // #[cfg(feature = "dump_leaks")]
        // {
        //     let mut header_done: bool = false;

        //     // only the constant atoms could be left
        //     for (i, a) in rt.atom_slots.iter().enumerate() {
        //         if let Some(p) = a {
        //             let p = p.as_ref();
        //             if i >= JS_ATOM_INIT_COUNT as usize || p.ref_count() != 1 {
        //                 if !header_done {
        //                     header_done = true;
        //                     if !rt.rt_info.is_null() {
        //                         println!(
        //                             "{}:1: atom leakage:",
        //                             std::ffi::CStr::from_ptr(rt.rt_info).to_string_lossy()
        //                         );
        //                     } else {
        //                         println!("Atom leaks: (NANE, ID, REFCNT)");
        //                     }
        //                 }
        //                 if !rt.rt_info.is_null() {
        //                     print!(" ");
        //                 } else {
        //                     print!("    {:6} {:6} ", i, (*p).ref_count());
        //                 }
        //                 match p.atom_type().unwrap() {
        //                     JSAtomType::String(_) => JS_DumpString(rt, p),
        //                     JSAtomType::GlobalSymbol(_) => {
        //                         print!("Symbol.for(");
        //                         JS_DumpString(rt, p);
        //                         print!(")");
        //                     }
        //                     JSAtomType::Symbol(_) => {
        //                         print!("Symbol(");
        //                         JS_DumpString(rt, p);
        //                         print!(")");
        //                     }
        //                     JSAtomType::Private(_) => {
        //                         print!("Private(");
        //                         JS_DumpString(rt, p);
        //                         print!(")");
        //                     }
        //                 }

        //                 if !rt.rt_info.is_null() {
        //                     println!(":{}", (*p).ref_count());
        //                 } else {
        //                     println!("");
        //                 }
        //             }
        //         }
        //     }

        //     if !rt.rt_info.is_null() && header_done {
        //         println!("");
        //     }
        // }

        // ensure shapes and gc objects are freed.
        #[cfg(debug_assertions)]
        {
            self.gc_obj_list.retain(|_, w| {
                if let Some(o) = w.upgrade() {
                    println!("[O.o] live gc: {:?} [{}]", o, o.ref_count());
                    true
                } else {
                    false
                }
            });

            for (_, mut sh_lst) in self.shape_registry.drain() {
                sh_lst.retain(|w| {
                    if let Some(s) = w.upgrade() {
                        s.dump();
                        true
                    } else {
                        false
                    }
                });
                debug_assert!(sh_lst.is_empty(), "{sh_lst:?}");
            }

            debug_assert!(self.gc_obj_list.is_empty(), "{:?}", self.gc_obj_list);
        }

        // free classe defs and associated name atoms
        // let rt = self as *mut Self;
        // for c in self.class_defs.drain(..) {
        //     JS_FreeAtomRT(rt, c.class_name);
        // }
        self.class_registry.clear();

        // guard: all non-const atom should be freed by logic.
        // #[cfg(debug_assertions)]
        // {
        //     for (_, ids) in self.atom_hash_lut.iter() {
        //         for id in ids {
        //             let a = crate::atom::AtomRef(id.get() as u32);
        //             if !a.is_const() {
        //                 println!("[atom leak] {id}: {}", a.to_str(self));
        //             }
        //         }
        //     }
        // }

        // free atoms under rt
        AtomRegistry::purge_for_rt(self);

        #[cfg(debug_assertions)]
        {
            let s = &mut self.malloc_state;
            println!("MemState: {:?}", s);

            // TODO:
            // if (*s).malloc_count > 1 {
            //     if !(*rt).rt_info.is_null() {
            //         print!(
            //             "{}:1: ",
            //             std::ffi::CStr::from_ptr((*rt).rt_info).to_string_lossy()
            //         );
            //     }
            //     println!(
            //         "Memory leak: {} bytes lost in {} block{}",
            //         (*s).malloc_size as usize - std::mem::size_of::<JSRuntime>(),
            //         (*s).malloc_count - 1,
            //         if (*s).malloc_count == 2 { "" } else { "s" }
            //     );
            // }
        }
    }
}

pub(crate) fn js_check_stack_overflow(rt: &JSRuntime, alloca_size: usize) -> bool {
    let sp = js_get_stack_pointer() as usize - alloca_size;
    if rt.stack_limit > 0 {
        sp < rt.stack_limit
    } else {
        false
    }
}

const fn update_stack_limit(rt: &mut JSRuntime) {
    if rt.stack_size == 0 {
        rt.stack_limit = 0; // no limit
    } else {
        rt.stack_limit = rt.stack_top - rt.stack_size;
    }
}

pub(crate) const fn JS_SetMaxStackSize(rt: &mut JSRuntime, stack_size: usize) {
    rt.stack_size = stack_size;
    update_stack_limit(rt);
}

fn JS_UpdateStackTop(rt: &mut JSRuntime) {
    rt.stack_top = js_get_stack_pointer() as usize;
    update_stack_limit(rt);
}
