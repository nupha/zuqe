// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
    ptr::NonNull,
};

use gc_lite::{GcTrace, GcTraceCtx, GcWeak};

use crate::{
    Object,
    builtins::iterator::ObjectIter,
    context::ScopedContext,
    error::{Exception, is_backtrace_needed},
    function::{AsyncStateImpl, VarRef},
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

    /// store Runtime pointer, initialized to func_obj.rt() at first.
    /// used to keep access to Runtime in case has been func_obj disposed.
    #[deprecated(note = "no use")]
    pub(crate) rt: RuntimePtr,

    /// Associated function of this stack frame, could be:
    /// - Bytecode Function object
    /// - Native function object
    pub(crate) func_obj: Object,

    /// extra values that should avoid been gc collected alone with this stack frame:
    /// e.g. this_obj, new_target. allows max 2 values.
    #[deprecated(
        note = "Use scope to protect gc nodes, filling proteced values in stack frame is not courraged"
    )]
    pub(crate) protected_values: [JSValue; 2],

    /// Execution mode
    pub(crate) mode: ExecMode,

    /// This stack frame is within AsyncState?
    pub(crate) is_async: bool,
    /// Weak backref to async state,
    /// only valid if stack frame is dedicated for an AsyncStateImpl
    pub(crate) async_state: GcWeak<AsyncStateImpl>,

    /// Buffer storing arguments, local variables, and stack space
    pub(crate) local_buf: Vec<JSValue>,

    /// Argument values base pointer and count, points into local_buf (self-referencing)
    /// For native callback stack frame, this pointer should be null.
    pub(crate) arg_base_ptr: *mut JSValue,
    pub(crate) arg_real_count: u16,
    pub(crate) arg_count: u16, // at least has `length`

    /// Local variables base pointer and count, points into local_buf (self-referencing).
    /// For native callback stack frame, this pointer should be null.
    pub(crate) var_base_ptr: *mut JSValue,
    pub(crate) var_count: u16,

    /// List of variable references to local vardefs in stack frame
    var_ref_list: Vec<VarRef>,

    /// Program counter offset for bytecode functions
    /// Position in local_buf relative to pc_base (see Bytecode::bytecode_start)
    pub(crate) cur_pc_offset: usize,
    /// Stack pointer offset (used for generator functions)
    /// Position in local_buf relative to stack_base_ptr()
    /// Use stack_cur_ptr() to get raw pointer to this position
    pub(crate) cur_sp_offset: i32,

    _mark: PhantomPinned,
}

impl GcTrace for StackFrame {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.func_obj.trace(tr);
        self.protected_values[0].trace(tr);
        self.protected_values[1].trace(tr);

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

        for vr in self.var_refs() {
            vr.trace(tr)
        }
        for v in self.var_values() {
            v.trace(tr)
        }
        for v in self.stack_values() {
            v.trace(tr)
        }

        // trace parent stack frames
        if let Some(up) = self.parent() {
            up.trace(tr);
        }
    }
}

impl PartialEq for StackFrame {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(self, other)
    }
}

impl StackFrame {
    pub(crate) fn new(func_obj: Object, mode: ExecMode, protected_values: [JSValue; 2]) -> Self {
        Self {
            prev: std::ptr::null_mut(),
            rt: func_obj.rt_ptr(),

            func_obj,
            mode,
            protected_values,
            is_async: false,
            async_state: GcWeak::default(),

            local_buf: Vec::new(),
            arg_base_ptr: std::ptr::null_mut(),
            arg_real_count: 0,
            arg_count: 0,
            var_base_ptr: std::ptr::null_mut(),
            var_count: 0,

            var_ref_list: Vec::new(),
            cur_pc_offset: 0,
            cur_sp_offset: 0,

            _mark: PhantomPinned,
        }
    }

