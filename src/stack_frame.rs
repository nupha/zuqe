use std::{marker::PhantomPinned, pin::Pin, ptr::NonNull};

use crate::{
    context::JSContext,
    error::{JS_IsUncatchableError, is_backtrace_needed},
    function::{AsyncStateWeakref, JSVarRef},
    iterator::ObjectIter,
    value::JSValue,
};

/// Represents a call stack frame in JavaScript execution, tracking function state and local variables.
/// StackFrame is usually allocated on stack, not heap.
#[derive(Debug)]
pub struct StackFrame {
    /// Pointer to previous stack frame (NULL for first frame)
    pub prev_frame: *mut Pin<Self>,

    /// Currently executing function.
    ///
    /// The value could be:
    /// - BCFunction object
    /// - native function object
    /// - undefined, which means stack frame is detached (cleared)
    ///
    /// TODO: use Option<JSObject> type?
    pub cur_func: JSValue,

    /// Buffer storing arguments, local variables, and stack space
    /// TODO: Replace with Box<[JSValue]> for better memory efficiency
    pub local_buf: Vec<JSValue>,

    /// Arguments array base pointer and count
    /// Points into local_buf (self-referencing)
    pub arg_base_ptr: *mut JSValue, // TODO: Consider using *const ?
    pub arg_real_count: u16,
    pub arg_count: u16, // at least has `length`

    /// Local variables base pointer and count
    /// Points into local_buf (self-referencing)
    pub var_base_ptr: *mut JSValue,
    pub var_count: u16,

    /// Program counter offset for bytecode functions
    /// Position in local_buf relative to pc_base (see Bytecode::bytecode_start)
    pub cur_pc_offset: usize,

    /// Stack pointer offset (used for generator functions)
    /// -1 when function is actively executing
    /// Position in local_buf relative to stack_base_ptr()
    /// Use stack_cur_ptr() to get raw pointer to this position
    pub cur_sp_offset: i32,

    /// List of variable references registered in this frame
    pub var_ref_list: Vec<JSVarRef>,

    /// Execution mode flags (only JS_MODE_MATH is valid for C functions)
    pub js_mode: i32,

    /// Weak reference to async function state (for async frames only)
    pub async_func: AsyncStateWeakref,

    pub _mark: PhantomPinned,
}

impl Default for StackFrame {
    fn default() -> Self {
        Self {
            prev_frame: std::ptr::null_mut(),
            cur_func: JSValue::UNDEFINED,
            local_buf: Vec::new(),
            arg_base_ptr: std::ptr::null_mut(),
            arg_real_count: 0,
            arg_count: 0,
            var_base_ptr: std::ptr::null_mut(),
            var_count: 0,
            cur_pc_offset: 0,
            cur_sp_offset: 0,
            var_ref_list: Vec::new(),
            js_mode: 0,
            async_func: AsyncStateWeakref::new(),
            _mark: PhantomPinned,
        }
    }
}

impl StackFrame {
    /// Returns slice of actual argument values for this frame
    pub const fn args_slice(&self) -> &[JSValue] {
        if self.arg_real_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.arg_base_ptr, self.arg_real_count as usize) }
        }
    }

    /// Returns the base pointer of the stack in this frame
    #[inline(always)]
    pub const fn stack_base_ptr(&self) -> NonNull<JSValue> {
        unsafe { NonNull::new(self.var_base_ptr.add(self.var_count as usize)).unwrap() }
    }

    /// Returns the current stack pointer position (points one past top value)
    ///
    /// Note: The returned pointer cannot be dereferenced directly as it refers to
    /// an invalid position. To access the top stack value, use offset(-1):
    ///
    /// ```rust
    /// let stack_top_val = self.stack_cur_ptr().offset(-1).as_ref();
    /// ```
    #[inline(always)]
    pub const fn stack_cur_ptr(&self) -> NonNull<JSValue> {
        debug_assert!(self.cur_sp_offset >= 0);
        unsafe { self.stack_base_ptr().add(self.cur_sp_offset as usize) }
    }

    /// Handles current exception by adding backtrace information if needed
    /// Returns:
    /// - Some((pc, sp)) if exception was caught (continue execution at new position)
    /// - None if exception is uncaught (abort execution)
    #[cold]
    pub(crate) fn handle_exception(
        &mut self,
        ctx: &JSContext,
        pc: usize,
        mut sp: usize,
    ) -> Option<(usize, usize)> {
        if is_backtrace_needed(&ctx.rt().cur_exception) {
            // add the backtrace information now:
            // it is not done before if the exception happens in a bytecode operation
            self.cur_pc_offset = pc;
            ctx.build_backtrace_to(&ctx.rt().cur_exception, None, 0);
        }

        if unsafe { !JS_IsUncatchableError(ctx.exception()) } {
            let sp_base = self.stack_base_ptr().as_ptr();

            // find catch offset
            while sp > 0 {
                sp -= 1;
                if let JSValue::CatchOffset(pos) =
                    unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }
                {
                    if pos == 0 {
                        // enumerator: close it with a throw

                        // drop the next method
                        unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) };
                        sp -= 1;

                        // close iterator if SP[-1] is obj
                        if let JSValue::Object(o) =
                            unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) }
                        {
                            let _ = ObjectIter::do_close(ctx, &o, true);
                        }
                    } else {
                        unsafe {
                            std::ptr::replace(sp_base.add(sp), ctx.take_exception());
                        }
                        sp += 1;
                        return Some((pos as usize, sp));
                    }
                }
            }
        }

        None // exception is uncatched, abort with sp=0, pc unchanged
    }

    /// Prepare to exit bytecode running.
    pub(crate) fn handle_exit(
        &mut self,
        ctx: &JSContext,
        pc: usize,
        sp: usize,
        is_generator: bool,
    ) {
        debug_assert!(
            std::ptr::eq(ctx.rt_mut().cur_stack_frame as *mut Self, self),
            "StackFrame::handle_exit() must be called on active stack frame"
        );

        if is_generator {
            // for async, local variables are freed by the caller.
            // Hence the label 'done' should never be reached in async execution.
            self.cur_pc_offset = pc;
            self.cur_sp_offset = sp as i32;
        } else {
            // is normal function
            if !self.var_ref_list.is_empty() {
                // variable references reference the stack: must close them
                self.close_var_refs();
            }

            // free local buf up to stack top (sp)
            // NOTE: this seems not needed. because when local_buf is dropped, rust will automatically do free stuff.
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

        // switch back to previous stack frame
        ctx.rt_mut().cur_stack_frame = self.prev_frame;
    }

    /// Cleans ups for stack frame
    #[cold]
    pub(crate) fn clear(&mut self) {
        self.cur_func = JSValue::UNDEFINED;
        self.local_buf.clear();
        self.var_ref_list.clear();
    }
}
