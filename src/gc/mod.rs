mod gc;
mod gcobj;
mod mark;
mod scan;

pub use {gc::*, gcobj::*, mark::*};

//
// LEGACY FUNCTIONS (TO BE REMOVED)
//

/// Adds a new GC object to the garbage collector's tracking list
/// Initializes the object's mark and type, then adds it to the GC object list
#[deprecated]
pub unsafe fn add_gc_object(
    rt: *mut crate::runtime::JSRuntime,
    h: *mut JSGCObjectHeader,
    tp: JSGCObjectTypeEnum,
) {
    // // Initialize the object's mark and type
    // (*h).set_gc_mark(0);
    // (*h).set_gc_obj_type(tp);

    // // Add the object to the end of the GC object list
    // list_add_tail(&mut (*h).link, &mut (*rt).gc_obj_list);
}

/// Function type for marking objects during garbage collection
/// Parameters:
/// - rt: Pointer to the JavaScript runtime
/// - gp: Pointer to the garbage collected object header
#[deprecated]
pub type JS_MarkFunc = unsafe fn(rt: *mut crate::runtime::JSRuntime, gp: *mut JSGCObjectHeader);

/// Marks a JavaScript value during garbage collection
/// Only marks values that have reference counts (objects and function bytecode)
#[deprecated]
pub unsafe fn JS_MarkValue(
    rt: *mut crate::runtime::JSRuntime,
    val: crate::value::JSValue,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");

    // if JS_VALUE_HAS_REF_COUNT(val) {
    //     match JS_VALUE_GET_TAG(val) {
    //         JS_TAG_OBJECT | JS_TAG_FUNCTION_BYTECODE => {
    //             mark_func(rt, JS_VALUE_GET_PTR(val).cast());
    //         }
    //         _ => {}
    //     }
    // }
}

/// Marks the realm associated with an auto-initialized property
/// This ensures the realm is preserved during garbage collection
#[deprecated]
pub unsafe fn js_autoinit_mark(
    rt: *mut crate::runtime::JSRuntime,
    pr: *mut crate::value::object::JSProperty,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
    // // Get the realm from the property and mark its header
    // let realm = js_autoinit_get_realm(pr);
    // mark_func(rt, &mut (*realm).header);
}

