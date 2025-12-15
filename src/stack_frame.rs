use std::{
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
    ptr::NonNull,
};

use gc_lite::{GcTracable, GcWeak};

use crate::{
    builtins::iterator::ObjectIter,
    context::ContextImpl,
    error::{JS_IsUncatchableError, is_backtrace_needed},
    function::{AsyncStateImpl, JSVarRef},
    value::JSValue,
};

bitflags::bitflags! {
    /// exeution attrs of stack frame.
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct ExecAttrs: u8 {
        /// strict mode
        const STRICT = 1 << 0;
        /// strip debug source
        const STRIP  = 1 << 1;
        /// is dedicated stack frame for async function execution
        const ASYNC  = 1 << 2;
    }
}

/// Represents a call stack frame in JavaScript execution, tracking function state and local variables.
/// StackFrame is usually allocated on stack, not heap.
#[derive(Debug)]
pub struct StackFrame {
    /// Pointer to previous stack frame (NULL for first frame)
    pub(crate) prev: *mut Pin<Self>,

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

    /// Execution attrs of stack frame.
    pub(crate) js_mode: ExecAttrs,

    /// Weak back-ref to async state,
    /// only valid if this stack frame is for an AsyncStateImpl
    pub(crate) async_func: GcWeak<AsyncStateImpl>,

    _mark: PhantomPinned,
}

unsafe impl GcTracable for StackFrame {
    #[inline]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.cur_func.trace(tracer);
        // self.local_buf.trace(tracer);

        self.vardefs_slice().iter().for_each(|x| x.trace(tracer));
        self.args_slice().iter().for_each(|x| x.trace(tracer));
        self.stack_slice().iter().for_each(|x| x.trace(tracer));
        self.var_ref_list.iter().for_each(|x| x.trace(tracer));

        if let Some(sf) = unsafe { self.prev.cast::<StackFrame>().as_ref() } {
            sf.trace(tracer);
        }
    }
}

impl PartialEq for StackFrame {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl StackFrame {
    /// Create new StackFrame. It's allocated on stack.
    pub(crate) fn new(func_obj: JSValue, mode: ExecAttrs) -> Self {
        Self {
            prev: std::ptr::null_mut(),
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

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// Returns slice of actual argument values for this frame
    pub const fn args_slice(&self) -> &[JSValue] {
        if self.arg_real_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.arg_base_ptr, self.arg_real_count as usize) }
        }
    }

    pub const fn vardefs_slice(&self) -> &[JSValue] {
        if self.var_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.var_base_ptr, self.var_count as usize) }
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
        if is_backtrace_needed(ctx.rt_ptr().exception()) {
            // add the backtrace information now:
            // it is not done before if the exception happens in a bytecode operation
            self.cur_pc_offset = pc;
            ctx.build_backtrace_to(ctx.rt_ptr().exception(), None, 0);
        }

        if unsafe { !JS_IsUncatchableError(ctx.rt_ptr().exception()) } {
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
                            std::ptr::replace(sp_base.add(sp), ctx.rt_ptr().take_exception());
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
        debug_assert_eq!(
            ctx.rt_ptr().current_stack_frame(),
            Some(&*self),
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
        // ctx.rt().cur_stack_frame = self.prev;
        ctx.rt_ptr().pop_stack_frame();
    }

    /// get stack frame parent iterator
    pub const fn parent_iter(&self, include_self: bool) -> StackFrameParentIter<'_> {
        StackFrameParentIter {
            stack_frame: if include_self {
                self as *const StackFrame as _
            } else {
                self.prev.cast::<StackFrame>()
            },
            _mark: PhantomData,
        }
    }
}

pub struct StackFrameParentIter<'a> {
    stack_frame: *mut StackFrame,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for StackFrameParentIter<'a> {
    type Item = &'a StackFrame;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(f) = unsafe { self.stack_frame.as_ref() } {
            self.stack_frame = f.prev.cast::<StackFrame>();
            Some(f)
        } else {
            None
        }
    }
}

impl StackFrameParentIter<'static> {
    pub const EMPTY: Self = Self {
        stack_frame: std::ptr::null_mut(),
        _mark: PhantomData,
    };
}
