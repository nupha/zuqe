use std::ffi::{c_char, c_void};

use crate::{
    atom::{
        atom_is_free, consts::JS_ATOM_INIT_COUNT, JSAtomStruct, JSAtomType, JS_FreeAtomRT,
        JS_ATOM_HASH_SYMBOL,
    },
    class::{js_arguments_exotic_methods, js_instantiate_prototype, JSClass},
    context::JSContext,
    function::{
        js_c_function_data_call, js_call_bound_function, js_call_c_function, JSAutoInitFunc,
        JS_InstantiateFunctionListItem2,
    },
    gc::{JSGCObjectHeader, JSGCPhaseEnum, JS_RunGC},
    generator::js_generator_function_call,
    job::JSJobEntry,
    js_std_class_def, list_entry,
    mem::{js_free_rt, JSMallocFunctions, JSMallocState, JS_DEFAULT_STACK_SIZE},
    module::{
        js_module_ns_autoinit, js_module_ns_exotic_methods, JSModuleLoaderFunc,
        JSModuleNormalizeFunc,
    },
    stack_frame::JSStackFrame,
    types::{
        init_list_head, list_del, list_empty, list_foreach, list_head, JSSharedArrayBufferFunctions,
    },
    utils::js_get_stack_pointer,
    value::{
        js_string_exotic_methods,
        object::{init_shape_hash, JSShape},
        JSString, JSValue, JSValueConst, JS_DumpString, JS_FreeValueRT, JS_TAG_OBJECT,
        JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_BOUND_FUNCTION, JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA,
    JS_CLASS_ERROR, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT,
    JS_CLASS_STRING,
};

pub const JS_MAX_LOCAL_VARS: u16 = 65535;
pub const JS_STACK_SIZE_MAX: u16 = 65534;

pub type JSInterruptHandler =
    unsafe fn(rt: *mut JSRuntime, opaque: *mut ::core::ffi::c_void) -> ::core::ffi::c_int;

pub type JSHostPromiseRejectionTracker = unsafe fn(
    ctx: *mut JSContext,
    promise: JSValue,
    reason: JSValue,
    is_handled: ::core::ffi::c_int,
    opaque: *mut ::core::ffi::c_void,
);

#[repr(C)]
pub struct JSRuntime {
    pub mf: JSMallocFunctions,

    pub malloc_state: JSMallocState,
    pub rt_info: *const c_char,

    pub atom_hash_size: i32, /* power of two */
    pub atom_count: i32,
    pub atom_size: i32,
    pub atom_count_resize: i32, /* resize hash table at this count */
    pub atom_hash: *mut u32,

    /// TODO: use HashMap<u32, Box<JSAtomStruct>>
    pub(crate) atom_array: *mut *mut JSAtomStruct,
    pub atom_free_index: i32, /* 0 = none */

    // TODO: use Vec<JSClass>
    pub class_array: *mut JSClass,
    pub class_count: i32, /* size of class_array */

    pub context_list: Vec<Box<JSContext>>,

    /* list of JSGCObjectHeader.link. List of allocated GC objects (used by garbage collector) */
    pub gc_obj_list: list_head,
    /* list of JSGCObjectHeader.link. Used during JS_FreeValueRT() */
    pub gc_zero_ref_count_list: list_head,
    pub tmp_obj_list: list_head, /* used during GC */
    pub gc_phase: JSGCPhaseEnum,
    pub malloc_gc_threshold: usize,

    #[cfg(feature = "dump_leaks")]
    pub string_list: list_head, /* list of JSString.link */

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

    pub job_list: list_head, /* list of JSJobEntry.link */

    pub module_normalize_func: Option<JSModuleNormalizeFunc>,
    pub module_loader_func: Option<JSModuleLoaderFunc>,
    pub module_loader_opaque: *mut c_void,
    /* timestamp for internal use in module evaluation */
    pub module_async_evaluation_next_timestamp: i64,

    pub can_block: bool, /* TRUE if Atomics.wait can block */
    /* used to allocate, free and clone SharedArrayBuffers */
    pub sab_funcs: JSSharedArrayBufferFunctions,

    /* Shape hash table */
    pub shape_hash_bits: i32,
    pub shape_hash_size: i32,
    pub shape_hash_count: i32, /* number of hashed shapes */
    /// shape hash table. use HashMap<u32, Box<JSShape>>
    pub shape_hash: *mut *mut JSShape,

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
            atom_hash: std::ptr::null_mut(),
            atom_array: std::ptr::null_mut(),
            class_array: std::ptr::null_mut(),
            user_opaque: std::ptr::null_mut(),
            gc_phase: JSGCPhaseEnum::JS_GC_PHASE_NONE,
            current_stack_frame: std::ptr::null_mut(),
            interrupt_opaque: std::ptr::null_mut(),
            host_promise_rejection_tracker_opaque: std::ptr::null_mut(),
            module_loader_opaque: std::ptr::null_mut(),
            sab_funcs: JSSharedArrayBufferFunctions::new(),
            shape_hash: std::ptr::null_mut(),
            atom_hash_size: 0,
            atom_count: 0,
            atom_size: 0,
            atom_count_resize: 0,
            atom_free_index: 0,
            class_count: 0,
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
            shape_hash_bits: 0,
            shape_hash_size: 0,
            shape_hash_count: 0,

            #[cfg(feature = "dump_leaks")]
            string_list: list_head::default(),
        });

        unsafe {
            init_list_head(&mut rt.gc_obj_list);
            init_list_head(&mut rt.gc_zero_ref_count_list);
            rt.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;

            #[cfg(feature = "dump_leaks")]
            init_list_head(&mut rt.string_list);

            init_list_head(&mut rt.job_list);

            rt.init_default_atoms()?;

            //
            // initialize builtin class protos
            //
            rt.add_classes(&js_std_class_def, JS_CLASS_OBJECT)?;

            rt.class_array
                .add(JS_CLASS_ARGUMENTS as usize)
                .as_mut()
                .unwrap()
                .exotic = &js_arguments_exotic_methods;

            rt.class_array
                .add(JS_CLASS_STRING as usize)
                .as_mut()
                .unwrap()
                .exotic = &js_string_exotic_methods;

            rt.class_array
                .add(JS_CLASS_MODULE_NS as usize)
                .as_mut()
                .unwrap()
                .exotic = &js_module_ns_exotic_methods;

            rt.class_array
                .add(JS_CLASS_C_FUNCTION as usize)
                .as_mut()
                .unwrap()
                .call
                .replace(js_call_c_function);

            rt.class_array
                .add(JS_CLASS_C_FUNCTION_DATA as usize)
                .as_mut()
                .unwrap()
                .call
                .replace(js_c_function_data_call);

            rt.class_array
                .add(JS_CLASS_BOUND_FUNCTION as usize)
                .as_mut()
                .unwrap()
                .call
                .replace(js_call_bound_function);

            rt.class_array
                .add(JS_CLASS_GENERATOR_FUNCTION as usize)
                .as_mut()
                .unwrap()
                .call
                .replace(js_generator_function_call);

            if init_shape_hash(&mut rt) == -1 {
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
    return sp < (*rt).stack_limit;
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
        let mut el = rt.job_list.next;
        let mut el1 = (*el).next;
        while el != std::ptr::addr_of_mut!(rt.job_list) {
            let job = list_entry!(el, JSJobEntry, link);
            for i in 0..(*job).argc {
                JS_FreeValueRT(rt, *(*job).argv.as_ptr().add(i as usize));
            }
            js_free_rt(rt, job.cast());

            el = el1;
            el1 = (*el).next;
        }
        init_list_head(&mut (*rt).job_list);
    }

    JS_RunGC(rt);

    #[cfg(feature = "dump_leaks")]
    {
        /* leaking objects */

        /* remove the internal refcounts to display only the object
        referenced externally */
        list_foreach(&mut (*rt).gc_obj_list, |el| {
            let p = list_entry!(el, JSGCObjectHeader, link);
            (*p).set_mark(0);
        });

        crate::gc::gc_decref(rt);

        let mut header_done = false;
        list_foreach(&mut (*rt).gc_obj_list, |el| {
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
        while el != &mut (*rt).gc_obj_list {
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

    debug_assert!(list_empty(&mut rt.gc_obj_list));

    // free the classes
    for i in 0..rt.class_count {
        let cl: *mut JSClass = (*rt).class_array.offset(i as isize);
        if (*cl).class_id != 0 {
            JS_FreeAtomRT(rt, (*cl).class_name);
        }
    }
    js_free_rt(rt, (*rt).class_array.cast());

    // bf_context_end(&(*rt).bf_ctx);

    #[cfg(feature = "dump_leaks")]
    /* only the atoms defined in JS_InitAtoms() should be left */
    {
        let mut header_done: bool = false;

        for i in 0..(*rt).atom_size {
            let p = *(*rt).atom_array.offset(i as isize);
            if !atom_is_free(p) {
                // && p->str
                if i >= JS_ATOM_INIT_COUNT as i32 || (*p).header.ref_count != 1 {
                    if !header_done {
                        header_done = true;
                        if !(*rt).rt_info.is_null() {
                            println!(
                                "{}:1: atom leakage:",
                                std::ffi::CStr::from_ptr((*rt).rt_info).to_string_lossy()
                            );
                        } else {
                            println!("Atom leaks: (NANE, ID, REFCNT)");
                        }
                    }
                    if !(*rt).rt_info.is_null() {
                        print!(" ");
                    } else {
                        print!("    {:6} {:6} ", i, (*p).header.ref_count);
                    }
                    match (*p).atom_type() {
                        JSAtomType::String => JS_DumpString(rt, p),
                        JSAtomType::GlobalSymbol => {
                            print!("Symbol.for(");
                            JS_DumpString(rt, p);
                            print!(")");
                        }
                        JSAtomType::Symbol => {
                            if (*p).hash() == JS_ATOM_HASH_SYMBOL {
                                print!("Symbol(");
                                JS_DumpString(rt, p);
                                print!(")");
                            } else {
                                print!("Private(");
                                JS_DumpString(rt, p);
                                print!(")");
                            }
                        }
                        _ => {}
                    }

                    if !(*rt).rt_info.is_null() {
                        println!(":{}", (*p).header.ref_count);
                    } else {
                        println!("");
                    }
                }
            }
        }
        if !(*rt).rt_info.is_null() && header_done {
            println!("");
        }
    }

    // free atoms
    for i in 0..(*rt).atom_size {
        let p = *rt.atom_array.offset(i as isize);
        if !atom_is_free(p) {
            #[cfg(feature = "dump_leaks")]
            list_del(&mut (*p).link);

            js_free_rt(rt, p.cast());
        }
    }
    js_free_rt(rt, rt.atom_array.cast());
    js_free_rt(rt, rt.atom_hash.cast());
    js_free_rt(rt, rt.shape_hash.cast());

    #[cfg(feature = "dump_leaks")]
    {
        if !list_empty(&mut (*rt).string_list) {
            if !(*rt).rt_info.is_null() {
                println!(
                    "{}:1: string leakage:",
                    std::ffi::CStr::from_ptr((*rt).rt_info).to_string_lossy()
                );
            } else {
                println!("String leaks (VALUE, REFCOUNT):\n");
            }

            let mut el = (*rt).string_list.next;
            while el != &mut (*rt).string_list {
                let str = list_entry!(el, JSGCObjectHeader, link).cast::<JSString>();
                if !(*rt).rt_info.is_null() {
                    print!(" ");
                } else {
                    print!("    {:6} ", (*str).header.ref_count);
                }
                JS_DumpString(rt, str);
                if !(*rt).rt_info.is_null() {
                    println!(":{}", (*str).header.ref_count);
                } else {
                    println!("");
                }
                list_del(&mut (*str).link);
                js_free_rt(rt, str.cast());

                el = (*el).next;
            }

            if !(*rt).rt_info.is_null() {
                println!("");
            }
        }

        {
            let s: *mut JSMallocState = &mut (*rt).malloc_state;
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

pub static js_autoinit_func_table: &[JSAutoInitFunc] = &[
    js_instantiate_prototype,        /* JS_AUTOINIT_ID_PROTOTYPE */
    js_module_ns_autoinit,           /* JS_AUTOINIT_ID_MODULE_NS */
    JS_InstantiateFunctionListItem2, /* JS_AUTOINIT_ID_PROP */
];
