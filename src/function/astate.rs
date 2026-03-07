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
    stack_frame::{ExecMode, StackFrame},
    value::JSValue,
};

#[derive(Debug)]
pub struct AsyncStateImpl {
    /// 'this' argument
    pub(crate) this_val: JSValue,
    /// number of function arguments
    pub(crate) argc: i32,
    /// used to throw an exception in JS_CallInternal()
    pub(crate) throw_flag: bool,
    /// TRUE if the function has returned. The stack frame is no longer valid
    pub(crate) is_completed: bool,
    /// for async function only
    pub(crate) resolving_funcs: [JSValue; 2],
    /// a dedicated stack frame for async execution
    pub(crate) stack_frame: Pin<Box<StackFrame>>,
}

impl GcTrace for AsyncStateImpl {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.this_val.trace(tr);
        self.resolving_funcs[0].trace(tr);
        self.resolving_funcs[1].trace(tr);
        self.stack_frame.trace(tr);
    }
}

impl AsyncStateImpl {
    fn new(
        mut ctx: ContextPtr,
        resolvings: [JSValue; 2],
        bc_func: BCWrapper,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let (arg_count, var_count, stack_size, mode) = {
            let bc = bc_func.payload().bytecode();
            (bc.arg_count, bc.var_count, bc.stack_size, bc.mode())
        };

        // alloc local buf, set up arguments, variables and stack pointer
        let arg_count = std::cmp::max(arg_count as usize, args.len());
        let buf_len = arg_count + var_count as usize + stack_size as usize;

        let mut local_buf = {
            let mut lst = Vec::<JSValue>::new();
            lst.try_reserve_exact(buf_len).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            // initialize with undefined
            unsafe {
                lst.set_len(buf_len);
            }
            lst.fill(JSValue::UNDEFINED);

            // copy arg values
            for (i, a) in args.iter().enumerate() {
                lst[i] = *a;
            }

            lst
        };

        // make a dedicated stack frame for async execution
        let mut stack_frame = Box::pin(StackFrame::new(
            bc_func.into(),
            mode | ExecMode::ASYNC,
            [JSValue::UNDEFINED, JSValue::UNDEFINED],
        ));

        unsafe {
            let sf = stack_frame.as_mut().get_unchecked_mut();

            sf.is_async = true;
            sf.arg_base_ptr = local_buf.as_mut_ptr();
            sf.arg_real_count = args.len() as _;
            sf.arg_count = arg_count as _;
            sf.var_base_ptr = local_buf.as_mut_ptr().add(arg_count as usize);
            sf.var_count = var_count;
            sf.local_buf = local_buf;
        }

        Ok(Self {
            argc: args.len() as _,
            this_val: this_obj,
            resolving_funcs: resolvings,
            throw_flag: false,
            is_completed: false,
            stack_frame,
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

    #[inline(always)]
    pub(crate) const fn set_throw_flag(&mut self, b: bool) {
        self.throw_flag = b;
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

    /// Resume execution of the async function.
    pub(crate) fn resume(&mut self, mut ctx: ContextPtr) -> JSResult {
        debug_assert!(!self.is_completed);

        if js_check_stack_overflow(ctx.rt_ref(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        let mut ret_val = ctx.with_new_scope(|scope| {
            self.resume_call(scope, self.this_val, unsafe {
                std::slice::from_raw_parts(self.arg_ptr().as_ptr(), self.arg_count())
            })
        });

        let completed = match ret_val {
            Ok(JSValue::UNDEFINED) => {
                debug_assert!(self.stack_frame.cur_sp_offset > 0);
                // get stack top value
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
        mut scope: ScopedContext,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> JSResult {
        scope.poll_interrupts()?;

        let throw_flag = self.throw_flag;

        // state has a dedicated stack frame for async function
        let mut func = *self.stack_frame.func_obj.as_BCWrapper().unwrap();
        let pc_offset = self.stack_frame.cur_pc_offset;
        let sp_offset = self.stack_frame.cur_sp_offset;
        debug_assert!(sp_offset >= 0);

        let func_node = func.gc_node_ptr();
        let bf = func.payload_mut();
        let bc = bf.bytecode();

        let ret_val = bc.execute(
            &mut scope,
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
            throw_flag,
        );

        // Note: closure values could be allocated in stack-frame's gc scope during execution,
        // before stack-frame is dropping, promote closure values up to func's gc scope.
        let mut rt = scope.rt_ptr();
        for vr in bf.closure_var_refs() {
            if let Some(n) = vr.value().gc_node_ptr() {
                rt.gc_heap_mut().bind(func_node, n);
            }
        }

        let res = ret_val.err();
        if let Ok(v) = res {
            scope.set_return_value(v);
        }

        res
    }
}

/// async execution state.
/// used to save execution state of an async function or generator.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct AsyncState(GcRef<AsyncStateImpl>);

gcref_wrapper!(AsyncState, AsyncStateImpl);

impl AsyncState {
    pub(crate) fn create(
        mut ctx: ContextPtr,
        resolvings: [JSValue; 2],
        bc_func: BCWrapper,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let inner = AsyncStateImpl::new(ctx, resolvings, bc_func, this_obj, args)?;
        let mut a_state = ctx.rt_mut().gc_alloc(inner).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        // setup backref link: StackFrame::async_state => GcWeak<AsyncStateImpl>
        unsafe {
            let w = a_state.downgrade(ctx.rt_ptr().gc_heap_mut());
            a_state.stack_frame.as_mut().get_unchecked_mut().async_state = w;
        }

        Ok(Self(a_state))
    }
}