    /// get parent stack frame
    #[inline(always)]
    pub const fn parent(&self) -> Option<&StackFrame> {
        unsafe { self.prev.cast::<StackFrame>().as_ref() }
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

    #[inline(always)]
    pub const fn var_refs(&self) -> &Vec<VarRef> {
        &self.var_ref_list
    }

    #[inline(always)]
    pub const fn var_refs_mut(&mut self) -> &mut Vec<VarRef> {
        &mut self.var_ref_list
    }

    /// Handles current exception by adding backtrace information if needed
    /// Returns:
    /// - Some((pc, sp)) if exception was caught (continue execution at new position)
    /// - None if exception is uncaught (abort execution)
    #[cold]
    pub(crate) fn handle_exception(
        &mut self,
        ctx: &mut ScopedContext,
        pc: usize,
        mut sp: usize,
    ) -> Option<(usize, usize)> {
        let exc = ctx.rt().exception();
        debug_assert!(exc != Exception::No);

        if let Exception::Error(err_val) = exc
            && is_backtrace_needed(err_val)
        {
            // add the backtrace information now:
            // it is not done before if the exception happens in a bytecode operation
            self.cur_pc_offset = pc;
            ctx.build_backtrace_to(err_val, None, 0);
        }

        if !exc.is_uncatchable() {
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
                            let _ = ObjectIter::close_iter_internal(ctx, o);
                        }
                    } else {
                        let exc = ctx.take_exception();
                        let err_val = exc.to_value(ctx);
                        unsafe {
                            std::ptr::replace(sp_base.add(sp), err_val);
                        }
                        sp += 1;
                        return Some((pos as usize, sp));
                    }
                }
            }
        }

        None // exception is uncatched, abort with sp=0, pc unchanged
    }

    /// Prepare to exit bytecode execution.
    pub(crate) fn handle_exit(
        &mut self,
        ctx: &mut ScopedContext,
        pc: usize,
        sp: usize,
        is_async: bool,
    ) {
        debug_assert_eq!(
            ctx.rt().current_stack_frame(),
            Some(&*self),
            "StackFrame::handle_exit() must be called on current stack frame"
        );

        if is_async {
            // async execution, local variables are freed by the caller.
            // Hence the label 'done' should never be reached in async execution.
            self.cur_pc_offset = pc;
            self.cur_sp_offset = sp as i32;
        } else {
            // var_refs may still referencing the stack,
            // before stack frame drop, detatch all var_refs.
            self.close_var_refs();

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
        ctx.rt_mut().pop_stack_frame();
    }

    /// get stack frame parent iterator
    pub const fn parent_iter(&self, include_self: bool) -> StackFrameParentIter<'_> {
        StackFrameParentIter {
            current: if include_self {
                Some(NonNull::from_ref(self))
            } else {
                NonNull::new(self.prev.cast())
            },
            _mark: PhantomData,
        }
    }

    // pub(crate) fn gc_nodes(&self) -> impl Iterator<Item = NonNull<GcHead>> {
    //     let args_iter = unsafe {
    //         std::slice::from_raw_parts(
    //             self.arg_base_ptr,
    //             std::cmp::max(self.arg_real_count, self.arg_count) as usize,
    //         )
    //         .iter()
    //         .filter_map(move |v| v.gc_node_ptr())
    //     };

    //     let vars_iter = self
    //         .var_values()
    //         .iter()
    //         .filter_map(move |v| v.gc_node_ptr());

    //     let stack_iter = self
    //         .stack_values()
    //         .iter()
    //         .filter_map(move |v| v.gc_node_ptr());

    //     let var_refs_iter = self.var_refs().iter().map(|v| v.gc_ref().node_ptr());

    //     std::iter::once(self.func_obj.gc_ref().node_ptr())
    //         .chain(args_iter)
    //         .chain(vars_iter)
    //         .chain(stack_iter)
    //         .chain(var_refs_iter)
    // }
}

pub struct StackFrameParentIter<'a> {
    current: Option<NonNull<StackFrame>>,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for StackFrameParentIter<'a> {
    type Item = &'a StackFrame;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(sf) = self.current.take() {
            unsafe {
                self.current = NonNull::new(sf.as_ref().prev.cast::<StackFrame>());
                Some(sf.as_ref())
            }
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
