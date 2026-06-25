// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{pin::Pin, ptr::NonNull};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    ScopedContext,
    context::ContextPtr,
    error::JSResult,
    function::BCWrapper,
    gcref_wrapper,
    runtime::js_check_stack_overflow,
    stack_frame::{ExecMode, FrameBuf, StackFrame},
    value::JSValue,
};

/// Async execution state for bytecode running, hold a dedicated stack frame for bytecode running.
/// Used to save execution state of an async function or generator.
#[derive(Debug)]
pub struct AsyncStateImpl {
    /// 'this' argument
    pub(crate) this_val: JSValue,

    /// indicates there is an exception on runtime (used by  JS_CallInternal())
    #[deprecated(
        note = "this method is useless, you may detect exception by checking runtime current exception != Exception::No"
    )]
    pub(crate) throw_flag: bool,

    /// TRUE if the function has returned. The stack frame is no longer valid
    pub(crate) is_completed: bool,
    /// for async function only
    pub(crate) resolving_funcs: [JSValue; 2],
    /// a dedicated stack frame for async execution
    pub(crate) stack_frame: Pin<Box<StackFrame>>,
}

impl GcTrace for AsyncStateImpl {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.this_val.trace(gcx);
        self.resolving_funcs[0].trace(gcx);
        self.resolving_funcs[1].trace(gcx);
        self.stack_frame.trace(gcx);
    }
}

