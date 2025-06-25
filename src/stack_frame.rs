use std::{marker::PhantomPinned, pin::Pin, ptr::NonNull};

use crate::{
    context::JSContext,
    error::{JS_IsUncatchableError, is_backtrace_needed},
    function::{JSAsyncFunctionStateWeakref, JSVarRef},
    iterator::JS_IteratorClose,
    value::JSValue,
};

/// Represents a stack frame in the JavaScript execution context
#[derive(Debug)]
pub struct JSStackFrame {
    /// NULL if first stack frame
    pub prev_frame: *mut Pin<Self>,

    /// current function, JS_UNDEFINED if the frame is detached
    pub cur_func: JSValue,

    /// buffer for arguments, variables, and stack space.
    pub local_buf: Vec<JSValue>,

    /// arguments base pointer and count.
    /// may point to local_buf, self referenced.
    pub arg_base_ptr: *mut JSValue, // TODO: use *const ?
    pub arg_count: u16,

    /// variables base pointer and count.
    /// may point to local_buf, self referenced.
    pub var_base_ptr: *mut JSValue, // TODO: use *const ?
    pub var_count: u16,

    /// PC of the instruction after the call (only used in bytecode functions).
    /// this denotes a position in local_buf biased to pc_base.
    ///
    /// for what **pc_base** means, see JSFunctionBytecode::bytecode_start.
    pub cur_pc_offset: usize,

    /// Current stack pointer value (only used in generators).
    /// -1 if the function is running.
    /// this denotes a position in local_buf biased to `stack_base_ptr()`,
    /// use `stack_cur_ptr()` can get raw pointer to the position.
    pub cur_sp_offset: i32,

    /// registered var refs in stack frame
    pub var_ref_list: Vec<JSVarRef>,

    /// for C functions, only JS_MODE_MATH may be set
    pub js_mode: i32,

    /// if the stack frame is decicated for an async function,
    /// this field hold a weakref of async function state.
    pub async_func: JSAsyncFunctionStateWeakref,

    pub _mark: PhantomPinned,
}

impl Default for JSStackFrame {
    fn default() -> Self {
        Self {
            prev_frame: std::ptr::null_mut(),
            cur_func: JSValue::UNDEFINED,
            local_buf: Vec::new(),
            arg_base_ptr: std::ptr::null_mut(),
            arg_count: 0,
            var_base_ptr: std::ptr::null_mut(),
            var_count: 0,
            cur_pc_offset: 0,
            cur_sp_offset: 0,
            var_ref_list: Vec::new(),
            js_mode: 0,
            async_func: JSAsyncFunctionStateWeakref::new(),
            _mark: PhantomPinned,
        }
    }
}

impl JSStackFrame {
    /// get argument values slice
    #[inline(always)]
    pub const fn args_slice(&self) -> &[JSValue] {
        if self.arg_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.arg_base_ptr, self.arg_count as usize) }
        }
    }

    /// get stack base pointer
    #[inline(always)]
    pub const fn stack_base_ptr(&self) -> NonNull<JSValue> {
        unsafe { NonNull::new(self.var_base_ptr.add(self.var_count as usize)).unwrap() }
    }

    /// get stack current pointer
    #[inline(always)]
    pub const fn stack_cur_ptr(&self) -> NonNull<JSValue> {
        debug_assert!(self.cur_sp_offset >= 0);
        unsafe { self.stack_base_ptr().add(self.cur_sp_offset as usize) }
    }

    /// processing current exception, add backtrace information for example.
    /// return value:
    /// - Some((pc, sp)): exception was catched, continue execution with updated (pc, sp);
    /// - None: exception is unrecovable, abort execution
    pub fn handle_exception(
        &mut self,
        ctx: &JSContext,
        pc: usize,
        mut sp: usize,
    ) -> Option<(usize, usize)> {
        if is_backtrace_needed(&ctx.rt().current_exception) {
            // add the backtrace information now:
            // it is not done before if the exception happens in a bytecode operation
            self.cur_pc_offset = pc;
            ctx.build_backtrace_to(&ctx.rt().current_exception, None, 0);
        }

        if unsafe { !JS_IsUncatchableError(&ctx.exception()) } {
            let sp_base = self.stack_base_ptr().as_ptr();

            // find catch offset
            while sp > 0 {
                sp -= 1;
                let val = unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) };
                if let JSValue::CatchOffset(pos) = val {
                    if pos == 0 {
                        // enumerator: close it with a throw
                        let iter =
                            unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) };
                        unsafe {
                            JS_IteratorClose(ctx, &iter, true).unwrap();
                        }
                        sp -= 1;
                    } else {
                        unsafe {
                            std::ptr::replace(sp_base.add(sp), ctx.take_exception());
                        }
                        sp += 1;
                        return Some((pos as usize, sp));
                    }
                }

                // if val.tag() == JS_TAG_CATCH_OFFSET {
                //     let pos = JS_VALUE_GET_INT(val);
                //     if pos == 0 {
                //         // enumerator: close it with a throw
                //         JS_FreeValue(ctx, *sp_base.add(sp - 1)); // drop the `next` method
                //         sp -= 1;
                //         JS_IteratorClose(ctx, *sp_base.add(sp - 1), true).unwrap();
                //     } else {
                //         *sp_base.add(sp) = ctx.rt_mut().current_exception.replace(JSValue::NULL);
                //         sp += 1;
                //         return Some((pos as usize, sp));
                //     }
                // }
            }
        }

        None // exception is uncatched, abort with sp=0, pc unchanged
    }

    /// clean-ups before exit
    pub fn handle_exit(&mut self, ctx: &JSContext, pc: usize, sp: usize, is_generator: bool) {
        if is_generator {
            // the local variables are freed by the caller in the generator
            // case. Hence the label 'done' should never be reached in a generator function.
            self.cur_pc_offset = pc;
            self.cur_sp_offset = sp as i32;
        } else {
            // is normal function
            if !self.var_ref_list.is_empty() {
                // variable references reference the stack: must close them
                self.close_var_refs(ctx);
            }

            // free local buf to stack top (sp)
            if !self.local_buf.is_empty() {
                let mut pval = self.local_buf.as_mut_ptr();
                let end_ptr = unsafe { self.stack_base_ptr().add(sp).as_ptr() };
                while pval < end_ptr {
                    unsafe {
                        drop(std::ptr::replace(pval, JSValue::UNDEFINED));
                        pval = pval.add(1);
                    }
                }
            }
        }

        ctx.rt_mut().current_stack_frame = self.prev_frame;
    }

    pub fn finalize(&mut self) {
        self.cur_func = JSValue::UNDEFINED;
        self.local_buf.clear();
        self.var_ref_list.clear();
    }
}
