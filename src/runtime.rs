use std::{
    collections::HashMap,
    ffi::{c_char, c_void},
    num::NonZeroU16,
    rc::Rc,
};

use crate::{
    array_buffer::JSSharedArrayBufferFunctions,
    atom::{JSAtomStruct, JS_FreeAtomRT},
    callable::CallableValue,
    class::{js_arguments_exotic_methods, JSClass},
    context::JSContext,
    function::{
        js_async_function_call, js_async_function_resolve_call, BoundFunctionValue,
        NativeFunctionValue,
    },
    gc::{JSGCObjectHeader, JSGCPhaseEnum},
    generator::{js_generator_function_call, AsyncGeneratorValue},
    list_entry,
    mem::{JSMallocFunctions, JSMallocState, JS_DEFAULT_STACK_SIZE},
    module::{js_module_ns_exotic_methods, JSModuleLoaderFunc, JSModuleNormalizeFunc},
    promise::{JSHostPromiseRejectionTracker, JSJobEntry, PromiseResolvingFuncValue},
    stack_frame::JSStackFrame,
    types::{init_list_head, list_empty, list_foreach, list_foreach_safe, list_head},
    utils::js_get_stack_pointer,
    value::{js_string_exotic_methods, object::JSShapeHandle, JSValue, JS_FreeValueRT},
    JS_CLASS_ARGUMENTS, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOUND_FUNCTION,
    JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_MODULE_NS,
    JS_CLASS_OBJECT, JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    JS_CLASS_STRING, STD_CLASS_DEFS,
};

pub const JS_MAX_LOCAL_VARS: u16 = 65535;
pub const JS_STACK_SIZE_MAX: u16 = 65534;

pub type JSInterruptHandler =
    unsafe fn(rt: *mut JSRuntime, opaque: *mut ::core::ffi::c_void) -> ::core::ffi::c_int;

#[repr(C)]
pub struct JSRuntime {
    pub mf: JSMallocFunctions,
    pub malloc_state: JSMallocState,

    pub rt_info: *const c_char,

    /// atom description slots list.
    /// each element is a atom description Rc<JSAtomStruct>, None indicates the slot is unused.
    /// when an atom is duplicated, the Rc<> is cloned.
    /// index 0 is reserved not for use; index 1 is for atom 1; index 2 is for atom 2, ...
    pub(super) atom_slots: Vec<Option<Rc<JSAtomStruct>>>,
    /// JSString hash to atom vec dict.
    /// Value is a list of atom index. Note: JSString hash may have collison.
    pub(super) atom_hash_lut: HashMap<u32, Vec<NonZeroU16>>,

    /// registered classes
    pub class_array: Vec<JSClass>,

    /// context list
    pub context_list: Vec<Box<JSContext>>,

    /// list of JSGCObjectHeader.link. List of allocated GC objects (used by garbage collector)
    pub gc_obj_list: list_head,
    /// list of JSGCObjectHeader.link. Used during JS_FreeValueRT()
    pub gc_zero_ref_count_list: list_head,
    /// used during GC
    pub tmp_obj_list: list_head,
    pub gc_phase: JSGCPhaseEnum,

    pub malloc_gc_threshold: usize,

    #[cfg(feature = "dump_leaks")]
    pub debug_string_list: list_head, /* list of JSString.link */

    /* stack limitation */
    pub stack_size: usize, /* in bytes, 0 if no limit */
    pub stack_top: usize,
    pub stack_limit: usize, /* lower stack limit */

    pub current_exception: JSValue,
    /* true if inside an out of memory error, to avoid recursing */
    pub in_out_of_memory: bool,

    pub current_stack_frame: *mut JSStackFrame,

    pub interrupt_handler: Option<JSInterruptHandler>,
    pub interrupt_opaque: *mut c_void,

    pub host_promise_rejection_tracker: Option<JSHostPromiseRejectionTracker>,
    pub host_promise_rejection_tracker_opaque: *mut c_void,

    /// list of JSJobEntry.link
    /// TODO: use VecDeque<Box<JSJobEntry>> or LinedList<Box<JSJobEntry>>
    pub job_list: list_head,

