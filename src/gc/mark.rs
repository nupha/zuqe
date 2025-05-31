use crate::{
    context::JSContext,
    error::JS_NATIVE_ERROR_COUNT,
    function::{JSFunctionBytecode, JSVarRef},
    list_entry,
    module::{JSExportTypeEnum, JSModuleDef},
    runtime::JSRuntime,
    utils::{list_foreach, Node},
    value::{
        object::{
            js_autoinit_get_realm, JSProperty, JS_PROP_AUTOINIT, JS_PROP_GETSET, JS_PROP_TMASK,
            JS_PROP_VARREF,
        },
        JSTag, JSValue, JS_TAG_FUNCTION_BYTECODE, JS_TAG_OBJECT, JS_VALUE_GET_PTR,
        JS_VALUE_GET_TAG, JS_VALUE_HAS_REF_COUNT,
    },
    JS_CLASS_OBJECT, JS_MKPTR,
};

use super::{GCObject, GCObjectNodeData, GCObjectType};

/// Function type for marking objects during garbage collection
/// Parameters:
/// - rt: Pointer to the JavaScript runtime
/// - gp: Pointer to the garbage collected object header
pub type GCMarkFunc = unsafe fn(rt: *mut JSRuntime, ob: *mut dyn GCObject);

impl JSRuntime {
    /// Marks a JavaScript value during garbage collection
    /// Only marks values that have reference counts (objects and function bytecode)
    pub(crate) unsafe fn gc_mark_value(&mut self, val: JSValue, mark_func: GCMarkFunc) {
        if JS_VALUE_HAS_REF_COUNT(val) {
            match JS_VALUE_GET_TAG(val) {
                JS_TAG_OBJECT => {
                    mark_func(self, val.object_unchecked());
                }
                JS_TAG_FUNCTION_BYTECODE => {
                    mark_func(self, JS_VALUE_GET_PTR(val).cast::<JSFunctionBytecode>());
                }
                _ => {}
            }
        }
    }

    pub unsafe fn gc_mark_module_def(&mut self, m: &mut JSModuleDef, mark_func: GCMarkFunc) {
        for me in m.export_entries.iter_mut() {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                var_ref: Some(vr), ..
            } = me.export_type
            {
                // mark for var_ref
                mark_func(self, vr.cast::<JSVarRef>().as_ptr());
            }
        }

        self.gc_mark_value(m.module_ns, mark_func);
        self.gc_mark_value(m.func_obj, mark_func);
        self.gc_mark_value(m.eval_exception, mark_func);
        self.gc_mark_value(m.meta_obj, mark_func);
        self.gc_mark_value(m.promise, mark_func);
        self.gc_mark_value(m.resolving_funcs[0], mark_func);
        self.gc_mark_value(m.resolving_funcs[1], mark_func);
    }

    /// Marks the realm associated with an auto-initialized property
    /// This ensures the realm is preserved during garbage collection
    pub unsafe fn gc_mark_autoinit(&mut self, pr: *mut JSProperty, mark_func: GCMarkFunc) {
        // Get the realm from the property and mark its header
        let realm = js_autoinit_get_realm(pr);
        mark_func(self, realm);
    }

    pub unsafe fn gc_mark_context(&mut self, ctx: &mut JSContext, mark_func: GCMarkFunc) {
        // Modules are not seen by the GC, so we directly mark the objects
        // referenced by each module
        list_foreach(&mut ctx.loaded_modules, |el| {
            let m = list_entry!(el, JSModuleDef, link);
            self.gc_mark_module_def(m.as_mut().unwrap(), mark_func);
        });

        self.gc_mark_value(ctx.global_obj, mark_func);
        self.gc_mark_value(ctx.global_var_obj, mark_func);

        self.gc_mark_value(ctx.throw_type_error, mark_func);
        self.gc_mark_value(ctx.eval_func, mark_func);

        self.gc_mark_value(ctx.array_proto_values, mark_func);

        for i in 0..JS_NATIVE_ERROR_COUNT {
            self.gc_mark_value(ctx.native_error_proto[i], mark_func);
        }

        for cproto in ctx.class_proto.iter() {
            self.gc_mark_value(*cproto, mark_func);
        }

        self.gc_mark_value(ctx.iterator_proto, mark_func);
        self.gc_mark_value(ctx.async_iterator_proto, mark_func);
        self.gc_mark_value(ctx.promise_ctor, mark_func);
        self.gc_mark_value(ctx.array_ctor, mark_func);
        self.gc_mark_value(ctx.regexp_ctor, mark_func);
        self.gc_mark_value(ctx.function_ctor, mark_func);
        self.gc_mark_value(ctx.function_proto, mark_func);

        if !ctx.array_shape.is_null() {
            mark_func(self, ctx.array_shape);
        }
    }
}