impl AsyncStateImpl {
    fn new(
        mut ctx: ContextPtr,
        resolving_funcs: [JSValue; 2],
        bytecode_func: BCWrapper,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let (arg_count, var_count, local_buf_size, mode) = {
            let bc = bytecode_func.payload().bytecode();
            (bc.arg_count, bc.var_count, bc.local_buf_size, bc.mode())
        };

        // alloc local buf, set up arguments, variables and stack pointer
        let arg_count = std::cmp::max(arg_count as usize, args.len());
        let buf_len = arg_count + local_buf_size as usize;

        // make a dedicated stack frame for async execution
        let mut stack_frame = Box::pin(StackFrame::new(
            bytecode_func.into(),
            mode | ExecMode::ASYNC,
            [JSValue::UNDEFINED, JSValue::UNDEFINED],
        ));

        let sf = unsafe { stack_frame.as_mut().get_unchecked_mut() };
        sf.is_async = true;

        // Initialize local_buf directly in-place to avoid the
        // SmallVec-move-then-dangling-pointer pitfall.
        sf.local_buf = FrameBuf::new_alloc(buf_len);
        sf.local_buf.write_slice(0, args);

        sf.arg_real_count = args.len() as _;
        sf.arg_count = arg_count as _;
        sf.var_count = var_count;

        // Derive pointers from the final buffer location.
        // With FrameBuf, the backing memory is fixed after construction.
        sf.arg_base_ptr = sf.local_buf.as_mut_ptr();
        sf.var_base_ptr = unsafe { sf.local_buf.as_mut_ptr().add(arg_count as usize) };

        Ok(Self {
            this_val: this_obj,
            resolving_funcs,
            is_completed: false,
            stack_frame,
            throw_flag: false,
        })
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub fn arg_ptr(&self) -> NonNull<JSValue> {
        unsafe { NonNull::new_unchecked(self.stack_frame.arg_base_ptr) }
    }

    #[inline(always)]
    pub fn arg_count(&self) -> usize {
        self.stack_frame.arg_real_count as usize
    }

    /// Note: is_reject=true indicates runtime must be an effective Exception
    #[deprecated(
        note = "this method is useless, you may detect exception by checking runtime current exception != Exception::No"
    )]
    #[inline(always)]
    pub(crate) const fn set_throw_flag(&mut self, is_reject: bool) {
        self.throw_flag = is_reject;
    }

    /// put given value to stack at position current stack pointer -1
    pub(super) fn set_return_value(&mut self, val: JSValue) {
        unsafe {
            *self
                .stack_frame
                .stack_base_ptr()
                .as_ptr()
                .add(self.stack_frame.cur_sp_offset as usize - 1) = val;
        }
    }

    /// Take the top value from the async frame stack and protect it
    /// in the current GC scope. After this call the stack slot is UNDEFINED
    /// and the returned value is guaranteed to survive GC within this scope.
    ///
    /// Unlike `pop`, this does NOT adjust SP — it only reads and clears SP[-1].
    /// This is the safe alternative to raw `ptr::replace` on the async stack.
    #[inline(always)]
    pub fn take_top(&mut self, ctx: &ScopedContext) -> JSValue {
        self.stack_frame.take(-1, ctx)
    }

    /// Resume execution of the async function.
    pub(crate) fn resume(&mut self, ctx: &mut ScopedContext) -> JSResult {
        debug_assert!(!self.is_completed);

        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        let mut ret_val = ctx.with_new_scope(|scope| {
            let res = self.resume_call(scope, self.this_val, unsafe {
                std::slice::from_raw_parts(self.arg_ptr().as_ptr(), self.arg_count())
            });
            res
        });

        let completed = match ret_val {
            Ok(JSValue::UNDEFINED) => {
                debug_assert!(self.stack_frame.cur_sp_offset > 0);
                // get stack top value
                //
                // SAFETY: The value read here has lost its LOCAL protection
                // (child scope exited above). However, between this ptr::replace
                // and the return, the only operation is close_var_refs() which
                // does NOT allocate GC objects. Therefore arena GC cannot fire
                // during this window. The caller is responsible for protection.
                ret_val = Ok(unsafe {
                    std::ptr::replace(
                        // HINT: stack_cur_ptr advances the stack_top pointer by 1.
                        // subtract 1 is the exact addr for stack_top.
                        self.stack_frame.stack_cur_ptr().offset(-1).as_ptr(),
                        JSValue::UNDEFINED,
                    )
                });
                true
            }
            Err(_) => true,
            _ => false,
        };

        if completed {
            self.is_completed = true;
            unsafe {
                // close closure variables.
                self.stack_frame
                    .as_mut()
                    .get_unchecked_mut()
                    .close_var_refs();
            }
        }

        ret_val
    }

    /// Resume executing async function in given scope
    fn resume_call(
        &mut self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> JSResult {
        if ctx.poll_interrupts() {
            return Err(-1);
        }

        // state has a dedicated stack frame for async function
        let mut func = *self.stack_frame.func_obj.as_BCWrapper().unwrap();
        let pc_offset = self.stack_frame.cur_pc_offset;
        let sp_offset = self.stack_frame.cur_sp_offset;
        debug_assert!(sp_offset >= 0);

        let func_node_ptr = func.gc_node_ptr().as_ptr();

        let bf = func.payload_mut();
        let bc = bf.bytecode();

        let ret_val = bc.execute(
            ctx,
            bf.home_object(),
            unsafe {
                self.stack_frame.as_mut().get_unchecked_mut() // dedicated stack frame for async state
            },
            this_obj,
            JSValue::UNDEFINED,
            pc_offset,
            sp_offset as usize,
            bf.closure_var_refs_mut(),
            args,
            self.throw_flag,
        );

        // Note: closure values could be allocated in stack-frame's gc scope during execution,
        // before stack-frame is dropping, promote closure values up to func's gc scope.
        let func_node = unsafe { std::ptr::NonNull::new_unchecked(func_node_ptr) };
        let mut rt = ctx.rt_ptr();
        for vr in bf.closure_var_refs() {
            if let Some(n) = vr.value().gc_node_ptr() {
                rt.gc_heap_mut().bind(func_node, n);
            }
        }

        ret_val.err()
    }
}

/// async execution state.
/// used to save execution state of an async function or generator.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct AsyncState(GcRef<AsyncStateImpl>);

gcref_wrapper!(AsyncState, AsyncStateImpl);

impl AsyncState {
    pub(crate) fn new(
        ctx: &mut ScopedContext,
        resolvings: [JSValue; 2],
        bytecode_func: BCWrapper,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let inner = AsyncStateImpl::new(ctx.into(), resolvings, bytecode_func, this_obj, args)?;
        let mut a_state: AsyncState = ctx
            .alloc_local(inner)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?
            .into();

        // setup backref link: StackFrame::async_state => GcWeak<AsyncStateImpl>
        unsafe {
            let w = a_state.gc_ref().downgrade(ctx.gc_heap_mut());
            a_state.stack_frame.as_mut().get_unchecked_mut().async_state = w;
        }

        Ok(a_state)
    }
}
