use std::{
    cell::RefCell,
    pin::Pin,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    context::{JS_MODE_ASYNC, JSContext},
    error::{JS_ThrowStackOverflow, JSResult},
    gc::GCObjectHeader,
    object::{GCObjectWeakref, JSObjectIsa},
    runtime::{JSRuntime, js_check_stack_overflow},
    stack_frame::JSStackFrame,
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

    /// stack frame to run async function
    pub frame: Box<JSStackFrame>,
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
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let (arg_count, var_count, stack_size, js_mode) = {
            let isa = unsafe { func_obj.object_unchecked().0.isa.borrow() };
            let bc = isa.func().unwrap().function_bytecode.clone().unwrap();
            let b = bc.0.borrow();

            (b.arg_count, b.var_count, b.stack_size, b.js_mode)
        };

        // alloc local buf, set up arguments, variables and stack pointer
        let arg_count = std::cmp::max(arg_count as usize, args.len());
        let buf_len = arg_count + var_count as usize + stack_size as usize;
        let mut local_buf = {
            let mut lst = Vec::<JSValue>::new();
            lst.try_reserve_exact(buf_len).map_err(|_| {
                ctx.JS_ThrowOutOfMemory();
                -1
            })?;
            lst
        };
        for a in args {
            local_buf.push(a.clone());
        }
        if buf_len > args.len() {
            local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                buf_len - args.len() as usize,
            ));
        }
        debug_assert_eq!(local_buf.len(), buf_len);

        let o = Self {
            header: GCObjectHeader::new(ctx.rt_mut(), 1),
            argc: args.len() as _,
            this_val: this_obj.clone(),
            resolving_funcs: resolvings,
            throw_flag: false,
            is_completed: false,

            frame: Box::new(JSStackFrame {
                js_mode: js_mode as i32 | JS_MODE_ASYNC,
                cur_func: func_obj.clone(),
                arg_base_ptr: local_buf.as_mut_ptr(),
                arg_count: arg_count as _,
                var_base_ptr: unsafe { local_buf.as_mut_ptr().add(arg_count as usize) },
                var_count,
                local_buf,
                ..Default::default()
            }),
        };
        Ok(o)
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
        self.frame.arg_count as usize
    }

    pub fn arg_slice(&self) -> &[JSValue] {
        unsafe {
            std::slice::from_raw_parts(self.frame.arg_base_ptr, self.frame.arg_count as usize)
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
        self.frame.finalize();
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
pub struct AsyncState(pub(crate) Rc<RefCell<Pin<Box<AsyncStateInner>>>>);

impl AsyncState {
    /// create new AsyncState, register to gc tracking list.
    pub fn new_with_resolvings(
        ctx: &JSContext,
        resolvings: Option<[JSValue; 2]>,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
    ) -> Result<AsyncState, i32> {
        let inner = AsyncStateInner::new(
            ctx,
            resolvings.unwrap_or([JSValue::UNDEFINED, JSValue::UNDEFINED]),
            func_obj,
            this_obj,
            args,
        )?;

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

    /// create new AsyncState, register to gc tracking list.
    #[inline(always)]
    pub fn new(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
    ) -> Result<AsyncState, i32> {
        Self::new_with_resolvings(ctx, None, func_obj, this_obj, args)
    }

    #[inline]
    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    #[inline]
    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.0.borrow().header.rt_ptr().as_ref() }
    }

    #[inline]
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

    /// Resume execution of the async state.
    pub fn resume(&self, ctx: &JSContext) -> JSResult {
        debug_assert!(!self.0.borrow().is_completed);

        let mut ret = if js_check_stack_overflow(ctx.rt(), 0) {
            JS_ThrowStackOverflow(ctx);
            Err(-1)
        } else {
            // short borrow to avoid conflict
            let (this_val, arg_list) = {
                let b = self.0.borrow();
                (b.this_val.clone(), unsafe {
                    std::slice::from_raw_parts(b.arg_ptr().as_ptr(), b.arg_count())
                })
            };
            JS_CallInternal(
                ctx,
                &JSValue::AsyncStateResume(self.clone()), // <-- use special value type
                &this_val,
                JSValue::UNDEFINED,
                arg_list,
                JS_CALL_FLAG_GENERATOR,
            )
            .check_exception()
        };

        let completed = match ret {
            Ok(JSValue::UNDEFINED) => {
                debug_assert!(self.0.borrow().frame.cur_sp_offset > 0);
                let mut stack_ptr = self.0.borrow().frame.stack_cur_ptr();
                ret = Ok(unsafe { stack_ptr.as_mut().replace(JSValue::UNDEFINED) });
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

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().header.gc_has_mark()
    }

    pub fn gc_set_mark(&self, mark: bool) {
        self.0.borrow_mut().header.gc_set_mark(mark);
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} [async_function]", self.id() as *const Self);
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

    pub fn hook_mark(val: JSValue) {
        if let Some(s) = val
            .object()
            .and_then(|p| p.0.isa.borrow().async_func_state().cloned())
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

    pub fn upgrade(&self) -> Option<AsyncState> {
        self.0.upgrade().map(AsyncState)
    }
}
