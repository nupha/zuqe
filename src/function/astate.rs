use std::{
    cell::RefCell,
    pin::Pin,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    context::{JS_MODE_ASYNC, JSContext},
    error::JSResult,
    function::BCFunction,
    gc::GCObjectHeader,
    object::{GCObjectWeakref, ObjectPayload},
    runtime::{JSRuntime, js_check_stack_overflow},
    stack_frame::StackFrame,
    value::JSValue,
};

use super::call::{JS_CALL_FLAG_GENERATOR, JS_CallInternal};

#[derive(Debug)]
pub(crate) struct AsyncStateInner {
    pub header: GCObjectHeader,

    /// 'this' argument
    pub this_val: JSValue,

    /// number of function arguments
    pub argc: i32,

    /// used to throw an exception in JS_CallInternal()
    pub throw_flag: bool,

    /* TRUE if the function has returned. The stack frame is no longer valid */
    pub is_completed: bool,

    /// for async function only
    pub resolving_funcs: [JSValue; 2],

    /// a dedicated stack frame to run async function
    pub frame: Box<StackFrame>,
}

impl Drop for AsyncStateInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl AsyncStateInner {
    fn new(
        ctx: &JSContext,
        resolvings: [JSValue; 2],
        bc_func: BCFunction,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let (arg_count, var_count, stack_size, js_mode) = {
            let bc = bc_func.bytecode();
            let b = bc.0.borrow();
            (b.arg_count, b.var_count, b.stack_size, b.js_mode)
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
                lst[i] = a.clone();
            }

            lst
        };

        let s = Self {
            header: GCObjectHeader::new(ctx.rt_mut(), 1),

            argc: args.len() as _,
            this_val: this_obj,
            resolving_funcs: resolvings,
            throw_flag: false,
            is_completed: false,

            frame: Box::new(StackFrame {
                js_mode: js_mode as i32 | JS_MODE_ASYNC,
                cur_func: bc_func.into(),
                arg_base_ptr: local_buf.as_mut_ptr(),
                arg_real_count: args.len() as _,
                arg_count: arg_count as _,
                var_base_ptr: unsafe { local_buf.as_mut_ptr().add(arg_count as usize) },
                var_count,
                local_buf,
                ..Default::default()
            }),
        };
        Ok(s)
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

    pub fn arg_slice(&self) -> &[JSValue] {
        unsafe {
            std::slice::from_raw_parts(self.frame.arg_base_ptr, self.frame.arg_real_count as usize)
        }
    }

    /// free values ranges from argument start through stack top.
    fn free_stack_values(&mut self, rt: &mut JSRuntime) {
        // free values on async stack frame
        if !self.frame.arg_base_ptr.is_null() {
            debug_assert!(
                self.frame.cur_sp_offset != -1,
                "cannot free the function if it is running"
            );

            let mut p = self.frame.arg_base_ptr; // arg base is on local_buf
            let end_p = self.frame.stack_cur_ptr().as_ptr(); // stack cur ptr is on local_buf
            while p < end_p {
                unsafe {
                    std::ptr::replace(p, JSValue::UNDEFINED);
                    p = p.add(1);
                }
            }

            self.frame.arg_base_ptr = std::ptr::null_mut();
        }

        // clean up var refs
        self.frame.clear();
    }

    fn finalize(&mut self) {
        if !self.is_completed {
            // cannot close the closure variables here because it would
            // potentially modify the object graph
            self.free_stack_values(unsafe { self.header.rt_ptr().as_mut() });
        }
    }
}

/// async execution state.
/// used to save execution state of an async function or generator.
#[derive(Debug, Clone)]
pub struct AsyncFuncState(pub(crate) Rc<RefCell<Pin<Box<AsyncStateInner>>>>);

