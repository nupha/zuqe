use std::{
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
    ptr::NonNull,
};

use gc_lite::{GcHead, GcPartitionId, GcTracable, GcTraceOp, GcWeak};

use crate::{
    Object,
    builtins::iterator::ObjectIter,
    context::ContextImpl,
    error::{JS_IsUncatchableError, is_backtrace_needed},
    function::{AsyncStateImpl, JSVarRef},
    runtime::RuntimePtr,
    value::JSValue,
};

bitflags::bitflags! {
    /// execution flag
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct ExecMode: u8 {
        /// strict mode
        const STRICT = 1 << 0;
        /// strip debug source
        const STRIP  = 1 << 1;
        /// use dedicated stack frame for async function execution
        const ASYNC  = 1 << 2;
    }
}

/// A stack frame to execute bytecode or native function,
/// holding function state, arguments, local variables and stack values.
#[derive(Debug)]
pub struct StackFrame {
    /// Raw pointer to parent stack frame (NULL means top)
    pub(super) prev: *mut Pin<Self>,

    /// Associated function of this stack frame, could be:
    /// - Bytecode Function object
    /// - Native function object
    pub(crate) func_obj: Object,

    /// Buffer storing arguments, local variables, and stack space
    pub(crate) local_buf: Vec<JSValue>,

    /// Arguments array base pointer and count
    /// Points into local_buf (self-referencing)
    pub(crate) arg_base_ptr: *mut JSValue,
    pub(crate) arg_real_count: u16,
    pub(crate) arg_count: u16, // at least has `length`

    /// Local variables base pointer, points into local_buf (self-referencing)
    /// native callback's stack frame don't have var, this pointer is null.
    pub(crate) var_base_ptr: *mut JSValue,
    /// Local variables count
    pub(crate) var_count: u16,

    /// List of variable references registered in this frame
    pub(crate) var_ref_list: Vec<JSVarRef>,

    /// Program counter offset for bytecode functions
    /// Position in local_buf relative to pc_base (see Bytecode::bytecode_start)
    pub(crate) cur_pc_offset: usize,
    /// Stack pointer offset (used for generator functions)
    /// Position in local_buf relative to stack_base_ptr()
    /// Use stack_cur_ptr() to get raw pointer to this position
    pub(crate) cur_sp_offset: i32,

    /// Execution attrs of stack frame.
    pub(crate) mode: ExecMode,

    /// Weak backref to async state,
    /// only valid if stack frame is for an AsyncStateImpl
    pub(crate) async_state: GcWeak<AsyncStateImpl>,

    /// gc partitionid for stack frame scoped gc allocs
    pub(crate) gc_scope_id: GcPartitionId,

    _mark: PhantomPinned,
}

unsafe impl GcTracable for StackFrame {
    fn trace(&self, mut tr: GcTraceOp) {
        self.func_obj.trace(tr);

        // trace args
        let cnt = std::cmp::max(self.arg_real_count, self.arg_count);
        if cnt > 0 {
            let mut p = self.arg_base_ptr;
            for _ in 0..cnt {
                unsafe {
                    (&*p).trace(tr);
                    p = p.add(1);
                }
            }
        }

        // local vars
        tr.add_nodes(self.var_values().iter().filter_map(|v| v.gc_node_ptr()));
        // stack values
        tr.add_nodes(self.stack_values().iter().filter_map(|v| v.gc_node_ptr()));
        // varrefs
        self.var_ref_list.iter().for_each(|r| r.trace(tr));

        // trace upward along chain
        // if let Some(sf) = self.parent() {
        //     sf.trace(tracer);
        // }
    }
}

impl PartialEq for StackFrame {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(self, other)
    }
}

impl Drop for StackFrame {
    fn drop(&mut self) {
        let pid = self.gc_scope_id;
        if pid != GcPartitionId::NONE {
            let mut rt = self.rt();
            rt.gc_heap_mut().remove_partition(pid);
        }
    }
}

impl StackFrame {
    /// Create new StackFrame. It's allocated on stack.
    pub(crate) fn new(func_obj: Object, mode: ExecMode) -> Self {
        Self {
            prev: std::ptr::null_mut(),
            func_obj,
            mode,

            local_buf: Vec::new(),
            arg_base_ptr: std::ptr::null_mut(),
            arg_real_count: 0,
            arg_count: 0,
            var_base_ptr: std::ptr::null_mut(),
            var_count: 0,

            async_state: GcWeak::default(),
            var_ref_list: Vec::new(),
            cur_pc_offset: 0,
            cur_sp_offset: 0,

            gc_scope_id: GcPartitionId::NONE,

            _mark: PhantomPinned,
        }
    }

