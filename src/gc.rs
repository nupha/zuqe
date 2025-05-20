use crate::{
    context::JSContext,
    error::JS_NATIVE_ERROR_COUNT,
    function::{free_function_bytecode, JSAsyncFunctionState, JSFunctionBytecode, JSVarRef},
    list_entry,
    module::{JSExportTypeEnum, JSModuleDef},
    runtime::JSRuntime,
    types::{
        init_list_head, list_add_tail, list_count, list_del, list_find, list_foreach,
        list_foreach_safe, list_head,
    },
    utils::JS_DumpObjectHeader,
    value::{
        object::{
            js_autoinit_get_realm, JSObject, JSProperty, JSShape, JS_PROP_AUTOINIT, JS_PROP_GETSET,
            JS_PROP_TMASK, JS_PROP_VARREF,
        },
        JSTag, JSValueConst, JS_TAG_FUNCTION_BYTECODE, JS_TAG_OBJECT, JS_VALUE_GET_PTR,
        JS_VALUE_GET_TAG, JS_VALUE_HAS_REF_COUNT,
    },
    JS_CLASS_OBJECT, JS_MKPTR,
};

pub type JS_MarkFunc = unsafe fn(rt: *mut JSRuntime, gp: *mut JSGCObjectHeader);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSGCObjectTypeEnum {
    JS_GC_OBJ_TYPE_JS_OBJECT,
    JS_GC_OBJ_TYPE_FUNCTION_BYTECODE,
    JS_GC_OBJ_TYPE_SHAPE,
    JS_GC_OBJ_TYPE_VAR_REF,
    JS_GC_OBJ_TYPE_ASYNC_FUNCTION,
    JS_GC_OBJ_TYPE_JS_CONTEXT,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSGCPhaseEnum {
    JS_GC_PHASE_NONE,
    JS_GC_PHASE_DECREF,
    JS_GC_PHASE_REMOVE_CYCLES,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct JSGCObjectHeader {
    pub ref_count: i32, // must come first, 32-bit

    // gc_obj_type: 4 bits
    // mark:  4 bits used by the GC
    bitfields: u8,

    dummy1: u8,  /* not used by the GC */
    dummy2: u16, /* not used by the GC */

    pub link: list_head,
}

impl JSGCObjectHeader {
    pub fn new(ref_count: i32) -> Self {
        Self {
            ref_count,
            bitfields: 0,
            dummy1: 0,
            dummy2: 0,
            link: list_head::default(),
        }
    }

    pub fn gc_obj_type(&self) -> JSGCObjectTypeEnum {
        unsafe { std::mem::transmute(self.bitfields >> 4) }
    }
    pub fn set_gc_obj_type(&mut self, t: JSGCObjectTypeEnum) {
        self.bitfields = (self.bitfields & 0x0f) | ((t as u8) << 4);
    }

    #[inline(always)]
    pub fn gc_mark(&self) -> u8 {
        self.bitfields & 0x0f
    }

    #[inline(always)]
    pub fn set_gc_mark(&mut self, m: u8) {
        self.bitfields = (self.bitfields & 0xf0) | (m & 0x0f)
    }
}

pub unsafe fn free_zero_refcount(rt: *mut JSRuntime) {
    (*rt).gc_phase = JSGCPhaseEnum::JS_GC_PHASE_DECREF;
    loop {
        let el = (*rt).gc_zero_ref_count_list.next;
        if el == &mut (*rt).gc_zero_ref_count_list {
            break;
        }

        let p = list_entry!(el, JSGCObjectHeader, link);
        debug_assert_eq!((*p).ref_count, 0);

        free_gc_object(rt, p);
    }
    (*rt).gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;
}

pub unsafe fn JS_MarkValue(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    if JS_VALUE_HAS_REF_COUNT(val) {
        match JS_VALUE_GET_TAG(val) {
            JS_TAG_OBJECT | JS_TAG_FUNCTION_BYTECODE => {
                mark_func(rt, JS_VALUE_GET_PTR(val).cast());
            }
            _ => {}
        }
    }
}

pub unsafe fn js_autoinit_mark(rt: *mut JSRuntime, pr: *mut JSProperty, mark_func: JS_MarkFunc) {
    mark_func(rt, &mut (*js_autoinit_get_realm(pr)).header);
}

unsafe fn mark_children(rt: *mut JSRuntime, gp: *mut JSGCObjectHeader, mark_func: JS_MarkFunc) {
    match (*gp).gc_obj_type() {
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT => {
            let p = gp.cast::<JSObject>();

            mark_func(rt, &mut (*p).shape_mut().unwrap().header);

            // Mark all the fields
            for (idx, prs) in (*p).shape().unwrap().props.values().enumerate() {
                let pr = (*p).prop_values.get_mut(idx).unwrap();

                if prs.flags() & JS_PROP_TMASK != 0 {
                    match prs.flags() & JS_PROP_TMASK {
                        JS_PROP_GETSET => {
                            if !pr.u.getset.getter.is_null() {
                                mark_func(rt, &mut (*pr.u.getset.getter).header);
                            }
                            if !pr.u.getset.setter.is_null() {
                                mark_func(rt, &mut (*pr.u.getset.setter).header);
                            }
                        }
                        JS_PROP_VARREF => {
                            // Note: the tag does not matter provided it is a GC object
                            mark_func(rt, &mut (*(*pr).u.var_ref).u0.header);
                        }
                        JS_PROP_AUTOINIT => {
                            js_autoinit_mark(rt, pr, mark_func);
                        }
                        _ => {}
                    }
                } else {
                    JS_MarkValue(rt, pr.u.value, mark_func);
                }
            }

            if (*p).class_id() != JS_CLASS_OBJECT {
                if let Some(f) = (*rt).class_array[(*p).class_id() as usize].gc_mark {
                    f(rt, JS_MKPTR!(JSTag::Object, p.cast()), mark_func);
                }
            }
        }
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
            // The template objects can be part of a cycle
            let b = gp.cast::<JSFunctionBytecode>();
            if let Some(cpool) = (*b).cpools_slice() {
                cpool.iter().for_each(|v| JS_MarkValue(rt, *v, mark_func));
            }
            if !(*b).realm.is_null() {
                mark_func(rt, &mut (*(*b).realm).header);
            }
        }
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF => {
            let var_ref = gp.cast::<JSVarRef>();
            if (*var_ref).is_detached() {
                JS_MarkValue(rt, *(*var_ref).pvalue, mark_func);
            } else if !(*var_ref).value_or_links.links.async_func.is_null() {
                mark_func(
                    rt,
                    &mut (*(*var_ref).value_or_links.links.async_func).header,
                );
            }
        }
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
            unimplemented!("mark_children() for JS_GC_OBJ_TYPE_ASYNC_FUNCTION")

            // let s = gp as *mut JSAsyncFunctionState;
            // let sf = &mut (*s).frame;
            // let mut sp: *mut JSValue;

            // if (*s).is_completed == 0 {
            //     JS_MarkValue(rt, (*sf).cur_func, mark_func);
            //     JS_MarkValue(rt, (*s).this_val, mark_func);
            //     // sf->cur_sp = NULL if the function is running
            //     if !(*sf).cur_sp.is_null() {
            //         // If the function is running, cur_sp is not known so we
            //         // cannot mark the stack. Marking the variables is not needed
            //         // because a running function cannot be part of a removable cycle
            //         sp = (*sf).arg_buf;
            //         while sp < (*sf).cur_sp {
            //             JS_MarkValue(rt, *sp, mark_func);
            //             sp = sp.offset(1);
            //         }
            //     }
            // }
            // JS_MarkValue(rt, (*s).resolving_funcs[0], mark_func);
            // JS_MarkValue(rt, (*s).resolving_funcs[1], mark_func);
        }
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_SHAPE => {
            let sh = gp.cast::<JSShape>();
            if !(*sh).proto.is_null() {
                mark_func(rt, &mut (*(*sh).proto).header);
            }
        }
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_CONTEXT => {
            let ctx = gp as *mut JSContext;
            JS_MarkContext(rt.as_mut().unwrap(), ctx.as_mut().unwrap(), mark_func);
        }
    }
}