impl AsyncFuncState {
    /// create new AsyncFuncState, register to gc tracking list.
    pub fn new_with_resolvings(
        ctx: &JSContext,
        resolvings: [JSValue; 2],
        bc_func: BCFunction,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<AsyncFuncState, i32> {
        let inner = AsyncStateInner::new(ctx, resolvings, bc_func, this_obj, args)?;

        // fix var_base_ptr field: it's self referenced
        let mut pinned = Box::pin(inner);
        unsafe {
            let a = pinned.as_mut().get_unchecked_mut();
            a.frame.var_base_ptr = a
                .frame
                .local_buf
                .as_mut_ptr()
                .add(a.frame.arg_count as usize);
        }

        let sframe = Self(Rc::new(RefCell::new(pinned)));
        let w = sframe.downgrade();

        // associate stack frame with async state
        unsafe {
            sframe
                .0
                .borrow_mut()
                .as_mut()
                .get_unchecked_mut()
                .frame
                .async_func = sframe.downgrade();
        }

        // register to gc
        ctx.rt_mut().gc_obj_list.insert(sframe.id(), w.into());

        Ok(sframe)
    }

    /// create new AsyncFuncState, register to gc tracking list.
    #[inline(always)]
    pub fn new(
        ctx: &JSContext,
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

    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.0.borrow().header.rt_ptr().as_ref() }
    }

    #[inline(always)]
    pub fn is_completed(&self) -> bool {
        self.0.borrow().is_completed
    }

    pub fn downgrade(&self) -> AsyncStateWeakref {
        AsyncStateWeakref(Rc::downgrade(&self.0))
    }

    pub fn set_throw_flag(&self, b: bool) {
        unsafe {
            self.0.borrow_mut().as_mut().get_unchecked_mut().throw_flag = b;
        }
    }

    /// Resume execution of the async function.
    pub fn resume(&self, ctx: &JSContext) -> JSResult {
        debug_assert!(!self.0.borrow().is_completed);

        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // short borrow to avoid conflict
        let (this_val, arg_list) = {
            let b = self.0.borrow();
            (b.this_val.clone(), unsafe {
                std::slice::from_raw_parts(b.arg_ptr().as_ptr(), b.arg_count())
            })
        };

        let mut ret = JS_CallInternal(
            ctx,
            &JSValue::AsyncStateResume(self.clone()), // <-- use special value type
            &this_val,
            JSValue::UNDEFINED,
            arg_list,
            JS_CALL_FLAG_GENERATOR,
        )
        .err();

        let completed = match ret {
            Ok(JSValue::UNDEFINED) => {
                debug_assert!(self.0.borrow().frame.cur_sp_offset > 0);
                // get stack top value
                ret = Ok(unsafe {
                    std::ptr::replace(
                        // HINT: stack_cur_ptr advances the addr of stack top value by 1.
                        // subtract 1 is the right addr for stack top.
                        self.0.borrow().frame.stack_cur_ptr().offset(-1).as_ptr(),
                        JSValue::UNDEFINED,
                    )
                });
                true
            }
            Err(_) => true,
            _ => false,
        };

        if completed {
            // end of execution
            let mut state = self.0.borrow_mut();
            let state = unsafe { state.as_mut().get_unchecked_mut() };

            state.is_completed = true;
            // close the closure variables.
            state.frame.close_var_refs(ctx);
        }

        ret
    }

    #[inline(always)]
    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().header.gc_has_mark()
    }

    #[inline(always)]
    pub fn gc_set_mark(&self, mark: bool) {
        self.0.borrow_mut().header.gc_set_mark(mark);
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} [async_function]", self.id() as *const Self);
    }

    pub fn class_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {}

    pub fn class_mark(val: JSValue) {
        if let Some(s) = val
            .as_object()
            .and_then(|p| p.payload().async_func_state().cloned())
        {
            s.gc_mark();
        }
    }
}

#[derive(Debug, Clone)]
pub struct AsyncStateWeakref(Weak<RefCell<Pin<Box<AsyncStateInner>>>>);

impl From<AsyncStateWeakref> for GCObjectWeakref {
    fn from(value: AsyncStateWeakref) -> Self {
        Self::AsyncFunction(value)
    }
}

impl AsyncStateWeakref {
    pub fn new() -> Self {
        Self(Weak::new())
    }

    pub fn upgrade(&self) -> Option<AsyncFuncState> {
        self.0.upgrade().map(AsyncFuncState)
    }
}
