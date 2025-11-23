use std::{marker::PhantomPinned, pin::Pin, ptr::NonNull};

use gc_lite::{Collect, GcWeak};

use crate::{
    context::ContextImpl,
    error::{JS_IsUncatchableError, is_backtrace_needed},
    function::{AsyncStateImpl, JSVarRef},
    iterator::ObjectIter,
    runtime::RuntimeImpl,
    value::JSValue,
};

/// Represents a call stack frame in JavaScript execution, tracking function state and local variables.
/// StackFrame is usually allocated on stack, not heap.
#[derive(Debug)]
pub struct StackFrame {
    /// Pointer to parent stack frame (NULL for first frame)
    pub(crate) parent: *mut Pin<Self>,

    /// Currently executing function.
    ///
    /// The value could be:
    /// - BCFunction object
    /// - native function object
    /// - undefined, which means stack frame is detached (cleared)
    ///
    /// TODO: use Option<ObjectRef> type?
    pub(crate) cur_func: JSValue,

    /// Buffer storing arguments, local variables, and stack space
    /// TODO: Replace with Box<[JSValue]> for better memory efficiency
    pub(crate) local_buf: Vec<JSValue>,

    /// Arguments array base pointer and count
    /// Points into local_buf (self-referencing)
    pub(crate) arg_base_ptr: *mut JSValue, // TODO: Consider using *const ?
    pub(crate) arg_real_count: u16,
    pub(crate) arg_count: u16, // at least has `length`

    /// Local variables base pointer and count
    /// Points into local_buf (self-referencing)
    pub(crate) var_base_ptr: *mut JSValue,
    pub(crate) var_count: u16,

    /// Program counter offset for bytecode functions
    /// Position in local_buf relative to pc_base (see Bytecode::bytecode_start)
    pub(crate) cur_pc_offset: usize,

    /// Stack pointer offset (used for generator functions)
    /// -1 when function is actively executing
    /// Position in local_buf relative to stack_base_ptr()
    /// Use stack_cur_ptr() to get raw pointer to this position
    pub(crate) cur_sp_offset: i32,

    /// List of variable references registered in this frame
    pub(crate) var_ref_list: Vec<JSVarRef>,

    /// Execution mode flags (only JS_MODE_MATH is valid for C functions)
    pub(crate) js_mode: i32,

    /// Weak back-ref to async state,
    /// only valid if this stack frame is for an AsyncStateImpl
    pub(crate) async_func: GcWeak<AsyncStateImpl>,

    _mark: PhantomPinned,
}

unsafe impl Collect for StackFrame {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        if let Some(sf) = unsafe { self.parent.cast::<StackFrame>().as_ref() } {
            sf.trace(tracer);
        }

        self.cur_func.trace(tracer);
        self.local_buf.trace(tracer);
        self.var_ref_list.trace(tracer);

        // this may have duplicated trace for argument values over local_buf,
        // but this is a guard in case args are not copied to local_buf
        for v in self.args_slice() {
            v.trace(tracer);
        }

        // trace values on stack
        for v in self.stack_slice() {
            v.trace(tracer);
        }
    }
}

impl StackFrame {
    /// Create new StackFrame. It's allocated on stack.
    pub fn new(func_obj: JSValue, mode: i32) -> Self {
        Self {
            parent: std::ptr::null_mut(),
            cur_func: func_obj,
            js_mode: mode,

            local_buf: Vec::new(),
            arg_base_ptr: std::ptr::null_mut(),
            arg_real_count: 0,
            arg_count: 0,
            var_base_ptr: std::ptr::null_mut(),
            var_count: 0,

            async_func: GcWeak::default(),
            var_ref_list: Vec::new(),
            cur_pc_offset: 0,
            cur_sp_offset: 0,

            _mark: PhantomPinned,
        }
    }

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
    /// `let stack_top_val = frame.stack_cur_ptr().offset(-1).as_ref();`
    #[inline(always)]
    pub const fn stack_cur_ptr(&self) -> NonNull<JSValue> {
        debug_assert!(self.cur_sp_offset >= 0);
        unsafe { self.stack_base_ptr().add(self.cur_sp_offset as usize) }
    }

    /// stack values slice
    pub fn stack_slice(&self) -> &[JSValue] {
        debug_assert!(self.cur_sp_offset >= 0);
        if self.cur_sp_offset != 0 {
            unsafe {
                let len = self.cur_sp_offset as usize;
                std::slice::from_raw_parts(self.stack_base_ptr().as_ptr(), len)
            }
        } else {
            &[]
        }
    }

    /// Handles current exception by adding backtrace information if needed
    /// Returns:
    /// - Some((pc, sp)) if exception was caught (continue execution at new position)
    /// - None if exception is uncaught (abort execution)
    #[cold]
    pub(crate) fn handle_exception(
        &mut self,
        ctx: &ContextImpl,
        pc: usize,
        mut sp: usize,
    ) -> Option<(usize, usize)> {
        if is_backtrace_needed(ctx.rt().exception()) {
            // add the backtrace information now:
            // it is not done before if the exception happens in a bytecode operation
            self.cur_pc_offset = pc;
            ctx.build_backtrace_to(ctx.rt().exception(), None, 0);
        }

        if unsafe { !JS_IsUncatchableError(ctx.rt().exception()) } {
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
                            let _ = ObjectIter::do_close(ctx, o, true);
                        }
                    } else {
                        unsafe {
                            std::ptr::replace(sp_base.add(sp), ctx.rt().take_exception());
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
        ctx: &ContextImpl,
        pc: usize,
        sp: usize,
        is_generator: bool,
    ) {
        debug_assert!(
            std::ptr::eq(ctx.rt().cur_stack_frame as *mut Self, self),
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
                        std::ptr::replace(pval, JSValue::UNDEFINED);
                        pval = pval.add(1);
                    }
                }
            }
        }

        // switch back to previous stack frame
        ctx.rt().cur_stack_frame = self.parent;
    }

    /// Cleans ups for stack frame
    #[cold]
    #[deprecated]
    pub(crate) fn clear(&mut self) {
        self.cur_func = JSValue::UNDEFINED;
        self.local_buf.clear();
        self.var_ref_list.clear();
    }
}

impl RuntimeImpl {
    /// Pushes a new stack frame onto the execution stack
    ///
    /// # Safety
    /// The caller must ensure the frame pointer is valid
    pub(crate) const fn push_stack_frame(&mut self, mut stack_frame: NonNull<StackFrame>) {
        if self.cur_stack_frame.is_null() {
            self.cur_stack_frame = stack_frame.cast::<Pin<StackFrame>>().as_ptr();
        } else {
            unsafe {
                stack_frame.as_mut().parent = self.cur_stack_frame;
            }
            self.cur_stack_frame = stack_frame.cast::<Pin<StackFrame>>().as_ptr();
        }
    }

    /// Pops the current stack frame from the execution stack
    ///
    /// # Safety
    /// The caller must ensure no references to the popped frame remain
    pub(crate) fn pop_stack_frame(&mut self) -> *mut Pin<StackFrame> {
        if self.cur_stack_frame.is_null() {
            std::ptr::null_mut()
        } else {
            let sf = self.cur_stack_frame;
            self.cur_stack_frame = unsafe { (*sf.cast::<StackFrame>()).parent };
            sf
        }
    }
}
