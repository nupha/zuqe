use std::{pin::Pin, ptr::NonNull};

use gc_lite::{GcRef, GcTracable};

use crate::{
    context::ContextImpl, error::JSResult, function::BCFunction, gcref_wrapper,
    runtime::js_check_stack_overflow, stack_frame::ExecMode, stack_frame::StackFrame,
    value::JSValue,
};

#[derive(Debug)]
pub(crate) struct AsyncStateImpl {
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
    /// a dedicated stack frame to run async
    pub(crate) frame: Pin<Box<StackFrame>>,
}

unsafe impl GcTracable for AsyncStateImpl {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.frame.trace(tracer);
        self.this_val.trace(tracer);
        self.resolving_funcs[0].trace(tracer);
        self.resolving_funcs[1].trace(tracer);
    }
}

impl AsyncStateImpl {
    fn new(
        ctx: &ContextImpl,
        resolvings: [JSValue; 2],
        bc_func: BCFunction,
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

        // make a dedicated stack frame for async function execution
        // (the stack frame mode has ASYNC flag set).
        let mut frame = Box::pin(StackFrame::new(bc_func.into(), mode | ExecMode::ASYNC));

        // setup localbuf and sections
        unsafe {
            let sf = frame.as_mut().get_unchecked_mut();
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
            frame,
        })
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub fn arg_ptr(&self) -> NonNull<JSValue> {
        unsafe { NonNull::new_unchecked(self.frame.arg_base_ptr) }
    }

    #[inline(always)]
    pub fn arg_count(&self) -> usize {
        self.frame.arg_real_count as usize
    }

    #[inline(always)]
    pub(crate) const fn set_throw_flag(&mut self, b: bool) {
        self.throw_flag = b;
    }

    /// put given value to stack at position current stack pointer -1
    pub(super) fn set_return_value(&mut self, val: JSValue) {
        unsafe {
            *self
                .frame
                .stack_base_ptr()
                .as_ptr()
                .add(self.frame.cur_sp_offset as usize - 1) = val;
        }
    }

    /// Resume execution of the async function.
    pub(crate) fn resume(&mut self, ctx: &ContextImpl) -> JSResult {
        debug_assert!(!self.is_completed);

        if js_check_stack_overflow(ctx.rt_ref(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // short borrow to avoid conflict
        let (this_val, arg_list) = {
            (self.this_val, unsafe {
                std::slice::from_raw_parts(self.arg_ptr().as_ptr(), self.arg_count())
            })
        };

        // run async func
        let mut ret = self.resume_call(ctx, this_val, arg_list);

        let completed = match ret {
            Ok(JSValue::UNDEFINED) => {
                debug_assert!(self.frame.cur_sp_offset > 0);
                // get stack top value
                ret = Ok(unsafe {
                    std::ptr::replace(
                        // HINT: stack_cur_ptr advances the stack_top pointer by 1.
                        // subtract 1 is the exact addr for stack_top.
                        self.frame.stack_cur_ptr().offset(-1).as_ptr(),
                        JSValue::UNDEFINED,
                    )
                });
                true
            }
            Err(_) => true,
            _ => false,
        };

        if completed {
            // mark end of execution
            self.is_completed = true;

            unsafe {
                // close the closure variables.
                self.frame.as_mut().get_unchecked_mut().close_var_refs();
            }
        }

        ret
    }

    fn resume_call(
        &mut self,
        caller_ctx: &ContextImpl,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> JSResult {
        caller_ctx.poll_interrupts()?;

        let throw_flag = self.throw_flag;

        // state has a dedicated stack frame for async function
        let mut bcf = *self.frame.cur_func.as_object_BCFunction().unwrap();
        let pc_offset = self.frame.cur_pc_offset;
        let sp_offset = self.frame.cur_sp_offset;
        debug_assert!(sp_offset >= 0);

        let isa = bcf.payload_mut();
        let ret_val = isa.bytecode().run(
            caller_ctx,
            isa.home_object(),
            unsafe {
                // use dedicated stack frame for async state
                let sf = self.frame.as_mut().get_unchecked_mut();
                sf.cur_sp_offset = -1;
                sf
            },
            this_obj,
            JSValue::UNDEFINED,
            pc_offset,
            sp_offset as usize,
            &mut isa.var_refs,
            args,
            throw_flag,
        );

        ret_val.err()
    }
}

/// async execution state.
/// used to save execution state of an async function or generator.
#[derive(Debug, Clone, Copy)]
pub struct AsyncFuncState(GcRef<AsyncStateImpl>);

gcref_wrapper!(AsyncFuncState, AsyncStateImpl);

impl PartialEq for AsyncFuncState {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl AsyncFuncState {
    pub fn new_with_resolvings(
        ctx: &ContextImpl,
        resolvings: [JSValue; 2],
        bc_func: BCFunction,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let inner = AsyncStateImpl::new(ctx, resolvings, bc_func, this_obj, args)?;

        let gc_ref = ctx.rt_ptr().gc_alloc(inner).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        let mut a_state = Self(gc_ref);

        // setup backref link for StackFrame to its wrapper AsyncState
        let w = gc_ref.downgrade(ctx.rt_ptr().gc_heap_mut());
        unsafe {
            a_state.frame.as_mut().get_unchecked_mut().async_func = w;
        }

        Ok(a_state)
    }

    /// create new AsyncFuncState, register to gc tracking list.
    #[inline(always)]
    pub fn new(
        ctx: &ContextImpl,
        bc_func: BCFunction,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        Self::new_with_resolvings(
            ctx,
            [const { JSValue::UNDEFINED }; 2],
            bc_func,
            this_obj,
            args,
        )
    }
}