pub unsafe fn gc_decref_child(rt: *mut JSRuntime, p: *mut JSGCObjectHeader) {
    debug_assert!(!p.is_null());
    debug_assert!(p.is_aligned());
    debug_assert!((*p).ref_count > 0);

    (*p).ref_count -= 1;
    if (*p).ref_count == 0 && (*p).gc_mark() == 1 {
        list_del(&mut (*p).link);
        list_add_tail(&mut (*p).link, &mut (*rt).tmp_obj_list);
    }
}

/// decrease the refcount of all the children of GC objects in the `gc_obj_list` chain,
/// and move those GC objects with zero refcount to `tmp_obj_list`
pub unsafe fn gc_decref(rt: &mut JSRuntime) {
    init_list_head(&mut (*rt).tmp_obj_list);

    print!(
        "[O.o][gc_decref] gc_obj_list={}, tmp_obj_list={}",
        list_count(&mut (*rt).gc_obj_list),
        list_count(&mut (*rt).tmp_obj_list)
    );

    list_foreach_safe(&mut rt.gc_obj_list, |el| {
        let p = list_entry!(el, JSGCObjectHeader, link);
        debug_assert_eq!((*p).gc_mark(), 0);

        mark_children(rt, p, gc_decref_child);
        (*p).set_gc_mark(1);

        if (*p).ref_count == 0 {
            list_del(&mut (*p).link);
            list_add_tail(&mut (*p).link, &mut rt.tmp_obj_list);
        }
    });

    println!(
        " -> gc_obj_list={}, tmp_obj_list={} [/O.o]",
        list_count(&mut (*rt).gc_obj_list),
        list_count(&mut (*rt).tmp_obj_list)
    );
}

