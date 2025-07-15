use std::{
    collections::{HashMap, VecDeque},
    ffi::{c_char, c_void},
    num::NonZeroU16,
    pin::Pin,
    rc::Rc,
};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOUND_FUNCTION,
    JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_MODULE_NS,
    JS_CLASS_OBJECT, JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    JS_CLASS_STRING, STD_CLASS_DEFS,
    array_buffer::JSSharedArrayBufferFunctions,
    atom::JS_FreeAtomRT,
    callable::CallableValue,
    class::{JSClass, js_arguments_exotic_methods},
    context::JSContext,
    function::{AsyncFunction, BoundFunction, ObjNativeFunc, js_async_function_resolve_call},
    gc::JSGCPhaseEnum,
    generator::{AsyncGeneratorValue, js_generator_function_call},
    mem::{JS_DEFAULT_STACK_SIZE, JSMallocFunctions, JSMallocState},
    module::{JSModuleLoaderFunc, JSModuleNormalizeFunc, js_module_ns_exotic_methods},
    object::{GCObjectWeakref, JSShapeWeakref},
    promise::{JSHostPromiseRejectionTracker, JSJobEntry, PromiseResolvingFunc},
    stack_frame::StackFrame,
    string::{JSStringInner, js_string_exotic_methods},
    utils::js_get_stack_pointer,
    value::JSValue,
};

pub const JS_MAX_LOCAL_VARS: u16 = 65535;
pub const JS_STACK_SIZE_MAX: u16 = 65534;

pub type JSInterruptHandler = fn(rt: &mut JSRuntime, opaque: *mut c_void) -> i32;

#[repr(C)]
pub struct JSRuntime {
    pub mf: JSMallocFunctions,
    pub malloc_state: JSMallocState,

    pub rt_info: *const c_char,

    /// atom description slots list.
    /// each element is a raw pointer of Rc<JSStringInner>,
    /// null indicates the slot is unused.
    /// when an atom is duplicated, the JSAtomStruct is cloned.
    /// index 0 is reserved not for use; index 1 is for atom 1; index 2 is for atom 2, ...
    pub(crate) atom_slots: Vec<*const JSStringInner>,
    /// JSString hash to atom vec dict.
    /// Value is list of atom indecies because JSString hash may have collison.
    pub(crate) atom_hash_lut: HashMap<u32, Vec<NonZeroU16>>,

    /// registered classes
    pub class_defs: Vec<JSClass>,

    /// context list
    pub context_list: Vec<JSContext>,

    /// requested to perform gc
    pub gc_req: bool,
    pub gc_phase: JSGCPhaseEnum,
    /// List of tracked GC objects (used by garbage collector)
    pub gc_obj_list: HashMap<usize, GCObjectWeakref>,
    /// Used during JS_FreeValueRT()
    pub gc_zero_ref_count_list: Vec<GCObjectWeakref>,
    /// used during GC
    pub tmp_obj_list: Vec<GCObjectWeakref>,

    pub malloc_gc_threshold: usize,

    /* stack limitation */
    pub stack_size: usize, /* in bytes, 0 if no limit */
    pub stack_top: usize,
    pub stack_limit: usize, /* lower stack limit */

    /// current exception value
    pub cur_exception: JSValue,
    /* true if inside an out of memory error, to avoid recursing */
    pub in_out_of_memory: bool,

    // stack frame of currently executing function
    pub cur_stack_frame: *mut Pin<StackFrame>,

    pub interrupt_handler: Option<JSInterruptHandler>,
    pub interrupt_opaque: *mut c_void,

    pub host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    pub host_promise_rejection_tracker_opaque: *mut c_void,

    /// queued jobs
    pub job_list: VecDeque<Box<JSJobEntry>>,

    pub module_normalize_func: Option<JSModuleNormalizeFunc>,
    pub module_loader_func: Option<JSModuleLoaderFunc>,
    pub module_loader_opaque: *mut c_void,
    /* timestamp for internal use in module evaluation */
    pub module_async_evaluation_next_timestamp: i64,

    pub can_block: bool, /* TRUE if Atomics.wait can block */

    /// callbacks to allocate, free and clone SharedArrayBuffers
    pub sab_funcs: Option<JSSharedArrayBufferFunctions>,

    /// shapes weakref registry.
    /// use Vec<> to hold conflicted key values.
    pub shape_registry: HashMap<u32, Vec<JSShapeWeakref>>,

    pub user_opaque: *mut c_void,
}

