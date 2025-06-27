use std::ops::DerefMut;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_PROP_STRING_DEF,
    context::JSContext,
    error::JSResult,
    function::{
        CallContext, JS_Call, JSAsyncFunctionState, JSCFunctionListEntry, async_func_resume,
    },
    gc::GCMarkFunc,
    iterator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW, js_create_iterator_result},
    promise::{PromiseValue, js_promise_resolve},
    runtime::JSRuntime,
    utils::UncheckedInto,
    value::{
        JSValue, JSValueConst,
        object::{JS_PROP_CONFIGURABLE, JSObjectIsa, JSObjectWeakref},
    },
};

use super::{FUNC_RET_AWAIT, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum JSAsyncGeneratorStateEnum {
    JS_ASYNC_GENERATOR_STATE_SUSPENDED_START,
    JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD,
    JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR,
    JS_ASYNC_GENERATOR_STATE_EXECUTING,
    JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN,
    JS_ASYNC_GENERATOR_STATE_COMPLETED,
}

#[derive(Debug)]
pub struct JSAsyncGeneratorData {
    /// back pointer to the object (const)
    pub generator: JSObjectWeakref,

    pub state: JSAsyncGeneratorStateEnum,

    /// None for state AWAITING_RETURN and COMPLETED
    pub func_state: Option<JSAsyncFunctionState>,

    /// list of JSAsyncGeneratorRequest
    pub queue: Vec<Box<JSAsyncGeneratorRequest>>,
}

#[derive(Debug)]
pub struct JSAsyncGeneratorRequest {
    /// completion: GEN_MAGIC_x
    pub completion_type: i32,
    pub result: JSValue,

    /// promise capability
    pub promise: JSValue,
    pub resolving_funcs: [JSValue; 2],
}

/// An AsyncGeneratorValue value
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct AsyncGeneratorValue(JSValue);

impl JSValue {
    /// interpret value as AsyncGeneratorValue object.
    /// you must ensure that `value` is an AsyncGeneratorValue object.
    #[inline(always)]
    pub unsafe fn as_async_generator_unchecked(self) -> AsyncGeneratorValue {
        debug_assert!(matches!(self, Self::Object(_)));
        debug_assert!(matches!(
            self.object_unchecked().class_id(),
            JS_CLASS_ASYNC_GENERATOR | JS_CLASS_ASYNC_GENERATOR_FUNCTION
        ));

        std::mem::transmute(self)
    }
}

impl AsyncGeneratorValue {
    pub fn hook_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let s = Box::new(JSAsyncGeneratorData {
            generator: JSObjectWeakref::default(),
            state: JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START,
            func_state: Some(JSAsyncFunctionState::new(ctx, func_obj, this_obj, args)?),
            queue: Vec::with_capacity(1),
        });

        // execute the function up to 'OP_initial_yield' (no yield nor await are possible)
        async_func_resume(ctx, s.func_state.as_ref().unwrap()).check_exception()?;

        // create AsyncGenerator instance
        let obj = ctx
            .create_instance_from_ctor(func_obj, JSObjectIsa::ASYNC_GENERATOR(Some(s)))
            .map_err(|_| -1)?;

        let ob = unsafe { obj.object_unchecked() };
        // fix generator field.
        if let JSObjectIsa::ASYNC_GENERATOR(Some(s)) = ob.0.isa.borrow_mut().deref_mut() {
            s.generator = ob.downgrade();
        } else {
            unreachable!()
        }

        Ok(obj)
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        println!("[O.o] TODO asycn generator value mark");

        // if let Some(JSObjectIsa::ASYNC_GENERATOR(p)) = val.object().map(|o| o.isa()) {
        //     list_foreach(&mut p.as_mut().unwrap().queue, |el| {
        //         let req = list_entry!(el, JSAsyncGeneratorRequest, link);

        //         (*rt).gc_mark_value((*req).result, mark_func);
        //         (*rt).gc_mark_value((*req).promise, mark_func);
        //         (*rt).gc_mark_value((*req).resolving_funcs[0], mark_func);
        //         (*rt).gc_mark_value((*req).resolving_funcs[1], mark_func);
        //     });

        //     if !p.as_ref().unwrap().func_state.is_null() {
        //         mark_func(rt, p.as_ref().unwrap().func_state);
        //     }
        // }
    }
}

