use std::{
    cell::RefCell,
    pin::Pin,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    JS_CLASS_ASYNC_FUNCTION_REJECT, JS_PROP_STRING_DEF,
    context::{JS_MODE_ASYNC, JSContext},
    error::{JS_ThrowStackOverflow, JSResult},
    gc::GCObjectHeader,
    promise::{PromiseValue, js_promise_resolve},
    runtime::{JSRuntime, js_check_stack_overflow},
    stack_frame::JSStackFrame,
    utils::UncheckedInto,
    value::{
        JSValue, JSValueConst,
        object::{GCObjectWeakref, JS_PROP_CONFIGURABLE, JSObjectIsa},
    },
};

use super::{
    CallContext, JS_Call, JSCFunctionListEntry,
    call::{JS_CALL_FLAG_GENERATOR, JS_CallInternal},
};

#[derive(Debug)]
#[repr(C)]
pub(crate) struct JSAsyncFunctionStateInner {
    pub header: GCObjectHeader,

    /// 'this' argument
    pub this_val: JSValue,

    /// number of function arguments
    pub argc: i32,

    /// used to throw an exception in JS_CallInternal()
    pub throw_flag: bool,

    /* TRUE if the function has returned. The stack frame is no longer valid */
    pub is_completed: bool,

    /// only used in JS async functions
    pub resolving_funcs: [JSValue; 2],

    /// stack frame to run async function
    pub frame: Box<JSStackFrame>,
}

impl Drop for JSAsyncFunctionStateInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl JSAsyncFunctionStateInner {
    /// create new JSAsyncFunctionState.
    /// `func_obj`, `this_obj`, `args` are not consumed.
    pub fn new(
        ctx: &JSContext,
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
            resolving_funcs: [JSValue::UNDEFINED, JSValue::UNDEFINED],
            throw_flag: false,
            is_completed: false,

            frame: Box::new(JSStackFrame {
                js_mode: js_mode as i32 | JS_MODE_ASYNC,
                cur_func: func_obj.clone(),
                arg_base_ptr: local_buf.as_mut_ptr(),
                arg_count: arg_count as _,
                var_base_ptr: unsafe { local_buf.as_mut_ptr().add(arg_count as usize) },
                var_count: var_count,
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

#[derive(Debug, Clone)]
pub struct JSAsyncFunctionState(pub(crate) Rc<RefCell<Pin<Box<JSAsyncFunctionStateInner>>>>);

impl JSAsyncFunctionState {
    /// make new JSAsyncFunctionState, register to gc tracking list.
    /// `func_obj`, `this_obj`, `args` are not consumed.
    pub fn new(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
    ) -> Result<JSAsyncFunctionState, i32> {
        let inner = JSAsyncFunctionStateInner::new(ctx, func_obj, this_obj, args)?;

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

        let s = Self(Rc::new(RefCell::new(pinned)));
        let w = s.downgrade();

        // associate stack frame with async state
        unsafe {
            s.0.borrow_mut()
                .as_mut()
                .get_unchecked_mut()
                .frame
                .async_func = s.downgrade();
        }

        // register to gc
        ctx.rt_mut().gc_obj_list.insert(s.id(), w.into());

        Ok(s)
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

    pub fn downgrade(&self) -> JSAsyncFunctionStateWeakref {
        JSAsyncFunctionStateWeakref(Rc::downgrade(&self.0))
    }

    pub fn set_throw_flag(&self, b: bool) {
        unsafe {
            self.0.borrow_mut().as_mut().get_unchecked_mut().throw_flag = b;
        }
    }

    /// resume async function execution
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
                &JSValue::AsyncFuncResume(self.clone()), // <-- use special value type `::AsyncFuncResume`
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
pub struct JSAsyncFunctionStateWeakref(Weak<RefCell<Pin<Box<JSAsyncFunctionStateInner>>>>);

impl From<JSAsyncFunctionStateWeakref> for GCObjectWeakref {
    fn from(value: JSAsyncFunctionStateWeakref) -> Self {
        Self::AsyncFunction(value)
    }
}

impl JSAsyncFunctionStateWeakref {
    pub fn new() -> Self {
        Self(Weak::new())
    }

    pub fn upgrade(&self) -> Option<JSAsyncFunctionState> {
        self.0.upgrade().map(JSAsyncFunctionState)
    }
}

#[deprecated(note = "use JSAsyncFunctionState::resume() instead")]
pub fn async_func_resume(ctx: &JSContext, af: &JSAsyncFunctionState) -> JSValue {
    af.resume(ctx).unwrap_or(JSValue::EXCEPTION)
}

pub unsafe fn js_async_function_resolve_create(
    ctx: &JSContext,
    s: &JSAsyncFunctionState,
) -> Result<[JSValue; 2], i32> {
    let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];

    for i in 0..2 {
        let func_obj = ctx
            .create_instance_from_proto(&ctx.0.borrow().function_proto, {
                if i == 0 {
                    JSObjectIsa::ASYNC_FUNCTION_RESOLVE(s.clone())
                } else {
                    JSObjectIsa::ASYNC_FUNCTION_REJECT(s.clone())
                }
            })
            .map_err(|_| -1)?;

        resolving_funcs[i] = func_obj;
    }

    return Ok(resolving_funcs);
}

#[deprecated(note = "use JSAsyncFunctionState::free_frame() instead")]
pub unsafe fn async_func_free_frame(rt: *mut JSRuntime, s: *mut JSAsyncFunctionState) {
    unreachable!("unused");
}

pub fn js_async_function_resolve_call(
    ctx: &JSContext,
    func_obj: &JSValueConst,
    this_obj: &JSValueConst,
    args: &[JSValue],
    flags: i32,
) -> JSResult {
    let p = unsafe { func_obj.object_unchecked() };
    let s = p.0.isa.borrow().async_func_state().cloned().unwrap();
    let is_reject = p.class_id() == JS_CLASS_ASYNC_FUNCTION_REJECT;

    let arg = if args.len() > 0 {
        args[0].clone()
    } else {
        JSValue::UNDEFINED
    };

    unsafe {
        s.0.borrow_mut().as_mut().get_unchecked_mut().throw_flag = is_reject;
    }

    if is_reject {
        ctx.throw(arg);
    } else {
        // return value of await
        debug_assert!(s.0.borrow().frame.cur_sp_offset > 0);

        let mut s = s.0.borrow_mut();
        let s = unsafe { s.as_mut().get_unchecked_mut() };
        unsafe {
            *s.frame
                .stack_base_ptr()
                .as_ptr()
                .add(s.frame.cur_sp_offset as usize - 1) = arg;
        }
    }

    js_async_function_resume(ctx, &s);
    Ok(JSValue::UNDEFINED)
}

/// call an async function
pub fn js_async_function_call(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_obj: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult {
    let s = JSAsyncFunctionState::new(ctx, func_obj, this_obj, args)?;

    let (promise, resolving_funcs) = ctx.create_promise(&JSValue::UNDEFINED)?;
    unsafe {
        s.0.borrow_mut()
            .as_mut()
            .get_unchecked_mut()
            .resolving_funcs = resolving_funcs;
    }

    js_async_function_resume(ctx, &s);

    Ok(promise.into())
}

pub fn js_async_function_resume(ctx: &JSContext, s: &JSAsyncFunctionState) {
    let func_ret = s.resume(ctx);

    if s.is_completed() {
        match func_ret {
            Ok(val) => {
                // normal return
                let func = s.0.borrow().resolving_funcs[0].clone(); // resolve: short borrow
                JS_Call(ctx, &func, &JSValue::UNDEFINED, std::slice::from_ref(&val));
            }
            Err(_) => {
                let func = s.0.borrow().resolving_funcs[1].clone(); // reject: short borrow
                let error = ctx.take_exception();
                JS_Call(ctx, &func, &JSValue::UNDEFINED, &[error]);
            }
        }
    } else {
        let cur_sp = s.0.borrow().frame.stack_cur_ptr();
        let value = unsafe { std::ptr::replace(cur_sp.offset(-1).as_ptr(), JSValue::UNDEFINED) };
        let promise_ctor = ctx.0.borrow().promise_ctor.clone();

        let promise = {
            let cc = CallContext::new(ctx, promise_ctor, std::slice::from_ref(&value), 0, None);
            js_promise_resolve(ctx, cc).unwrap_or(JSValue::EXCEPTION)
        };
        if promise.is_exception() {
            let error = ctx.take_exception();
            JS_Call(
                ctx,
                &s.0.borrow().resolving_funcs[1],
                &JSValue::UNDEFINED,
                std::slice::from_ref(&error),
            );
        }

        let resolving_funcs = if let Ok(rr) = unsafe { js_async_function_resolve_create(ctx, s) } {
            rr
        } else {
            let error = ctx.take_exception();
            JS_Call(
                ctx,
                &s.0.borrow().resolving_funcs[1],
                &JSValue::UNDEFINED,
                &[error],
            );
            [const { JSValue::UNDEFINED }; 2]
        };

        let res = unsafe {
            UncheckedInto::<PromiseValue>::unchecked_into(promise).reaction(
                ctx,
                &resolving_funcs,
                // no need to create 'thrownawayCapability' as in the spec
                &[const { JSValue::UNDEFINED }; 2],
            )
        };

        if res.is_err() {
            let error = ctx.take_exception();
            JS_Call(
                ctx,
                &s.0.borrow().resolving_funcs[1],
                &JSValue::UNDEFINED,
                &[error],
            );
        }
    }
}

pub const js_async_function_proto_funcs: [JSCFunctionListEntry; 1] = [JS_PROP_STRING_DEF!(
    "[Symbol.toStringTag]",
    "AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
