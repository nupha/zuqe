use std::ptr::NonNull;

use crate::{
    container_of,
    context::{JSContext, JS_MODE_ASYNC},
    gc::{add_gc_object, remove_gc_object, JSGCObjectHeader, JSGCObjectTypeEnum},
    list_entry,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    types::{
        init_list_head, list_add_tail, list_del, list_foreach_safe, list_foreach_with_ret,
        list_head,
    },
    value::{JSValue, JS_DupValue, JS_FreeValueRT},
};

use super::{async_func_free, JSAsyncFunctionState};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct JSVarRef {
    pub u0: JSVarRefHeaderOrGCFields,

    // 一个指向 JSValue 的指针。这个字段有两个主要用途：
    // 1. 变量值的定位 ：
    //    - 当变量在栈上时（未分离状态）， pvalue 指向栈上的变量值位置（ var_buf 或 arg_buf ）
    //    - 当变量被分离时（detached = true）， pvalue 指向 JSVarRef 结构体内部的 value_or_links.value 字段
    // 2. 闭包变量访问 ：
    //    - 它允许闭包函数访问外部作用域的变量
    //    - 通过这个指针可以实现对变量的读写操作，无论变量实际存储在栈上还是已经被分离到堆上
    pub pvalue: *mut JSValue,

    // 联合体，根据变量是否被分离(detached)有不同用途：
    // - 当变量被分离时：存储实际的 JSValue 值
    // - 当变量未被分离时：存储链表节点信息和异步函数状态指针
    pub value_or_links: JSVarRefValueOrLinks,
}

// #[cfg(debug_assertions)]
// impl Drop for JSVarRef {
//     fn drop(&mut self) {
//         // println!("[O.o] drop varref={:p}", self);
//         debug_assert_eq!(unsafe { self.u0.header.ref_count }, 0);
//     }
// }