    pub module_normalize_func: Option<JSModuleNormalizeFunc>,
    pub module_loader_func: Option<JSModuleLoaderFunc>,
    pub module_loader_opaque: *mut c_void,
    /* timestamp for internal use in module evaluation */
    pub module_async_evaluation_next_timestamp: i64,

    pub can_block: bool, /* TRUE if Atomics.wait can block */

    /// callbacks to allocate, free and clone SharedArrayBuffers
    pub sab_funcs: Option<JSSharedArrayBufferFunctions>,

    /// shapes hash table, use Vec<> to hold conflicted key values.
    pub shape_registry: HashMap<u32, Vec<JSShapeHandle>>,

    pub user_opaque: *mut c_void,
}

impl JSRuntime {
    pub fn new(mf: JSMallocFunctions, opaque: *mut c_void) -> Result<Box<Self>, i32> {
        let mut ms = JSMallocState {
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
                lst.push(None); // element #0 is reserved, not for use.
                lst
            },

            class_array: Vec::new(),

            gc_phase: JSGCPhaseEnum::JS_GC_PHASE_NONE,
            current_stack_frame: std::ptr::null_mut(),
            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            module_loader_opaque: std::ptr::null_mut(),
            sab_funcs: None,

            context_list: Vec::with_capacity(1),
            gc_obj_list: list_head::default(),
            gc_zero_ref_count_list: list_head::default(),
            tmp_obj_list: list_head::default(),
            malloc_gc_threshold: 256 * 1024,
            stack_size: JS_DEFAULT_STACK_SIZE,
            stack_top: 0,
            stack_limit: 0,
            current_exception: JSValue::NULL,
            in_out_of_memory: false,
            interrupt_handler: None,
            host_promise_rejection_tracker: None,
            job_list: list_head::default(),
            module_normalize_func: None,
            module_loader_func: None,
            module_async_evaluation_next_timestamp: 0,
            can_block: false,

            shape_registry: HashMap::new(),

            #[cfg(feature = "dump_leaks")]
            debug_string_list: list_head::default(),

            user_opaque: std::ptr::null_mut(),
            interrupt_opaque: std::ptr::null_mut(),
        });

        unsafe {
            init_list_head(&mut rt.gc_obj_list);
            init_list_head(&mut rt.gc_zero_ref_count_list);
            rt.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;

            #[cfg(feature = "dump_leaks")]
            init_list_head(&mut rt.debug_string_list);

            init_list_head(&mut rt.job_list);

            rt.init_default_atoms()?;

            //
            // register intrinsic classes
            //
            rt.add_classes(&STD_CLASS_DEFS, JS_CLASS_OBJECT)?;

            // setup common classes
            {
                rt.class_array[JS_CLASS_ARGUMENTS as usize].exotic = &js_arguments_exotic_methods;
                rt.class_array[JS_CLASS_STRING as usize].exotic = &js_string_exotic_methods;
                rt.class_array[JS_CLASS_MODULE_NS as usize].exotic = &js_module_ns_exotic_methods;

                rt.class_array[JS_CLASS_C_FUNCTION as usize]
                    .call
                    .replace(NativeFunctionValue::hook_call);
                rt.class_array[JS_CLASS_C_FUNCTION_DATA as usize]
                    .call
                    .replace(CallableValue::hook_call);
                rt.class_array[JS_CLASS_BOUND_FUNCTION as usize]
                    .call
                    .replace(BoundFunctionValue::hook_call);
                rt.class_array[JS_CLASS_GENERATOR_FUNCTION as usize]
                    .call
                    .replace(js_generator_function_call);
            }

            // setup async classes
            {
                rt.class_array[JS_CLASS_PROMISE_RESOLVE_FUNCTION as usize].call =
                    Some(PromiseResolvingFuncValue::hook_call);
                rt.class_array[JS_CLASS_PROMISE_REJECT_FUNCTION as usize].call =
                    Some(PromiseResolvingFuncValue::hook_call);
                rt.class_array[JS_CLASS_ASYNC_FUNCTION as usize].call =
                    Some(js_async_function_call);
                rt.class_array[JS_CLASS_ASYNC_FUNCTION_RESOLVE as usize].call =
                    Some(js_async_function_resolve_call);
                rt.class_array[JS_CLASS_ASYNC_FUNCTION_REJECT as usize].call =
                    Some(js_async_function_resolve_call);
                rt.class_array[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize].call =
                    Some(AsyncGeneratorValue::hook_call);
            }

            if rt.shape_registry.try_reserve(16).is_err() {
                return Err(-1);
            }

            rt.stack_size = JS_DEFAULT_STACK_SIZE;
            JS_UpdateStackTop(rt.as_mut());

            rt.current_exception = JSValue::NULL;
        }

        Ok(rt)
    }
}