impl JSAsyncGeneratorData {
    pub fn resume_next(&mut self, ctx: &JSContext) {
        enum State {
            ResumeExec,
            Done,
        }

        loop {
            let state: State = loop {
                if let Some(next) = self.queue.first() {
                    match self.state {
                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING => {
                        // only happens when restarting execution after await()
                        break State::ResumeExec;
                    }
                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN => {
                        break State::Done;
                    }
                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START => {
                        if next.completion_type == GEN_MAGIC_NEXT {
                            self.func_state.as_ref().unwrap().set_throw_flag(false);
                            self.state =
                                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                            break State::ResumeExec;
                        } else {
                            self.complete( );
                        }
                    }
                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED => {
                        if next.completion_type == GEN_MAGIC_NEXT {
                            self.resolve(ctx, JSValue::UNDEFINED, true);
                        } else if next.completion_type == GEN_MAGIC_RETURN {
                            self.state =
                                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN;
                            unsafe {
                                js_async_generator_completed_return(ctx, self, next.result.clone())
                                    .unwrap();
                            }
                        } else {
                            self.reject(ctx, next.result.clone());
                        }
                        break State::Done;
                    }
                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                    | JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR => {
                        let value = next.result.clone();
                        if self.state
                            == JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                            && next.completion_type == GEN_MAGIC_THROW
                        {
                            ctx.throw(value);
                            self.func_state.as_ref().unwrap().set_throw_flag(true);
                        } else {
                            // 'yield' returns a value. 'yield *' also returns a value
                            // in case the 'throw' method is called
                            unsafe {
                                let mut s = self.func_state.as_ref().unwrap().0.borrow_mut();
                                let s = s.as_mut().get_unchecked_mut();

                                std::ptr::replace(
                                    s.frame.stack_cur_ptr().offset(-1).as_ptr(),
                                    value,
                                );
                                std::ptr::replace(
                                    s.frame.stack_cur_ptr().as_ptr(),
                                    JSValue::from (next.completion_type),
                                );

                                s.frame.cur_sp_offset += 1;
                                s.throw_flag = false;
                            }
                        }
                        self.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                        break State::ResumeExec;
                    }
                }
                } else {
                    break State::Done;
                }
            }; // inner loop

            match state {
                State::Done => {
                    return;
                }
                State::ResumeExec => loop {
                    unsafe {
                        let func_ret = async_func_resume(ctx, self.func_state.as_ref().unwrap());
                        if self.func_state.as_ref().unwrap().0.borrow().is_completed {
                            if func_ret.is_exception() {
                                let value = ctx.take_exception();
                                self.complete();
                                self.reject(ctx, value);
                            } else {
                                // end of function
                                self.complete();
                                self.resolve(ctx, func_ret, true);
                            }
                        } else {
                            let func_ret_code = if let JSValue::Int(i) = func_ret {
                                i
                            } else {
                                unreachable!()
                            };

                            let value = self
                                .func_state
                                .as_ref()
                                .unwrap()
                                .0
                                .borrow_mut()
                                .frame
                                .stack_cur_ptr()
                                .offset(-1)
                                .as_mut()
                                .replace(JSValue::UNDEFINED);

                            match func_ret_code {
                                FUNC_RET_YIELD | FUNC_RET_YIELD_STAR => {
                                    self.state = if func_ret_code == FUNC_RET_YIELD_STAR {
                                        JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR
                                    } else {
                                        JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                                    };
                                    self.resolve(ctx, value, false);
                                }
                                FUNC_RET_AWAIT => {
                                    if js_async_generator_await(ctx, self, value).is_err() {
                                        // exception: throw it
                                        self.func_state.as_ref().unwrap().set_throw_flag(true);
                                        continue; // State::ResumeExec loop
                                    } else {
                                        return;
                                    }
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                },
            } // state match
        } // outer loop
    }

    /// set async generator data `self` as completed
    pub fn complete(&mut self) {
        if self.state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED {
            self.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED;
            self.func_state.take();
        }
    }

    fn resolve_or_reject(&mut self, ctx: &JSContext, result: &JSValue, is_reject: bool) {
        let next = self.queue.remove(0);
        JS_Call(
            ctx,
            &next.resolving_funcs[is_reject as usize],
            &JSValue::UNDEFINED,
            std::slice::from_ref(result),
        );
    }

    #[inline(always)]
    pub fn resolve(&mut self, ctx: &JSContext, value: JSValue, done: bool) {
        let result = js_create_iterator_result(ctx, value, done).unwrap_or(JSValue::EXCEPTION);
        self.resolve_or_reject(ctx, &result, false);
    }

    #[inline(always)]
    pub fn reject(self: &mut JSAsyncGeneratorData, ctx: &JSContext, exception: JSValue) {
        self.resolve_or_reject(ctx, &exception, true);
    }
}

#[deprecated(note = "use JSAsyncGeneratorData::resume_next() instead")]
pub unsafe fn js_async_generator_resume_next(ctx: &JSContext, s: *mut JSAsyncGeneratorData) {
    (*s).resume_next(ctx);
}

#[deprecated(note = "use JSAsyncGeneratorData::complete() instead")]
pub unsafe fn js_async_generator_complete(ctx: &JSContext, s: *mut JSAsyncGeneratorData) {
    (*s).complete();
}

/// `value` is not consumed.
#[deprecated(note = "use JSAsyncGeneratorData::resolve() instead")]
pub unsafe fn js_async_generator_resolve(
    ctx: &JSContext,
    s: *mut JSAsyncGeneratorData,
    value: JSValueConst,
    done: bool,
) {
    (*s).resolve(ctx, value, done);
}

#[inline(always)]
#[deprecated(note = "use JSAsyncGeneratorData::reject() instead")]
pub unsafe fn js_async_generator_reject(
    ctx: &JSContext,
    s: *mut JSAsyncGeneratorData,
    exception: JSValueConst,
) {
    (*s).reject(ctx, exception);
}

#[deprecated(note = "use JSAsyncGeneratorData::resolve_or_reject() instead")]
pub unsafe fn js_async_generator_resolve_or_reject(
    ctx: &JSContext,
    s: *mut JSAsyncGeneratorData,
    mut result: JSValueConst,
    is_reject: bool,
) {
    (*s).resolve_or_reject(ctx, &result, is_reject);
}

pub unsafe fn js_async_generator_completed_return(
    ctx: &JSContext,
    s: &mut JSAsyncGeneratorData,
    value: JSValue,
) -> Result<(), i32> {
    let promise: JSValue = match {
        let cc = CallContext::new(
            ctx,
            ctx.0.borrow().promise_ctor.clone(),
            std::slice::from_ref(&value),
            0, // is_reject=0
            None,
        );
        js_promise_resolve(ctx, cc)
    } {
        Ok(r) => r,
        Err(_) => {
            // Fail looking up JS_ATOM_constructor when is_reject==0.
            // A poisoned .constructor property is observable and the resulting
            // exception should be delivered to the catch handler.
            let err = ctx.take_exception();
            let cc = CallContext::new(
                ctx,
                ctx.0.borrow().promise_ctor.clone(),
                std::slice::from_ref(&err),
                1, // is_reject=1
                None,
            );
            js_promise_resolve(ctx, cc)?
        }
    };

    let resolving_funcs1 = js_async_generator_resolve_function_create(
        ctx,
        JSValue::Object(s.generator.upgrade().unwrap()),
        true,
    )?;

    let res = UncheckedInto::<PromiseValue>::unchecked_into(promise).reaction(
        ctx,
        &resolving_funcs1,
        &[JSValue::UNDEFINED, JSValue::UNDEFINED],
    );

    res
}

pub unsafe fn js_async_generator_await(
    ctx: &JSContext,
    s: &mut JSAsyncGeneratorData,
    value: JSValueConst,
) -> Result<(), i32> {
    let promise = {
        let cc = CallContext::new(
            ctx,
            ctx.0.borrow().promise_ctor.clone(),
            std::slice::from_ref(&value),
            0,
            None,
        );
        js_promise_resolve(ctx, cc)?
    };

    let resolving_funcs = js_async_generator_resolve_function_create(
        ctx,
        JSValue::Object(s.generator.upgrade().unwrap()),
        false,
    )?;

    let res = UncheckedInto::<PromiseValue>::unchecked_into(promise).reaction(
        ctx,
        &resolving_funcs,
        // no need to create 'thrownawayCapability' as in the spec
        &[JSValue::UNDEFINED, JSValue::UNDEFINED],
    );

    res
}

/// create [resolve, reject] functions for async `generator`.
/// `generator` is not consumed.
pub unsafe fn js_async_generator_resolve_function_create(
    ctx: &JSContext,
    generator: JSValue,
    is_resume_next: bool,
) -> Result<[JSValue; 2], i32> {
    let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    for i in 0..2 {
        let func = ctx.create_native_callable_object(
            js_async_generator_resolve_function,
            1,
            i + if is_resume_next { 2 } else { 0 },
            std::slice::from_ref(&generator),
        )?;
    }

    Ok(resolving_funcs)
}

pub fn js_async_generator_resolve_function(ctx: &JSContext, args: CallContext) -> JSResult {
    let is_reject = args.magic() & 1 != 0;
    let val = args.arg(0);
    let mut isa = unsafe { val.object_unchecked().0.isa.borrow_mut() };

    if let JSObjectIsa::ASYNC_GENERATOR(Some(p)) = isa.deref_mut() {
        if args.magic() >= 2 {
            // resume next case in AWAITING_RETURN state
            debug_assert!(matches!(
                p.state,
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN
                    | JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED
            ));

            p.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED;
            if is_reject {
                p.reject(ctx, val.clone());
            } else {
                p.resolve(ctx, val.clone(), true);
            }
        } else {
            // restart function execution after await()
            debug_assert_eq!(
                p.state,
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING
            );

            p.func_state.as_ref().unwrap().set_throw_flag(is_reject);

            if is_reject {
                ctx.throw(val.clone());
            } else {
                // return value of await
                unsafe {
                    std::ptr::replace(
                        p.func_state
                            .as_ref()
                            .unwrap()
                            .0
                            .borrow_mut()
                            .frame
                            .stack_cur_ptr()
                            .offset(-1)
                            .as_ptr(),
                        val.clone(),
                    );
                }
            }

            p.resume_next(ctx);
        }

        Ok(JSValue::UNDEFINED)
    } else {
        unreachable!()
    }
}

pub fn js_async_generator_next(ctx: &JSContext, cc: CallContext) -> JSResult {
    let (promise, resolving_funcs) = ctx.create_promise(&JSValue::UNDEFINED)?;

    if let Some(JSObjectIsa::ASYNC_GENERATOR(p)) = cc
        .this()
        .object()
        .map(|o| o.0.isa.borrow_mut())
        .as_deref_mut()
    {
        let p = p.as_mut().unwrap();

        let req = Box::new(JSAsyncGeneratorRequest {
            completion_type: cc.magic(),
            result: cc.arg(0).clone(),
            promise: promise.clone().as_value(),
            resolving_funcs,
        });
        p.queue.push(req);

        if p.state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING {
            p.resume_next(ctx);
        }
    } else {
        ctx.JS_ThrowTypeError("not an AsyncGeneratorValue object");
        let err = ctx.take_exception();
        JS_Call(ctx, &resolving_funcs[1], &JSValue::UNDEFINED, &[err]);
    }

    Ok(promise.into())
}

pub fn js_iterator_proto_iterator(ctx: &JSContext, cc: CallContext) -> JSResult {
    Ok(cc.this().clone())
}

pub const js_async_iterator_proto_funcs: [JSCFunctionListEntry; 1] = [JS_CFUNC_NATIVE_DEF!(
    c"[Symbol.asyncIterator]",
    0,
    js_iterator_proto_iterator,
    0
)];

pub const js_async_generator_proto_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_NATIVE_DEF!(c"next", 1, js_async_generator_next, GEN_MAGIC_NEXT as _),
    JS_CFUNC_NATIVE_DEF!(c"return", 1, js_async_generator_next, GEN_MAGIC_RETURN as _),
    JS_CFUNC_NATIVE_DEF!(c"throw", 1, js_async_generator_next, GEN_MAGIC_THROW as _),
    JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"AsyncGeneratorValue",
        JS_PROP_CONFIGURABLE
    ),
];

pub const js_async_generator_function_proto_funcs: [JSCFunctionListEntry; 1] =
    [JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"AsyncGeneratorFunction",
        JS_PROP_CONFIGURABLE
    )];