#[repr(C)]
#[derive(Clone, Copy)]
pub union JSVarRefHeaderOrGCFields {
    pub header: JSGCObjectHeader, // must come first
    pub gc_fields: JSVarRefGCFields,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JSVarRefGCFields {
    // reserved for header.ref_count and header.mark/gc_obj_type
    __gc_ref_count: i32,
    __gc_mark: u8,

    /// bitfield for is_detached and is_arg
    pub bitfield: u8,
    /// index of the function variable on the stack
    pub var_idx: u16,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub union JSVarRefValueOrLinks {
    pub value: JSValue,       // used when is_detached = TRUE
    pub links: JSVarRefLinks, // used when is_detached = FALSE
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JSVarRefLinks {
    /// JSStackFrame.var_ref_list list
    pub var_ref_list: list_head,

    /// != NULL if async stack frame
    pub async_func: *mut JSAsyncFunctionState,
}

impl JSVarRef {
    pub fn is_detached(&self) -> bool {
        unsafe { self.u0.gc_fields.bitfield & 0b1000_0000 != 0 }
    }
    pub fn set_detached(&mut self, v: bool) {
        unsafe {
            self.u0.gc_fields.bitfield =
                (self.u0.gc_fields.bitfield & !0b1000_0000) | ((v as u8) << 7)
        }
    }

    pub fn is_arg(&self) -> bool {
        unsafe { self.u0.gc_fields.bitfield & 0b0100_0000 != 0 }
    }

    pub fn set_arg(&mut self, v: bool) {
        unsafe {
            self.u0.gc_fields.bitfield =
                (self.u0.gc_fields.bitfield & !0b0100_0000) | ((v as u8) << 6)
        }
    }

    pub fn var_idx(&self) -> u16 {
        unsafe { self.u0.gc_fields.var_idx }
    }
    pub fn set_var_idx(&mut self, v: u16) {
        self.u0.gc_fields.var_idx = v
    }

    /// free the var ref. if ref count reaches zero, will drop the var ref.
    /// otherwise returns Some()
    pub unsafe fn free(mut self: Box<Self>, rt: *mut JSRuntime) -> Option<Box<Self>> {
        debug_assert!(self.u0.header.ref_count > 0);

        self.u0.header.ref_count -= 1;
        if self.u0.header.ref_count == 0 {
            if self.is_detached() {
                JS_FreeValueRT(rt, self.value_or_links.value);
            } else {
                list_del(&mut self.value_or_links.links.var_ref_list); // still on the stack

                if !self.value_or_links.links.async_func.is_null() {
                    async_func_free(rt, self.value_or_links.links.async_func);
                }
            }

            remove_gc_object(&mut self.u0.header);

            None // dropped
        } else {
            Some(self) // still alive
        }
    }
}

#[deprecated(note = "use JSVarRef::dispose() instead")]
pub unsafe fn free_var_ref(rt: *mut JSRuntime, var_ref: *mut JSVarRef) {
    if !var_ref.is_null() {
        if let Some(vr) = Box::from_raw(var_ref).free(rt) {
            // still alive
            Box::leak(vr);
        }

        // debug_assert!((*var_ref).u0.header.ref_count > 0);

        // (*var_ref).u0.header.ref_count -= 1;

        // if (*var_ref).u0.header.ref_count == 0 {
        //     if (*var_ref).is_detached() {
        //         JS_FreeValueRT(rt, (*var_ref).value_or_links.value);
        //     } else {
        //         list_del(&mut (*var_ref).value_or_links.links.var_ref_list); // still on the stack
        //         if !(*var_ref).value_or_links.links.async_func.is_null() {
        //             async_func_free(rt, (*var_ref).value_or_links.links.async_func);
        //         }
        //     }

        //     remove_gc_object(&mut (*var_ref).u0.header);
        //     drop(Box::from_raw(var_ref));
        // }
    }
}

#[deprecated(note = "use JSStackFrame::close_lexical_var() instead")]
pub unsafe fn close_lexical_var(
    ctx: *mut JSContext,
    sf: *mut JSStackFrame,
    var_idx: u16,
    is_arg: bool,
) {
    (*sf).close_lexical_var(ctx.as_mut().unwrap(), var_idx, is_arg);
}

#[deprecated(note = "use JSStackFrame::close_var_refs() instead")]
pub unsafe fn close_var_refs(ctx: *mut JSContext, sf: *mut JSStackFrame) {
    (*sf).close_var_refs(ctx.as_mut().unwrap());
}

impl JSStackFrame {
    /// create a var ref
    unsafe fn add_var_ref(
        &mut self,
        ctx: &mut JSContext,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<NonNull<JSVarRef>, i32> {
        let mut var_ref = if let Some(mut p) = js_malloc_typed::<JSVarRef>(ctx, 1, true) {
            let o = JSVarRef {
                u0: JSVarRefHeaderOrGCFields {
                    header: JSGCObjectHeader::new(1),
                },
                pvalue: std::ptr::null_mut(),
                value_or_links: JSVarRefValueOrLinks {
                    value: JSValue::UNDEFINED,
                },
            };
            p.write(o);

            init_list_head(&mut p.as_mut().u0.header.link);
            Box::from_raw(p.as_ptr())
        } else {
            return Err(-1);
        };

        var_ref.set_detached(false);
        var_ref.set_var_idx(var_idx);
        var_ref.set_arg(is_arg);

        list_add_tail(
            &mut var_ref.value_or_links.links.var_ref_list,
            &mut self.var_ref_list,
        );

        add_gc_object(
            ctx.rt_mut(),
            &mut var_ref.u0.header,
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF,
        );

        Ok(NonNull::new_unchecked(Box::into_raw(var_ref)))
    }

    pub unsafe fn get_var_ref(
        &mut self,
        ctx: &mut JSContext,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<NonNull<JSVarRef>, i32> {
        let r = list_foreach_with_ret(&mut self.var_ref_list, |el| {
            let vr = list_entry!(el, JSVarRef, value_or_links);
            if (*vr).var_idx() == var_idx && (*vr).is_arg() == is_arg {
                (*vr).u0.header.ref_count += 1;
                return Some(NonNull::new(vr).unwrap());
            }
            None
        });
        if let Some(vr) = r {
            debug_assert!(vr.as_ref().u0.header.ref_count >= 2);
            return Ok(vr);
        }

        // create new var_ref
        let mut vr = self.add_var_ref(ctx, var_idx, is_arg)?;

        vr.as_mut().value_or_links.links.async_func = if self.js_mode & JS_MODE_ASYNC != 0 {
            // The stack frame is detached and may be destroyed at any
            // time so its reference count must be increased. Calling
            // close_var_refs() when destroying the stack frame is not
            // possible because it would change the graph between the GC
            // objects. Another solution could be to temporarily detach
            // the JSVarRef of async functions during the GC. It would
            // have the advantage of allowing the release of unused stack
            // frames in a cycle.

            // `self` is from the JSAsyncFunctionState::frame field.
            // use container_of!() to get the JSAsyncFunctionState that contains `self` stack frame.
            let afs: *mut JSAsyncFunctionState =
                container_of!(self as *mut JSStackFrame, JSAsyncFunctionState, frame);
            (*afs).header.ref_count += 1;

            afs
        } else {
            std::ptr::null_mut()
        };

        vr.as_mut().pvalue = if is_arg {
            self.arg_base_ptr.offset(var_idx as isize)
        } else {
            self.var_base_ptr.offset(var_idx as isize)
        };

        Ok(vr)
    }

    pub unsafe fn close_var_refs(&mut self, ctx: &mut JSContext) {
        list_foreach_safe(&mut self.var_ref_list, |el| {
            let var_ref = list_entry!(el, JSVarRef, value_or_links);

            /* no need to unlink var_ref->var_ref_link as the list is never used afterwards */
            if !(*var_ref).value_or_links.links.async_func.is_null() {
                async_func_free(ctx.rt_mut(), (*var_ref).value_or_links.links.async_func);
            }

            let var_idx = (*var_ref).var_idx();
            (*var_ref).value_or_links.value = JS_DupValue(
                ctx,
                if (*var_ref).is_arg() {
                    *self.arg_base_ptr.offset(var_idx as isize)
                } else {
                    *self.var_base_ptr.offset(var_idx as isize)
                },
            );

            (*var_ref).pvalue = &mut (*var_ref).value_or_links.value;

            // the reference is no longer to a local variable
            (*var_ref).set_detached(true);
        });
    }

    pub unsafe fn close_lexical_var(&mut self, ctx: &mut JSContext, var_idx: u16, is_arg: bool) {
        list_foreach_safe(&mut self.var_ref_list, |el| {
            let var_ref = list_entry!(el, JSVarRef, value_or_links);

            if (*var_ref).var_idx() == var_idx && (*var_ref).is_arg() == is_arg {
                list_del(&mut (*var_ref).value_or_links.links.var_ref_list);

                if !(*var_ref).value_or_links.links.async_func.is_null() {
                    async_func_free((*ctx).rt_mut(), (*var_ref).value_or_links.links.async_func);
                }

                (*var_ref).value_or_links.value =
                    JS_DupValue(ctx, *self.var_base_ptr.offset(var_idx as isize));

                (*var_ref).pvalue = &mut (*var_ref).value_or_links.value;

                // the reference is no longer to a local variable
                (*var_ref).set_detached(true);
            }
        });
    }
}