impl Drop for JSRuntime {
    fn drop(&mut self) {
        unsafe {
            JS_FreeRuntime(self);
        }
    }
}

#[inline(always)]
pub unsafe fn js_check_stack_overflow(rt: *const JSRuntime, alloca_size: usize) -> bool {
    let sp = js_get_stack_pointer() - alloca_size;
    sp < (*rt).stack_limit
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

pub unsafe fn JS_UpdateStackTop(rt: *mut JSRuntime) {
    (*rt).stack_top = js_get_stack_pointer();
    update_stack_limit(rt);
}

unsafe fn JS_FreeRuntime(rt: &mut JSRuntime) {
    log::warn!("JS_FreeRuntime({rt:p})");

    JS_FreeValueRT(rt, rt.current_exception);

    // free jobs
    {
        list_foreach_safe(&mut rt.job_list, |el| {
            let job_ent = list_entry!(el, JSJobEntry, link);
            for i in 0..(*job_ent).argc {
                JS_FreeValueRT(rt, (*job_ent).args[i as usize]);
            }
            drop(Box::from_raw(job_ent));
        });

        init_list_head(&mut rt.job_list);
    }

    rt.gc();

    #[cfg(feature = "dump_leaks")]
    {
        /* leaking objects */

        /* remove the internal refcounts to display only the object
        referenced externally */
        list_foreach(&mut rt.gc_obj_list, |el| {
            let p = list_entry!(el, JSGCObjectHeader, link);
            (*p).set_gc_mark(0);
        });

        crate::gc::gc_decref(rt);

        let mut header_done = false;
        list_foreach(&mut rt.gc_obj_list, |el| {
            let p = list_entry!(el, JSGCObjectHeader, link);
            if (*p).ref_count != 0 {
                if !header_done {
                    println!("Object leaks:\n");
                    crate::utils::JS_DumpObjectHeader();
                    header_done = true;
                }
                crate::gc::JS_DumpGCObject(rt, p);
            }
        });

        let mut count = 0;
        let mut el = rt.gc_obj_list.next;
        while el != &mut rt.gc_obj_list {
            let p = list_entry!(el, JSGCObjectHeader, link);
            if (*p).ref_count == 0 {
                count += 1;
            }
            el = (*el).next;
        }
        if count != 0 {
            println!("Secondary object leaks: {}", count);
        }
    }

    debug_assert!(list_empty(&rt.gc_obj_list));

    // free the classes
    while let Some(cls) = rt.class_array.pop() {
        if cls.class_id != 0 {
            JS_FreeAtomRT(rt, cls.class_name);
        }
    }

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

    // free atoms
    rt.atom_hash_lut.clear();
    for mut a in rt.atom_slots.drain(..) {
        if let Some(p) = a.take() {
            // println!(
            //     "[O.o] atom {}, rc={}",
            //     p.content().to_str(),
            //     Rc::strong_count(&p)
            // );

            // if __JS_AtomIsConst(p.atom_type().unwrap().atom()) {
            //     drop(p);
            // } else {
            //     debug_assert_eq!(Rc::strong_count(&p), 0);
            // }
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
            let s: *mut JSMallocState = &mut rt.malloc_state;
            println!("MemState: {:?}", *s);

            // FIXME:
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

    // JSRuntime itself is not allocated with js_malloc, so we do not free it here.
    // {
    //     let ms = &mut (*rt).malloc_state;
    //     ((*rt).mf.js_free)(ms, rt.cast());
    // }
}