unsafe fn gc_scan_incref_child(rt: *mut JSRuntime, p: *mut JSGCObjectHeader) {
    (*p).ref_count += 1;
    if (*p).ref_count == 1 {
        /* ref_count was 0: remove from tmp_obj_list and add at the
        end of gc_obj_list */
        list_del(&mut (*p).link);
        list_add_tail(&mut (*p).link, &mut (*rt).gc_obj_list);
        p.as_mut().unwrap().set_gc_mark(0); /* reset the mark for the next GC call */
    }
}
unsafe fn gc_scan_incref_child2(rt: *mut JSRuntime, p: *mut JSGCObjectHeader) {
    (*p).ref_count += 1;
}

/// keep the objects with refcount > 0 and their children.
///
/// check objects in `gc_obj_list`, find those having zero refcount,
/// then migrate them from `gc_obj_list` to `gc_tmp_list`.
pub unsafe fn gc_scan(rt: &mut JSRuntime) {
    print!(
        "[O.o][gc_scan] gc_obj_list={}, tmp_obj_list={}",
        list_count(&mut rt.gc_obj_list),
        list_count(&mut rt.tmp_obj_list),
    );

    list_foreach(&mut rt.gc_obj_list, |el| {
        let p = list_entry!(el, JSGCObjectHeader, link);
        debug_assert!((*p).ref_count > 0);

        // reset the mark for the next GC call
        (*p).set_gc_mark(0);

        mark_children(rt, p, |rt: *mut JSRuntime, p: *mut JSGCObjectHeader| {
            (*p).ref_count += 1;

            if (*p).ref_count == 1 {
                // ref_count was 0, `p` is now in `tmp_obj_list`,
                // restore it from `tmp_obj_list` to `gc_obj_list`
                debug_assert!(
                    list_find(&mut (*p).link, &mut (*rt).tmp_obj_list),
                    "zeroed refcount object must in tmp_obj_list"
                );

                list_del(&mut (*p).link);
                list_add_tail(&mut (*p).link, &mut (*rt).gc_obj_list);

                // reset the mark for the next GC call
                (*p).set_gc_mark(0);
            }
        });
    });

    // restore the refcount of the objects to be deleted.
    list_foreach(&mut rt.tmp_obj_list, |el| {
        let p = list_entry!(el, JSGCObjectHeader, link);
        mark_children(rt, p, |_, p: *mut JSGCObjectHeader| {
            // debug_assert_eq!((*p).ref_count, 0); not nessesarily 0?
            (*p).ref_count += 1;
        });
    });

    println!(
        " -> gc_obj_list={}, tmp_obj_list={} [/O.o]",
        list_count(&mut (*rt).gc_obj_list),
        list_count(&mut (*rt).tmp_obj_list),
    );
}