// /// Recursively marks all child objects of a GC object
// /// This ensures all referenced objects are preserved during garbage collection
// pub(super) unsafe fn gc_mark_children(
//     rt: *mut JSRuntime,
//     ob: *mut dyn GCObject,
//     mark_func: GCMarkFunc,
// ) {
//     debug_assert!(matches!(
//         (*ob).gc_status(),
//         GCStatus::Tracking | GCStatus::Temp
//     ));

//     match (*ob).gc_type() {
//         GCObjectType::Object(mut p) => {
//             mark_func(rt, p.as_ref().shape);

//             // Mark all the fields
//             for (idx, prs) in p.as_ref().shape().unwrap().props.values().enumerate() {
//                 let pr = p.as_mut().prop_values.get_mut(idx).unwrap();

//                 if prs.flags() & JS_PROP_TMASK != 0 {
//                     match prs.flags() & JS_PROP_TMASK {
//                         JS_PROP_GETSET => {
//                             if !pr.u.getset.getter.is_null() {
//                                 mark_func(rt, pr.u.getset.getter);
//                             }
//                             if !pr.u.getset.setter.is_null() {
//                                 mark_func(rt, pr.u.getset.setter);
//                             }
//                         }
//                         JS_PROP_VARREF => {
//                             // Note: the tag does not matter provided it is a GC object
//                             // mark_func(rt, &mut (*(*pr).u.var_ref).u0.header);
//                         }
//                         JS_PROP_AUTOINIT => {
//                             (*rt).gc_mark_autoinit(pr, mark_func);
//                         }
//                         _ => {}
//                     }
//                 } else {
//                     (*rt).gc_mark_value(pr.u.value, mark_func);
//                 }
//             }

//             if p.as_ref().class_id() != JS_CLASS_OBJECT {
//                 if let Some(f) = (*rt).class_array[p.as_ref().class_id() as usize].gc_mark {
//                     f(rt, JS_MKPTR!(JSTag::Object, p.as_ptr().cast()), mark_func);
//                 }
//             }
//         }
//         GCObjectType::FunctionByteCode(b) => {
//             // The template objects can be part of a cycle
//             if let Some(cpool) = b.as_ref().cpools_slice() {
//                 cpool
//                     .iter()
//                     .for_each(|v| (*rt).gc_mark_value(*v, mark_func));
//             }
//             if !b.as_ref().realm.is_null() {
//                 mark_func(rt, b.as_ref().realm);
//             }
//         }
//         GCObjectType::VarRef(p) => {
//             let var_ref: Pin<&JSVarRefInner> = Pin::new_unchecked(p.as_ref());
//             if var_ref.is_detached() {
//                 (*rt).gc_mark_value(*(*var_ref).pvalue, mark_func);
//             } else if !(*var_ref).value_or_links.links.async_func.is_null() {
//                 mark_func(rt, (*var_ref).value_or_links.links.async_func);
//             }
//         }
//         GCObjectType::AsyncFunction(s) => {
//             unimplemented!("mark_children() for JS_GC_OBJ_TYPE_ASYNC_FUNCTION")

//             // let s = gp as *mut JSAsyncFunctionState;
//             // let sf = &mut (*s).frame;
//             // let mut sp: *mut JSValue;

//             // if (*s).is_completed == 0 {
//             //     JS_MarkValue(rt, (*sf).cur_func, mark_func);
//             //     JS_MarkValue(rt, (*s).this_val, mark_func);
//             //     // sf->cur_sp = NULL if the function is running
//             //     if !(*sf).cur_sp.is_null() {
//             //         // If the function is running, cur_sp is not known so we
//             //         // cannot mark the stack. Marking the variables is not needed
//             //         // because a running function cannot be part of a removable cycle
//             //         sp = (*sf).arg_buf;
//             //         while sp < (*sf).cur_sp {
//             //             JS_MarkValue(rt, *sp, mark_func);
//             //             sp = sp.offset(1);
//             //         }
//             //     }
//             // }
//             // JS_MarkValue(rt, (*s).resolving_funcs[0], mark_func);
//             // JS_MarkValue(rt, (*s).resolving_funcs[1], mark_func);
//         }
//         GCObjectType::Shape(sh) => {
//             if !sh.as_ref().proto.is_null() {
//                 mark_func(rt, sh.as_ref().proto);
//             }
//         }
//         GCObjectType::Context(mut ctx) => {
//             (*rt).gc_mark_context(ctx.as_mut(), mark_func);
//         }
//     }
// }