    /// get runtime
    #[inline(always)]
    fn rt(&self) -> RuntimePtr {
        self.func_obj.rt()
    }

    /// get parent stack frame
    #[inline(always)]
    pub const fn parent(&self) -> Option<&StackFrame> {
        unsafe { self.prev.cast::<StackFrame>().as_ref() }
    }

    /// get parent stack frame
    #[inline(always)]
    pub const fn parent_mut(&mut self) -> Option<&mut StackFrame> {
        unsafe { self.prev.cast::<StackFrame>().as_mut() }
    }

    /// get-exist or lazy-init gc scope for this stack frame.
    pub(crate) fn init_gc_scope(&mut self) -> GcPartitionId {
        if self.gc_scope_id == GcPartitionId::NONE {
            let mut rt = self.rt();

            let parent = if let Some(up) = unsafe { self.prev.cast::<StackFrame>().as_mut() } {
                up.init_gc_scope()
            } else {
                rt.gc_root_scope
            };

            self.gc_scope_id = rt.gc_heap_mut().create_sub_partition(parent);
        }

        self.gc_scope_id
    }

    /// perform gc collect for scope of this stack frame
    pub(crate) fn gc_collect(&self) {
        if self.gc_scope_id != GcPartitionId::NONE {
            self.rt().gc_collect_in(self.gc_scope_id);
        }
    }

    /// Returns the base pointer of the stack in this frame.
    /// Stack frame for native callback does not have stack, calling in this case will panic.
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
    pub(crate) const fn stack_cur_ptr(&self) -> NonNull<JSValue> {
        debug_assert!(self.cur_sp_offset >= 0);
        unsafe { self.stack_base_ptr().add(self.cur_sp_offset as usize) }
    }

    pub const fn stack_values(&self) -> &[JSValue] {
        debug_assert!(self.cur_sp_offset >= 0);
        if self.cur_sp_offset > 0 {
            unsafe {
                std::slice::from_raw_parts(
                    self.stack_base_ptr().as_ptr(),
                    self.cur_sp_offset as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// Returns *Actual* argument values
    pub const fn arg_values(&self) -> &[JSValue] {
        if self.arg_real_count > 0 {
            unsafe { std::slice::from_raw_parts(self.arg_base_ptr, self.arg_real_count as usize) }
        } else {
            &[]
        }
    }

    /// Returns local variable values
    pub const fn var_values(&self) -> &[JSValue] {
        if self.var_count > 0 {
            unsafe { std::slice::from_raw_parts(self.var_base_ptr, self.var_count as usize) }
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
                            let _ = ObjectIter::close_internal(ctx, o, true);
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
        ctx.rt_ptr().pop_stack_frame();
    }

    /// get stack frame parent iterator
    pub const fn parent_iter(&self, include_self: bool) -> StackFrameParentIter<'_> {
        StackFrameParentIter {
            current: if include_self {
                Some(NonNull::from_ref(self))
            } else {
                unsafe {
                    if !self.prev.is_null() {
                        Some(NonNull::new_unchecked(self.prev.cast()))
                    } else {
                        None
                    }
                }
            },
            _mark: PhantomData,
        }
    }

    pub fn gc_nodes(&self) -> impl Iterator<Item = NonNull<GcHead>> {
        let args_iter = unsafe {
            std::slice::from_raw_parts(
                self.arg_base_ptr,
                std::cmp::max(self.arg_real_count, self.arg_count) as usize,
            )
            .iter()
            .filter_map(move |v| v.gc_node_ptr())
        };

        let vars_iter = self
            .var_values()
            .iter()
            .filter_map(move |v| v.gc_node_ptr());

        let stack_iter = self
            .stack_values()
            .iter()
            .filter_map(move |v| v.gc_node_ptr());

        let var_refs_iter = self.var_ref_list.iter().map(|v| v.gc_ref().node_ptr());

        std::iter::once(self.func_obj.gc_ref().node_ptr())
            .chain(args_iter)
            .chain(vars_iter)
            .chain(stack_iter)
            .chain(var_refs_iter)
    }
}

pub struct StackFrameParentIter<'a> {
    current: Option<NonNull<StackFrame>>,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for StackFrameParentIter<'a> {
    type Item = &'a StackFrame;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(sf) = self.current.take() {
            self.current.replace(sf);
            Some(unsafe { sf.as_ref() })
        } else {
            None
        }
    }
}

impl StackFrameParentIter<'static> {
    pub const EMPTY: Self = Self {
        current: None,
        _mark: PhantomData,
    };
}