/// To free object in tmp_obj_list
pub unsafe fn gc_free_cycles(rt: *mut JSRuntime) {
    #[cfg(feature = "dump_gc_free")]
    let mut header_done = false;

    (*rt).gc_phase = JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES;

    loop {
        let el = (*rt).tmp_obj_list.next;
        if el == &mut (*rt).tmp_obj_list {
            break;
        }

        let p = list_entry!(el, JSGCObjectHeader, link);

        // Only need to free the GC object associated with JS values or async functions.
        // The rest will be automatically removed because they must be referenced by them
        match (*p).gc_obj_type() {
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT
            | JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE
            | JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
                #[cfg(feature = "dump_gc_free")]
                {
                    if !header_done {
                        println!("Freeing cycles:");
                        JS_DumpObjectHeader();
                        header_done = true;
                    }
                    JS_DumpGCObject(rt, p);
                }

                free_gc_object(rt, p);
            }
            _ => {
                list_del(&mut (*p).link);
                list_add_tail(&mut (*p).link, &mut (*rt).gc_zero_ref_count_list);
            }
        }
    }

    (*rt).gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;

    // drop memory allocated for gc obj
    list_foreach_safe(&mut (*rt).gc_zero_ref_count_list, |el| {
        let p = list_entry!(el, JSGCObjectHeader, link);
        match (*p).gc_obj_type() {
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT => {
                drop(Box::from_raw(p.cast::<JSObject>()));
            }
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
                drop(Box::from_raw(p.cast::<JSFunctionBytecode>()));
            }
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
                drop(Box::from_raw(p.cast::<JSAsyncFunctionState>()));
            }
            _ => {
                unreachable!()
            }
        }
    });

    init_list_head(&mut (*rt).gc_zero_ref_count_list);
}

impl JSRuntime {
    /// Run GC
    pub fn gc(&mut self) {
        unsafe {
            // decrement the reference of the children of each object. mark = 1 after this pass.
            gc_decref(self);
            // keep the GC objects with a non zero refcount and their childs
            gc_scan(self);
            // free the GC objects in a cycle
            gc_free_cycles(self);
        }
    }
}

#[deprecated(note = "use JSRuntime::gc() instead")]
pub fn JS_RunGC(rt: &mut JSRuntime) {
    rt.gc();
}

pub unsafe fn add_gc_object(rt: *mut JSRuntime, h: *mut JSGCObjectHeader, tp: JSGCObjectTypeEnum) {
    // println!(
    //     "[add_gc_object] gc_head={h:p}, el={:p}, type={tp:?}",
    //     &(*h).link
    // );

    (*h).set_gc_mark(0);
    (*h).set_gc_obj_type(tp);
    list_add_tail(&mut (*h).link, &mut (*rt).gc_obj_list);
}

/// remove the node from gc_obj_list
#[inline(always)]
pub unsafe fn remove_gc_object(h: *mut JSGCObjectHeader) {
    list_del(&mut (*h).link);
}

pub unsafe fn free_gc_object(rt: *mut JSRuntime, gp: *mut JSGCObjectHeader) {
    match (*gp).gc_obj_type() {
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT => {
            JSObject::free(rt, gp.cast());
        }
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
            free_function_bytecode(rt, gp.cast());
        }
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
            Box::from_raw(gp.cast::<JSAsyncFunctionState>()).free(rt);
        }
        _ => {
            unreachable!()
        }
    }
}

