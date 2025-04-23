use std::ptr::NonNull;

use crate::{
    context::JSContext,
    error::{build_backtrace, is_backtrace_needed, JS_IsUncatchableError},
    iterator::JS_IteratorClose,
    types::{list_empty, list_head},
    value::{JSTag, JSValue, JS_FreeValue, JS_VALUE_GET_INT, JS_VALUE_GET_TAG},
};

/// Represents a stack frame in the JavaScript execution context
#[repr(C)]
pub struct JSStackFrame {
    /// NULL if first stack frame
    pub prev_frame: *mut Self,

    /// current function, JS_UNDEFINED if the frame is detached
    pub cur_func: JSValue,

    /// buffer for arguments, variables, and stack space.
    pub local_buf: Vec<JSValue>,

    /// arguments base pointer and count
    pub arg_base_ptr: *mut JSValue,
    pub arg_count: u16,

    /// variables base pointer and count
    pub var_base_ptr: *mut JSValue, // TODO: use var_offset:usize instead
    pub var_count: u16,

    /// only used in bytecode functions : PC of the instruction after the call.
    /// this is the offset to pc base
    pub cur_pc_offset: usize,
    /// only used in generators. Current stack pointer value. 0 if the function is running.
    /// this is the offset to sp base
    pub cur_sp_offset: usize,

    /// registered var refs in stack frame
    /// list of JSVarRef.var_ref_link
    pub var_ref_list: list_head,

    /// for C functions, only JS_MODE_MATH may be set
    pub js_mode: i32,
}

impl JSStackFrame {
    /// get argument values slice
    #[inline(always)]
    pub fn args_slice(&self) -> &[JSValue] {
        if self.arg_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.arg_base_ptr, self.arg_count as usize) }
        }
    }

    /// get stack base pointer
    #[inline(always)]
    pub fn stack_base_ptr(&self) -> NonNull<JSValue> {
        unsafe { NonNull::new(self.var_base_ptr.add(self.var_count as usize)).unwrap() }
    }

    /// get stack current pointer
    #[inline(always)]
    pub fn stack_current_ptr(&self) -> NonNull<JSValue> {
        unsafe { self.stack_base_ptr().add(self.cur_sp_offset) }
    }

    /// processing current exception, add backtrace information for example.
    /// return value:
    /// - Some((pc, sp)): exception was catched, continue execution with updated (pc, sp);
    /// - None: exception is unrecovable, abort execution
    pub unsafe fn handle_exception(
        &mut self,
        ctx: &mut JSContext,
        pc: usize,
        mut sp: usize,
    ) -> Option<(usize, usize)> {
        if is_backtrace_needed(ctx.rt().current_exception) {
            // add the backtrace information now:
            // it is not done before if the exception happens in a bytecode operation
            self.cur_pc_offset = pc;

            build_backtrace(
                ctx,
                ctx.rt().current_exception,
                std::ptr::null(),
                Some(0),
                0,
            );
        }

        if !JS_IsUncatchableError(ctx, ctx.rt().current_exception) {
            let sp_base = self.stack_base_ptr().as_ptr();

            // find catch offset
            while sp > 0 {
                sp -= 1;
                let val = *sp_base.add(sp);

                // debug
                // {
                //     print!(
                //         "[O.o]         pc={pc}, sp={sp}, tag={:?} {}, rc={:?}, obj_cls={:?} ",
                //         JS_VALUE_GET_NORM_TAG(val),
                //         JS_VALUE_GET_TAG(val),
                //         val.ref_count(),
                //         if JS_VALUE_GET_TAG(val) == JSTag::Object as i32 {
                //             (*JS_VALUE_GET_OBJ(val)).class_id()
                //         } else {
                //             9999
                //         }
                //     );
                //     val.dump(ctx.rt());
                //     println!();
                // }

                JS_FreeValue(ctx, val);

                if JS_VALUE_GET_TAG(val) == JSTag::CatchOffset as i32 {
                    let pos = JS_VALUE_GET_INT(val);
                    if pos == 0 {
                        // enumerator: close it with a throw
                        JS_FreeValue(ctx, *sp_base.add(sp - 1)); // drop the `next` method
                        sp -= 1;
                        JS_IteratorClose(ctx, *sp_base.add(sp - 1), true).unwrap();
                    } else {
                        *sp_base.add(sp) = ctx.rt_mut().current_exception.replace(JSValue::NULL);
                        sp += 1;
                        return Some((pos as usize, sp));
                    }
                }
            }
        }

        None // exception is uncatched, abort with sp=0, pc unchanged
    }

    /// clean-ups before exit
    pub unsafe fn handle_exit(
        &mut self,
        ctx: &mut JSContext,
        pc: usize,
        sp: usize,
        is_generator: bool,
    ) {
        if is_generator {
            // the local variables are freed by the caller in the generator
            // case. Hence the label 'done' should never be reached in a generator function.
            self.cur_pc_offset = pc;
            self.cur_sp_offset = sp;
        } else {
            // is normal function
            if !list_empty(&mut self.var_ref_list) {
                // variable references reference the stack: must close them
                self.close_var_refs(ctx);
            }

            // free local buf to stack top (sp)
            if !self.local_buf.is_empty() {
                let mut pval = self.local_buf.as_ptr();
                let sp_top = self.stack_base_ptr().add(sp).as_ptr();

                while pval < sp_top {
                    JS_FreeValue(ctx, *pval);
                    pval = pval.add(1);
                }
            }
        }

        ctx.rt_mut().current_stack_frame = self.prev_frame;
    }
}