/// Recursively marks all child objects of a GC object
/// This ensures all referenced objects are preserved during garbage collection
pub(super) unsafe fn gc_mark_children(
    rt: *mut JSRuntime,
    node: *mut Node<GCObjectNodeData>,
    mark_func: GCMarkFunc,
) {
    match (*node).gc_type() {
        GCObjectType::Object(mut p) => {
            mark_func(rt, p.as_ref().shape);

            // Mark all the fields
            for (idx, prs) in p.as_ref().shape().unwrap().props.values().enumerate() {
                let pr = p.as_mut().prop_values.get_mut(idx).unwrap();

                if prs.flags() & JS_PROP_TMASK != 0 {
                    match prs.flags() & JS_PROP_TMASK {
                        JS_PROP_GETSET => {
                            if !pr.u.getset.getter.is_null() {
                                mark_func(rt, pr.u.getset.getter);
                            }
                            if !pr.u.getset.setter.is_null() {
                                mark_func(rt, pr.u.getset.setter);
                            }
                        }
                        JS_PROP_VARREF => {
                            // Note: the tag does not matter provided it is a GC object
                            // mark_func(rt, &mut (*(*pr).u.var_ref).u0.header);
                        }
                        JS_PROP_AUTOINIT => {
                            (*rt).gc_mark_autoinit(pr, mark_func);
                        }
                        _ => {}
                    }
                } else {
                    (*rt).gc_mark_value(pr.u.value, mark_func);
                }
            }

            if p.as_ref().class_id() != JS_CLASS_OBJECT {
                if let Some(f) = (*rt).class_array[p.as_ref().class_id() as usize].gc_mark {
                    f(rt, JS_MKPTR!(JSTag::Object, p.as_ptr().cast()), mark_func);
                }
            }
        }
        GCObjectType::FunctionBytecode(b) => {
            // The template objects can be part of a cycle
            if let Some(cpool) = b.as_ref().cpools_slice() {
                cpool
                    .iter()
                    .for_each(|v| (*rt).gc_mark_value(*v, mark_func));
            }
            if !b.as_ref().realm.is_null() {
                mark_func(rt, b.as_ref().realm);
            }
        }
        GCObjectType::VarRef(p) => {
            let vr = p.cast::<JSVarRef>().as_ref();
            if vr.is_detached() {
                (*rt).gc_mark_value(*vr.pvalue, mark_func);
            } else if !vr.async_func.is_null() {
                mark_func(rt, vr.async_func);
            }
        }
        GCObjectType::AsyncFunction(mut s) => {
            let sf = &mut s.as_mut().frame;

            if !s.as_ref().is_completed {
                (*rt).gc_mark_value(sf.cur_func, mark_func);
                (*rt).gc_mark_value(s.as_ref().this_val, mark_func);

                // cur_sp_offset = 0 if the function is running.
                // If the function is running, cur_sp_offset is not known so we
                // cannot mark the stack. Marking the variables is not needed
                // because a running function cannot be part of a removable cycle

                if sf.cur_sp_offset != 0 {
                    // for async function, arg_base_ptr point to a position in local_buf,
                    // cur_sp_offset is also about local_buf,
                    // they are match.
                    // mark values in arg_base through cur_sp range.
                    let mut ptr = sf.arg_base_ptr;
                    let end_ptr = sf.stack_cur_ptr().as_ptr();
                    while ptr < end_ptr {
                        (*rt).gc_mark_value(*ptr, mark_func);
                        ptr = ptr.add(1);
                    }
                }
            }
            (*rt).gc_mark_value(s.as_ref().resolving_funcs[0], mark_func);
            (*rt).gc_mark_value(s.as_ref().resolving_funcs[1], mark_func);
        }
        GCObjectType::Shape(sh) => {
            if !sh.as_ref().proto.is_null() {
                mark_func(rt, sh.as_ref().proto);
            }
        }
        GCObjectType::Context(mut ctx) => {
            (*rt).gc_mark_context(ctx.as_mut(), mark_func);
        }
    }
}