impl JSRuntime {
    pub fn new(mf: JSMallocFunctions, opaque: *mut c_void) -> Result<Box<Self>, i32> {
        let ms = JSMallocState {
            opaque,
            malloc_limit: usize::MAX,
            ..Default::default()
        };

        // ms.malloc_count += 1;
        // ms.malloc_size += std::mem::size_of::<JSRuntime>() + MALLOC_OVERHEAD;

        let mut rt = Box::new(JSRuntime {
            mf,
            malloc_state: ms,
            rt_info: std::ptr::null(),

            atom_hash_lut: HashMap::with_capacity(256),
            atom_slots: {
                let mut lst = Vec::with_capacity(256);
                lst.push(std::ptr::null()); // element #0 is reserved not for use.
                lst
            },

            class_defs: Vec::new(),

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

        rt.init_default_atoms()?;
        rt.shape_registry.try_reserve(16).map_err(|_| -1)?;

        // register intrinsic classes
        rt.add_classes(&STD_CLASS_DEFS, JS_CLASS_OBJECT)?;

        // setup common classes
        {
            rt.class_defs[JS_CLASS_ARGUMENTS as usize].exotic = &js_arguments_exotic_methods;
            rt.class_defs[JS_CLASS_STRING as usize].exotic = &js_string_exotic_methods;
            rt.class_defs[JS_CLASS_MODULE_NS as usize].exotic = &js_module_ns_exotic_methods;

            rt.class_defs[JS_CLASS_C_FUNCTION as usize]
                .call
                .replace(ObjNativeFunc::hook_call);
            rt.class_defs[JS_CLASS_C_FUNCTION_DATA as usize]
                .call
                .replace(CallableValue::hook_call);
            rt.class_defs[JS_CLASS_BOUND_FUNCTION as usize]
                .call
                .replace(BoundFunction::hook_call);
            rt.class_defs[JS_CLASS_GENERATOR_FUNCTION as usize]
                .call
                .replace(js_generator_function_call);
        }

        // setup async related classes
        {
            rt.class_defs[JS_CLASS_PROMISE_RESOLVE_FUNCTION as usize].call =
                Some(PromiseResolvingFunc::hook_call);
            rt.class_defs[JS_CLASS_PROMISE_REJECT_FUNCTION as usize].call =
                Some(PromiseResolvingFunc::hook_call);

            rt.class_defs[JS_CLASS_ASYNC_FUNCTION as usize].call = Some(AsyncFunction::hook_call);
            rt.class_defs[JS_CLASS_ASYNC_FUNCTION_RESOLVE as usize].call =
                Some(js_async_function_resolve_call);
            rt.class_defs[JS_CLASS_ASYNC_FUNCTION_REJECT as usize].call =
                Some(js_async_function_resolve_call);

            rt.class_defs[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize].call =
                Some(AsyncGeneratorValue::hook_call);
        }

        JS_UpdateStackTop(rt.as_mut());

        Ok(rt)
    }

    fn finalize(&mut self) {
        self.job_list.clear();
        self.cur_exception = JSValue::UNDEFINED;
        self.context_list.clear();

        self.gc();

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
                        true
                    } else {
                        false
                    }
                });
                debug_assert!(sh_lst.is_empty());
            }

            debug_assert!(self.gc_obj_list.is_empty(), "{:?}", self.gc_obj_list);
        }

        // free classe name atoms
        for i in 0..self.class_defs.len() {
            let (class_id, class_name) = {
                let c = &self.class_defs[i];
                (c.class_id, c.class_name)
            };

            // 0: base object class
            if class_id != 0 {
                JS_FreeAtomRT(self, class_name);
            }
        }

        // free atom descriptors
        self.atom_hash_lut.clear();
        for a in self.atom_slots.drain(..) {
            if !a.is_null() {
                drop(unsafe { Rc::from_raw(a) });
            }
        }

        // dump string leakage
        #[cfg(feature = "dump_leaks")]
        {
            // if !list_empty(&mut rt.debug_string_list) {
            //     if !rt.rt_info.is_null() {
            //         println!(
            //             "{}:1: string leakage:",
            //             std::ffi::CStr::from_ptr(rt.rt_info).to_string_lossy()
            //         );
            //     } else {
            //         println!("String leaks:\n");
            //     }

            //     list_foreach_safe(&mut rt.debug_string_list, |el| {
            //         let str = list_entry!(el, JSGCObjectHeader, link).cast::<JSString>();

            //         if !rt.rt_info.is_null() {
            //             print!(" ");
            //         } else {
            //             print!("    {:6} ", (*str).ref_count());
            //         }

            //         JS_DumpString(rt, str);

            //         if !rt.rt_info.is_null() {
            //             println!(":{}", (*str).ref_count());
            //         } else {
            //             println!("");
            //         }

            //         list_del(&mut (*str).link);
            //         drop(Box::from_raw(str));
            //     });

            //     if !rt.rt_info.is_null() {
            //         println!("");
            //     }
            // }

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
}

impl Drop for JSRuntime {
    fn drop(&mut self) {
        self.finalize();
    }
}

#[inline(always)]
pub fn js_check_stack_overflow(rt: &JSRuntime, alloca_size: usize) -> bool {
    let sp = js_get_stack_pointer() as usize - alloca_size;
    if rt.stack_limit > 0 {
        sp < rt.stack_limit
    } else {
        false
    }
}

pub unsafe fn update_stack_limit(rt: *mut JSRuntime) {
    if (*rt).stack_size == 0 {
        (*rt).stack_limit = 0; // no limit
    } else {
        (*rt).stack_limit = (*rt).stack_top - (*rt).stack_size;
    }
}

pub unsafe fn JS_SetMaxStackSize(rt: *mut JSRuntime, stack_size: usize) {
    (*rt).stack_size = stack_size;
    update_stack_limit(rt);
}

pub fn JS_UpdateStackTop(rt: *mut JSRuntime) {
    unsafe {
        (*rt).stack_top = js_get_stack_pointer() as usize;
        update_stack_limit(rt);
    }
}
