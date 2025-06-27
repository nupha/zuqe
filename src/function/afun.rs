use std::{
    cell::RefCell,
    pin::Pin,
    rc::{Rc, Weak},
};

use crate::{
    JS_CLASS_ASYNC_FUNCTION_REJECT, JS_PROP_STRING_DEF,
    context::{JS_MODE_ASYNC, JSContext},
    error::{JS_ThrowStackOverflow, JSResult},
    gc::{GCMarkFunc, GCObjectHeader},
    promise::{PromiseValue, js_promise_resolve},
    runtime::{JSRuntime, js_check_stack_overflow},
    stack_frame::JSStackFrame,
    utils::UncheckedInto,
    value::{
        JS_FreeValue, JSValue, JSValueConst,
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
        let isa = unsafe { func_obj.object_unchecked().0.isa.borrow() };
        let bc = isa.func().unwrap().function_bytecode.clone().unwrap();
        let b = bc.0.borrow();

        // alloc local buf, set up arguments, variables and stack pointer
        let arg_count = std::cmp::max(b.arg_count as usize, args.len());
        let buf_len = arg_count + b.var_count as usize + b.stack_size as usize;
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
                js_mode: b.js_mode as i32 | JS_MODE_ASYNC,
                cur_func: func_obj.clone(),
                arg_base_ptr: local_buf.as_mut_ptr(),
                arg_count: arg_count as _,
                var_base_ptr: unsafe { local_buf.as_mut_ptr().add(arg_count as usize) },
                var_count: b.var_count,
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

    pub fn arg_slice(&self) -> &[JSValue] {
        unsafe { std::slice::from_raw_parts(self.frame.arg_base_ptr, self.argc as usize) }
    }

    fn free_stack(&mut self, rt: &mut JSRuntime) {
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
            self.free_stack(unsafe { self.header.rt_ptr().as_mut() });
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

    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.0.borrow().header.rt_ptr().as_ref() }
    }

    pub fn downgrade(&self) -> JSAsyncFunctionStateWeakref {
        JSAsyncFunctionStateWeakref(Rc::downgrade(&self.0))
    }

    pub fn set_throw_flag(&self, b: bool) {
        unsafe {
            self.0.borrow_mut().as_mut().get_unchecked_mut().throw_flag = b;
        }
    }

    fn strong_count(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().header.gc_has_mark()
    }

    pub fn gc_set_mark(&self, mark: bool) {
        self.0.borrow_mut().header.gc_set_mark(mark);
    }

    // fn gc_status(&self) -> GCObjectStatus {
    //     self.0.borrow().header.gc_status()
    // }

    // fn gc_set_status(&self, s: GCObjectStatus) {
    //      self.0.borrow_mut().header.set_gc_mark(s);
    // }

    // fn gc_mark_children(&self, rt: &mut JSRuntime, mark_func: GCMarkFunc) {
    //     self.0.borrow_mut().gc_mark_children(rt, mark_func);
    // }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} {:4} [async_function]", self, self.strong_count());
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        //if let Some(s) = val.object().and_then(|p| p.isa().async_func_state()) {
        // mark_func(rt, s);
        //}
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

pub fn async_func_resume(ctx: &JSContext, s: &JSAsyncFunctionState) -> JSValue {
    debug_assert!(!s.0.borrow().is_completed);

    let mut ret = if js_check_stack_overflow(ctx.rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        JSValue::EXCEPTION
    } else {
        JS_CallInternal(
            ctx,
            &JSValue::AsyncFuncResume(s.clone()), // <-- use special value type `::AsyncFuncResume`
            &s.0.borrow().this_val,
            JSValue::UNDEFINED,
            s.0.borrow().arg_slice(),
            JS_CALL_FLAG_GENERATOR,
        )
    };

    if ret.is_exception() || ret.is_undefined() {
        if ret.is_undefined() {
            debug_assert!(s.0.borrow().frame.cur_sp_offset > 0);
            let mut stack_ptr = s.0.borrow().frame.stack_cur_ptr();
            ret = unsafe { stack_ptr.as_mut().replace(JSValue::UNDEFINED) };
        }

        // end of execution
        {
            let mut state = s.0.borrow_mut();
            let state = unsafe { state.as_mut().get_unchecked_mut() };

            state.is_completed = true;
            // close the closure variables.
            state.frame.close_var_refs(ctx);
            // state.free_frame(ctx.rt_mut());
        }
    }

    ret
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
            .map_err(|_| {
                JS_FreeValue(ctx, &resolving_funcs[0]);
                -1
            })?;

        resolving_funcs[i] = func_obj;
    }

    return Ok(resolving_funcs);
}

#[deprecated(note = "use JSAsyncFunctionState::free_frame() instead")]
pub unsafe fn async_func_free_frame(rt: *mut JSRuntime, s: *mut JSAsyncFunctionState) {
    unreachable!("unused");

    // let sf = &mut (*s).frame;

    // if !sf.arg_base_ptr.is_null() {
    //     // cannot free the function if it is running
    //     let mut p = sf.arg_base_ptr;
    //     let cur_sp = sf.stack_cur_ptr().as_ptr();
    //     while p < cur_sp {
    //         JS_FreeValueRT(rt, *p);
    //         p = p.add(1);
    //     }

    //     sf.arg_base_ptr = std::ptr::null_mut();
    // }

    // JS_FreeValueRT(rt, sf.cur_func);
    // JS_FreeValueRT(rt, (*s).this_val);
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
    let func_ret = async_func_resume(ctx, s);

    if s.0.borrow().is_completed {
        let ret2: JSValue = if func_ret.is_exception() {
            let error = ctx.take_exception();
            JS_Call(
                ctx,
                &s.0.borrow().resolving_funcs[1], // reject
                &JSValue::UNDEFINED,
                &[error],
            )
        } else {
            // normal return
            JS_Call(
                ctx,
                &s.0.borrow().resolving_funcs[0], // resolve
                &JSValue::UNDEFINED,
                &[func_ret],
            )
        };
    } else {
        let cur_sp = s.0.borrow().frame.stack_cur_ptr();
        let value = unsafe { std::ptr::replace(cur_sp.offset(-1).as_ptr(), JSValue::UNDEFINED) };

        let promise = {
            let cc = CallContext::new(
                ctx,
                ctx.0.borrow().promise_ctor.clone(),
                std::slice::from_ref(&value),
                0,
                None,
            );
            js_promise_resolve(ctx, cc).unwrap_or(JSValue::EXCEPTION)
        };
        if promise.is_exception() {
            let mut error = ctx.take_exception();
            let ret2 = JS_Call(
                ctx,
                &s.0.borrow().resolving_funcs[1],
                &JSValue::UNDEFINED,
                &[error],
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
    c"[Symbol.toStringTag]",
    c"AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