pub unsafe fn JS_MarkContext(rt: &mut JSRuntime, ctx: &mut JSContext, mark_func: JS_MarkFunc) {
    // Modules are not seen by the GC, so we directly mark the objects
    // referenced by each module
    list_foreach(&mut ctx.loaded_modules, |el| {
        let m = list_entry!(el, JSModuleDef, link);
        js_mark_module_def(rt, m.as_mut().unwrap(), mark_func);
    });

    // let mut el = ctx.loaded_modules.next;
    // while el != &mut ctx.loaded_modules {
    //     debug_assert!(el.is_aligned());

    //     let m = list_entry!(el, JSModuleDef, link);
    //     js_mark_module_def(rt, m.as_mut().unwrap(), mark_func);

    //     el = (*el).next;
    // }

    JS_MarkValue(rt, ctx.global_obj, mark_func);
    JS_MarkValue(rt, ctx.global_var_obj, mark_func);

    JS_MarkValue(rt, ctx.throw_type_error, mark_func);
    JS_MarkValue(rt, ctx.eval_func, mark_func);

    JS_MarkValue(rt, ctx.array_proto_values, mark_func);

    for i in 0..JS_NATIVE_ERROR_COUNT {
        JS_MarkValue(rt, ctx.native_error_proto[i], mark_func);
    }

    for cproto in ctx.class_proto.iter() {
        JS_MarkValue(rt, *cproto, mark_func);
    }
    // for i in 0..rt.class_count {
    //     JS_MarkValue(rt, ctx.class_proto[i as usize], mark_func);
    // }

    JS_MarkValue(rt, ctx.iterator_proto, mark_func);
    JS_MarkValue(rt, ctx.async_iterator_proto, mark_func);
    JS_MarkValue(rt, ctx.promise_ctor, mark_func);
    JS_MarkValue(rt, ctx.array_ctor, mark_func);
    JS_MarkValue(rt, ctx.regexp_ctor, mark_func);
    JS_MarkValue(rt, ctx.function_ctor, mark_func);
    JS_MarkValue(rt, ctx.function_proto, mark_func);

    if !ctx.array_shape.is_null() {
        mark_func(rt, &mut (*ctx.array_shape).header);
    }
}

pub unsafe fn js_mark_module_def(rt: *mut JSRuntime, m: &mut JSModuleDef, mark_func: JS_MarkFunc) {
    for me in m.export_entries.iter() {
        if me.export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL && !me.u.local.var_ref.is_null()
        {
            mark_func(rt, &mut (*me.u.local.var_ref).u0.header);
        }
    }

    JS_MarkValue(rt, m.module_ns, mark_func);
    JS_MarkValue(rt, m.func_obj, mark_func);
    JS_MarkValue(rt, m.eval_exception, mark_func);
    JS_MarkValue(rt, m.meta_obj, mark_func);
    JS_MarkValue(rt, m.promise, mark_func);
    JS_MarkValue(rt, m.resolving_funcs[0], mark_func);
    JS_MarkValue(rt, m.resolving_funcs[1], mark_func);
}

/* indicate that the object may be part of a function prototype cycle */
pub unsafe fn set_cycle_flag(ctx: *mut JSContext, obj: JSValueConst) {
    // to nothing
}

pub unsafe fn JS_DumpGCObject(rt: *mut JSRuntime, p: *mut JSGCObjectHeader) {
    if (*p).gc_obj_type() == JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT {
        (*p.cast::<JSObject>()).dump(rt.as_ref().unwrap());
    } else {
        print!("{:14p} {:4} ", p, (*p).ref_count);
        match (*p).gc_obj_type() {
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
                print!("[function bytecode]");
            }
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_SHAPE => {
                print!("[shape]");
            }
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF => {
                print!("[var_ref]");
            }
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
                print!("[async_function]");
            }
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_CONTEXT => {
                print!("[js_context]");
            }
            _ => {
                print!("[unknown {:?}]", (*p).gc_obj_type());
            }
        }
        println!("");
    }
}

pub unsafe fn JS_DumpGCObjects(rt: *mut JSRuntime) {
    println!("GCObjects: {{");
    JS_DumpObjectHeader();

    list_foreach(&mut (*rt).gc_obj_list, |el| {
        let p = list_entry!(el, JSGCObjectHeader, link);
        JS_DumpGCObject(rt, p);
    });

    println!("}}");
}