/// Recursively marks all child objects of a GC object
/// This ensures all referenced objects are preserved during garbage collection
#[deprecated]
pub(super) unsafe fn mark_children(
    rt: *mut crate::runtime::JSRuntime,
    gp: *mut JSGCObjectHeader,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");

    // match (*gp).gc_obj_type() {
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT => {
    //         let p = gp.cast::<JSObject>();

    //         mark_func(rt, &mut (*p).shape_mut().unwrap().header);

    //         // Mark all the fields
    //         for (idx, prs) in (*p).shape().unwrap().props.values().enumerate() {
    //             let pr = (*p).prop_values.get_mut(idx).unwrap();

    //             if prs.flags() & JS_PROP_TMASK != 0 {
    //                 match prs.flags() & JS_PROP_TMASK {
    //                     JS_PROP_GETSET => {
    //                         if !pr.u.getset.getter.is_null() {
    //                             mark_func(rt, &mut (*pr.u.getset.getter).header);
    //                         }
    //                         if !pr.u.getset.setter.is_null() {
    //                             mark_func(rt, &mut (*pr.u.getset.setter).header);
    //                         }
    //                     }
    //                     JS_PROP_VARREF => {
    //                         // Note: the tag does not matter provided it is a GC object
    //                         // mark_func(rt, &mut (*(*pr).u.var_ref).u0.header);
    //                     }
    //                     JS_PROP_AUTOINIT => {
    //                         js_autoinit_mark(rt, pr, mark_func);
    //                     }
    //                     _ => {}
    //                 }
    //             } else {
    //                 JS_MarkValue(rt, pr.u.value, mark_func);
    //             }
    //         }

    //         if (*p).class_id() != JS_CLASS_OBJECT {
    //             if let Some(f) = (*rt).class_array[(*p).class_id() as usize].gc_mark {
    //                 f(rt, JS_MKPTR!(JSTag::Object, p.cast()), mark_func);
    //             }
    //         }
    //     }
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
    //         // The template objects can be part of a cycle
    //         let b = gp.cast::<JSFunctionBytecode>();
    //         if let Some(cpool) = (*b).cpools_slice() {
    //             cpool.iter().for_each(|v| JS_MarkValue(rt, *v, mark_func));
    //         }
    //         if !(*b).realm.is_null() {
    //             mark_func(rt, &mut (*(*b).realm).header);
    //         }
    //     }
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF => {
    //         let var_ref = Pin::new_unchecked(gp.cast::<JSVarRefInner>().as_ref().unwrap());
    //         if (*var_ref).is_detached() {
    //             JS_MarkValue(rt, *(*var_ref).pvalue, mark_func);
    //         } else if !(*var_ref).value_or_links.links.async_func.is_null() {
    //             mark_func(
    //                 rt,
    //                 &mut (*(*var_ref).value_or_links.links.async_func).header,
    //             );
    //         }
    //     }
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
    //         unimplemented!("mark_children() for JS_GC_OBJ_TYPE_ASYNC_FUNCTION")

    //         // let s = gp as *mut JSAsyncFunctionState;
    //         // let sf = &mut (*s).frame;
    //         // let mut sp: *mut JSValue;

    //         // if (*s).is_completed == 0 {
    //         //     JS_MarkValue(rt, (*sf).cur_func, mark_func);
    //         //     JS_MarkValue(rt, (*s).this_val, mark_func);
    //         //     // sf->cur_sp = NULL if the function is running
    //         //     if !(*sf).cur_sp.is_null() {
    //         //         // If the function is running, cur_sp is not known so we
    //         //         // cannot mark the stack. Marking the variables is not needed
    //         //         // because a running function cannot be part of a removable cycle
    //         //         sp = (*sf).arg_buf;
    //         //         while sp < (*sf).cur_sp {
    //         //             JS_MarkValue(rt, *sp, mark_func);
    //         //             sp = sp.offset(1);
    //         //         }
    //         //     }
    //         // }
    //         // JS_MarkValue(rt, (*s).resolving_funcs[0], mark_func);
    //         // JS_MarkValue(rt, (*s).resolving_funcs[1], mark_func);
    //     }
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_SHAPE => {
    //         let sh = gp.cast::<JSShape>();
    //         if !(*sh).proto.is_null() {
    //             mark_func(rt, &mut (*(*sh).proto).header);
    //         }
    //     }
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_CONTEXT => {
    //         let ctx = gp as *mut JSContext;
    //         JS_MarkContext(rt.as_mut().unwrap(), ctx.as_mut().unwrap(), mark_func);
    //     }
    // }
}

#[deprecated]
pub unsafe fn JS_MarkContext(
    rt: &mut crate::runtime::JSRuntime,
    ctx: &mut crate::context::JSContext,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");

    // // Modules are not seen by the GC, so we directly mark the objects
    // // referenced by each module
    // list_foreach(&mut ctx.loaded_modules, |el| {
    //     let m = list_entry!(el, JSModuleDef, link);
    //     js_mark_module_def(rt, m.as_mut().unwrap(), mark_func);
    // });

    // JS_MarkValue(rt, ctx.global_obj, mark_func);
    // JS_MarkValue(rt, ctx.global_var_obj, mark_func);

    // JS_MarkValue(rt, ctx.throw_type_error, mark_func);
    // JS_MarkValue(rt, ctx.eval_func, mark_func);

    // JS_MarkValue(rt, ctx.array_proto_values, mark_func);

    // for i in 0..JS_NATIVE_ERROR_COUNT {
    //     JS_MarkValue(rt, ctx.native_error_proto[i], mark_func);
    // }

    // for cproto in ctx.class_proto.iter() {
    //     JS_MarkValue(rt, *cproto, mark_func);
    // }

    // JS_MarkValue(rt, ctx.iterator_proto, mark_func);
    // JS_MarkValue(rt, ctx.async_iterator_proto, mark_func);
    // JS_MarkValue(rt, ctx.promise_ctor, mark_func);
    // JS_MarkValue(rt, ctx.array_ctor, mark_func);
    // JS_MarkValue(rt, ctx.regexp_ctor, mark_func);
    // JS_MarkValue(rt, ctx.function_ctor, mark_func);
    // JS_MarkValue(rt, ctx.function_proto, mark_func);

    // if !ctx.array_shape.is_null() {
    //     mark_func(rt, &mut (*ctx.array_shape).header);
    // }
}

#[deprecated]
pub unsafe fn js_mark_module_def(
    rt: *mut crate::runtime::JSRuntime,
    m: &mut crate::module::JSModuleDef,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");

    // for me in m.export_entries.iter_mut() {
    //     if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
    //         var_ref: Some(vr), ..
    //     } = &mut me.export_type
    //     {
    //         // mark for var_ref
    //         mark_func(rt, &mut vr.as_mut().get_unchecked_mut().u0.header);
    //     }
    // }

    // JS_MarkValue(rt, m.module_ns, mark_func);
    // JS_MarkValue(rt, m.func_obj, mark_func);
    // JS_MarkValue(rt, m.eval_exception, mark_func);
    // JS_MarkValue(rt, m.meta_obj, mark_func);
    // JS_MarkValue(rt, m.promise, mark_func);
    // JS_MarkValue(rt, m.resolving_funcs[0], mark_func);
    // JS_MarkValue(rt, m.resolving_funcs[1], mark_func);
}

// unsafe fn gc_scan_incref_child(rt: *mut JSRuntime, p: *mut JSGCObjectHeader) {
//     (*p).ref_count += 1;
//     if (*p).ref_count == 1 {
//         /* ref_count was 0: remove from tmp_obj_list and add at the
//         end of gc_obj_list */
//         list_del(&mut (*p).link);
//         list_add_tail(&mut (*p).link, &mut (*rt).gc_obj_list);
//         p.as_mut().unwrap().set_gc_mark(0); /* reset the mark for the next GC call */
//     }
// }
// unsafe fn gc_scan_incref_child2(rt: *mut JSRuntime, p: *mut JSGCObjectHeader) {
//     (*p).ref_count += 1;
// }

/// Preserves objects with positive reference counts and their child objects.
/// Identifies objects with zero reference count in the GC object list
/// and moves them to a temporary list for removal.
#[deprecated]
pub unsafe fn gc_scan(rt: &mut crate::runtime::JSRuntime) {
    unreachable!("unused");

    // print!(
    //     "[O.o][gc_scan] gc_obj_list={}, tmp_obj_list={}",
    //     list_count(&mut rt.gc_obj_list),
    //     list_count(&mut rt.tmp_obj_list),
    // );

    // list_foreach(&mut rt.gc_obj_list, |el| {
    //     let p = list_entry!(el, JSGCObjectHeader, link);
    //     debug_assert!((*p).ref_count > 0);

    //     // reset the mark for the next GC call
    //     (*p).set_gc_mark(0);

    //     mark_children(rt, p, |rt: *mut JSRuntime, p: *mut JSGCObjectHeader| {
    //         (*p).ref_count += 1;

    //         if (*p).ref_count == 1 {
    //             // ref_count was 0, `p` is now in `tmp_obj_list`,
    //             // restore it from `tmp_obj_list` to `gc_obj_list`
    //             debug_assert!(
    //                 list_find(&mut (*p).link, &mut (*rt).tmp_obj_list),
    //                 "zeroed refcount object must in tmp_obj_list"
    //             );

    //             list_del(&mut (*p).link);
    //             list_add_tail(&mut (*p).link, &mut (*rt).gc_obj_list);

    //             // reset the mark for the next GC call
    //             (*p).set_gc_mark(0);
    //         }
    //     });
    // });

    // // restore the refcount of the objects to be deleted.
    // list_foreach(&mut rt.tmp_obj_list, |el| {
    //     let p = list_entry!(el, JSGCObjectHeader, link);
    //     mark_children(rt, p, |_, p: *mut JSGCObjectHeader| {
    //         // debug_assert_eq!((*p).ref_count, 0); not nessesarily 0?
    //         (*p).ref_count += 1;
    //     });
    // });

    // println!(
    //     " -> gc_obj_list={}, tmp_obj_list={} [/O.o]",
    //     list_count(&mut (*rt).gc_obj_list),
    //     list_count(&mut (*rt).tmp_obj_list),
    // );
}

/// Frees objects in the temporary list, handling cyclic references
#[deprecated]
pub unsafe fn gc_free_cycles(rt: &mut crate::runtime::JSRuntime) {
    unreachable!("unused");

    // #[cfg(feature = "dump_gc_free")]
    // let mut header_done = false;

    // rt.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES;

    // loop {
    //     let el = rt.tmp_obj_list.next;
    //     if el == &mut rt.tmp_obj_list {
    //         break;
    //     }

    //     // Note: object in tmp_obj_list not always have ref_count==0.
    //     let p = list_entry!(el, JSGCObjectHeader, link);

    //     // Only need to free the GC object associated with JS values or async functions.
    //     // The rest will be automatically removed because they must be referenced by them
    //     match (*p).gc_obj_type() {
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT
    //         | JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE
    //         | JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
    //             #[cfg(feature = "dump_gc_free")]
    //             {
    //                 if !header_done {
    //                     println!("Freeing cycles:");
    //                     JS_DumpObjectHeader();
    //                     header_done = true;
    //                 }
    //                 JS_DumpGCObject(rt, p);
    //             }

    //             free_gc_object(rt, p);

    //             // `el` is removed from tmp_obj_list.
    //             // `el` could be added to zero ref count list or dropped directly.
    //         }
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF => {
    //             list_del(&mut (*p).link);
    //             list_add_tail(&mut (*p).link, &mut rt.gc_zero_ref_count_list);
    //         }
    //         _ => {
    //             list_del(&mut (*p).link);
    //             list_add_tail(&mut (*p).link, &mut rt.gc_zero_ref_count_list);
    //         }
    //     }
    // }

    // rt.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;

    // // drop gc obj payload data
    // list_foreach_safe(&mut rt.gc_zero_ref_count_list, |el| {
    //     let p = list_entry!(el, JSGCObjectHeader, link);
    //     match (*p).gc_obj_type() {
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT => {
    //             drop(Box::from_raw(p.cast::<JSObject>()));
    //         }
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
    //             drop(Box::from_raw(p.cast::<JSFunctionBytecode>()));
    //         }
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
    //             drop(Box::from_raw(p.cast::<JSAsyncFunctionState>()));
    //         }
    //         t => {
    //             unreachable!("{t:?}")
    //         }
    //     }
    // });

    // // clear gc_zero_ref_count_list
    // init_list_head(&mut (*rt).gc_zero_ref_count_list);
}

/// Frees a GC object based on its type
/// Handles different types of GC objects with their specific cleanup logic
#[deprecated(note = "use gc_free() instead")]
pub unsafe fn free_gc_object(rt: *mut crate::runtime::JSRuntime, gp: *mut JSGCObjectHeader) {
    unreachable!("unused");

    // match (*gp).gc_obj_type() {
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT => {
    //         JSObject::dispose(rt.as_mut().unwrap(), gp.cast());
    //     }
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
    //         free_function_bytecode(rt, gp.cast());
    //     }
    //     JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
    //         Box::from_raw(gp.cast::<JSAsyncFunctionState>()).dispose(rt);
    //     }
    //     _ => {
    //         unreachable!()
    //     }
    // }
}

/// Removes an object from either the main GC object list or temporary list
#[deprecated(note = "use set status to Zeroed instead")]
pub unsafe fn remove_gc_object(h: *mut JSGCObjectHeader) {
    unreachable!("unused");
    // crate::types::list_del(&mut (*h).link);
}

/// Frees objects with zero reference count (in gc_zero_ref_count_list chain).
#[deprecated(note = "use JSRuntime::gc_free_zeroed() instead")]
pub unsafe fn free_zero_refcount(rt: *mut crate::runtime::JSRuntime) {
    (*rt).gc_free_zeroed();

    // // Set GC phase to decrement reference count
    // (*rt).gc_phase = JSGCPhaseEnum::JS_GC_PHASE_DECREF;

    // // Process each object in the zero refcount list
    // loop {
    //     let el = (*rt).gc_zero_ref_count_list.next;
    //     // Stop when we've processed all objects
    //     if el == &mut (*rt).gc_zero_ref_count_list {
    //         break;
    //     }

    //     // Get the object header from the list entry
    //     let p = list_entry!(el, JSGCObjectHeader, link);
    //     debug_assert_eq!((*p).ref_count, 0);

    //     // Free the GC object
    //     free_gc_object(rt, p);
    // }
    // // Reset GC phase
    // (*rt).gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;
}

/// Decrements the reference count of a child GC object
/// Moves the object to the temporary list if its refcount reaches zero
#[deprecated]
pub unsafe fn gc_decref_child(rt: *mut crate::runtime::JSRuntime, p: *mut JSGCObjectHeader) {
    unreachable!("unsed");

    // // Safety checks
    // debug_assert!(!p.is_null());
    // debug_assert!(p.is_aligned());
    // debug_assert!((*p).ref_count > 0);

    // // Decrement the reference count
    // (*p).ref_count -= 1;

    // // If refcount reaches zero and object is marked, move to temporary list
    // if (*p).ref_count == 0 && (*p).gc_mark() == 1 {
    //     list_del(&mut (*p).link);
    //     list_add_tail(&mut (*p).link, &mut (*rt).tmp_obj_list);
    // }
}

/// Decreases reference counts for all child objects in the GC object list.
/// Moves objects with zero reference count to a temporary list for cleanup.
#[deprecated(note = "use JSRuntime::gc_deref() instead")]
pub unsafe fn gc_decref(rt: &mut crate::runtime::JSRuntime) {
    unreachable!("unsed");

    // init_list_head(&mut (*rt).tmp_obj_list);

    // print!(
    //     "[O.o][gc_decref] gc_obj_list={}, tmp_obj_list={}",
    //     list_count(&mut (*rt).gc_obj_list),
    //     list_count(&mut (*rt).tmp_obj_list)
    // );

    // list_foreach_safe(&mut rt.gc_obj_list, |el| {
    //     let p = list_entry!(el, JSGCObjectHeader, link);
    //     debug_assert_eq!((*p).gc_mark(), 0);

    //     mark_children(rt, p, gc_decref_child);
    //     (*p).set_gc_mark(1);

    //     if (*p).ref_count == 0 {
    //         list_del(&mut (*p).link);
    //         list_add_tail(&mut (*p).link, &mut rt.tmp_obj_list);
    //     }
    // });

    // println!(
    //     " -> gc_obj_list={}, tmp_obj_list={} [/O.o]",
    //     list_count(&mut (*rt).gc_obj_list),
    //     list_count(&mut (*rt).tmp_obj_list)
    // );
}

#[deprecated(note = "use GCObject::gc_dump() instead")]
pub unsafe fn JS_DumpGCObject(rt: *mut crate::runtime::JSRuntime, p: *mut JSGCObjectHeader) {
    unreachable!("unused");

    // if (*p).gc_obj_type() == JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT {
    //     (*p.cast::<JSObject>()).dump(rt.as_ref().unwrap());
    // } else {
    //     print!("{:14p} {:4} ", p, (*p).ref_count);
    //     match (*p).gc_obj_type() {
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE => {
    //             print!("[function bytecode]");
    //         }
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_SHAPE => {
    //             print!("[shape]");
    //         }
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF => {
    //             print!("[var_ref]");
    //         }
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION => {
    //             print!("[async_function]");
    //         }
    //         JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_CONTEXT => {
    //             print!("[js_context]");
    //         }
    //         _ => {
    //             print!("[unknown {:?}]", (*p).gc_obj_type());
    //         }
    //     }
    //     println!("");
    // }
}

#[deprecated(note = "use JSRuntime::gc_dump_objects() instead")]
pub unsafe fn JS_DumpGCObjects(rt: *mut crate::runtime::JSRuntime) {
    println!("GCObjects: {{");
    JS_DumpObjectHeader();

    for node in (*rt).gc_obj_list.iter() {
        node.gc_dump(rt.as_ref().unwrap());
    }

    println!("}}");
}
