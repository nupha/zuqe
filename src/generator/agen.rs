use std::ops::{Deref, DerefMut};

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_PROP_STRING_DEF,
    context::JSContext,
    error::JSResult,
    function::{AsyncObjPayload, CallContext, FieldDef},
    iterator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW, js_create_iterator_result},
    object::{JS_PROP_CONFIGURABLE, JSObjectWeakref, ObjectPayload},
    promise::{Promise, js_promise_resolve},
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
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
pub struct ObjAsyncGenerator {
    /// reference back to the object (const)
    pub generator: JSObjectWeakref,

    pub state: JSAsyncGeneratorStateEnum,

    /// None for state AWAITING_RETURN and COMPLETED
    pub func_state: Option<AsyncObjPayload>,

    /// list of JSAsyncGeneratorRequest
    pub queue: Vec<Box<JSAsyncGeneratorRequest>>,
}

#[derive(Debug)]
pub struct JSAsyncGeneratorRequest {
    /// completion: GEN_MAGIC_x
    pub completion_type: i32,
    pub result: JSValue,

    /// promise capability
    /// TODO: use Promise
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
            self.as_object_unchecked().class_id(),
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
        let s = Box::new(ObjAsyncGenerator {
            generator: JSObjectWeakref::default(),
            state: JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START,
            func_state: Some(AsyncObjPayload::new(ctx, func_obj, this_obj.clone(), args)?),
            queue: Vec::with_capacity(1),
        });

        // execute the function up to 'OP_initial_yield' (no yield nor await are possible)
        s.func_state.as_ref().unwrap().resume(ctx)?;

        // create AsyncGenerator instance
        let ob = ctx
            .create_object_apply_ctor(func_obj, ObjectPayload::ASYNC_GENERATOR(Some(s)))
            .map_err(|_| -1)?;

        // fix generator field.
        if let ObjectPayload::ASYNC_GENERATOR(Some(s)) = ob.0.payload.borrow_mut().deref_mut() {
            s.generator = ob.downgrade();
        } else {
            unreachable!()
        }

        Ok(ob.into())
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {}

    pub fn hook_mark(val: JSValue) {
        let p = unsafe { val.as_object_unchecked() };

        if let ObjectPayload::ASYNC_GENERATOR(Some(gd)) = p.0.payload.borrow().deref() {
            if let Some(st) = &gd.func_state {
                st.gc_mark();
            }

            for req in gd.queue.iter() {
                req.result.gc_mark();
                req.promise.gc_mark();
                req.resolving_funcs[0].gc_mark();
                req.resolving_funcs[1].gc_mark();
            }
        }
    }
}

impl ObjAsyncGenerator {
    pub fn resume_next(&mut self, ctx: &JSContext) {
        enum State {
            Resume,
            Done,
        }

        // Hint: queue's first element will be removed in resolve() or reject().
        while let Some(next) = self.queue.first() {
            let state = match self.state {
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING => {
                    // only happens when restarting execution after await()
                    State::Resume
                }
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN => State::Done,
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START => {
                    if next.completion_type == GEN_MAGIC_NEXT {
                        self.func_state.as_ref().unwrap().set_throw_flag(false);
                        self.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                        State::Resume
                    } else {
                        self.complete();
                        continue;
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
                    State::Done
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
                            s.throw_flag = false;

                            // stack change:
                            // SP[-1] <- value
                            // SP[0] <- next completion type
                            // SP++
                            std::ptr::replace(s.frame.stack_cur_ptr().offset(-1).as_ptr(), value);
                            std::ptr::replace(
                                s.frame.stack_cur_ptr().as_ptr(),
                                JSValue::Int(next.completion_type),
                            );
                            s.frame.cur_sp_offset += 1;
                        }
                        self.func_state.as_ref().unwrap().set_throw_flag(false);
                    }
                    self.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                    State::Resume
                }
            };

            match state {
                State::Done => {
                    return;
                }
                State::Resume => 'resume: loop {
                    let func_ret = self.func_state.as_ref().unwrap().resume(ctx);
                    if self.func_state.as_ref().unwrap().0.borrow().is_completed {
                        // end of function
                        match func_ret {
                            Ok(val) => {
                                self.complete();
                                self.resolve(ctx, val, true);
                            }
                            Err(_) => {
                                let exc = ctx.take_exception();
                                self.complete();
                                self.reject(ctx, exc);
                            }
                        }
                    } else {
                        let code = if let Ok(JSValue::Int(i)) = func_ret {
                            i
                        } else {
                            unreachable!()
                        };

                        // take stack top value.
                        let value = unsafe {
                            std::ptr::replace(
                                self.func_state
                                    .as_ref()
                                    .unwrap()
                                    .0
                                    .borrow_mut()
                                    .frame
                                    // HINT: stack_cur_ptr advances the addr of stack top value by 1.
                                    // subtract 1 to access stack top value addr.
                                    .stack_cur_ptr()
                                    .offset(-1)
                                    .as_ptr(),
                                JSValue::UNDEFINED,
                            )
                        };

                        match code {
                            FUNC_RET_YIELD | FUNC_RET_YIELD_STAR => {
                                self.state = if code == FUNC_RET_YIELD_STAR {
                                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR
                                } else {
                                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                                };
                                self.resolve(ctx, value, false);
                            }
                            FUNC_RET_AWAIT => {
                                if unsafe { js_async_generator_await(ctx, self, value).is_err() } {
                                    // exception: throw it
                                    self.func_state.as_ref().unwrap().set_throw_flag(true);
                                    continue 'resume;
                                } else {
                                    return;
                                }
                            }
                            _ => unreachable!(),
                        }
                    }

                    break 'resume;
                },
            } // state match
        }
    }

    /// async generator is completed
    pub fn complete(&mut self) {
        if self.state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED {
            self.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED;
            self.func_state.take();
        }
    }

    fn resolve_or_reject(&mut self, ctx: &JSContext, result: &JSValue, is_reject: bool) {
        let next = self.queue.remove(0);
        next.resolving_funcs[is_reject as usize]
            .call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(result))
            .unwrap();
    }

    pub fn resolve(&mut self, ctx: &JSContext, value: JSValue, done: bool) {
        let result = js_create_iterator_result(ctx, value, done).unwrap_or(JSValue::EXCEPTION);
        self.resolve_or_reject(ctx, &result, false);
    }

    #[inline(always)]
    pub fn reject(self: &mut ObjAsyncGenerator, ctx: &JSContext, exception: JSValue) {
        self.resolve_or_reject(ctx, &exception, true);
    }
}

pub fn js_async_generator_completed_return(
    ctx: &JSContext,
    s: &mut ObjAsyncGenerator,
    value: JSValue,
) -> Result<(), i32> {
    let promise: Promise = match {
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
    }
    .try_into()?;

    let resolving_funcs1 = unsafe {
        js_async_generator_resolve_function_create(
            ctx,
            JSValue::Object(s.generator.upgrade().unwrap()),
            true,
        )?
    };

    promise.reaction(
        ctx,
        &resolving_funcs1,
        &[JSValue::UNDEFINED, JSValue::UNDEFINED],
    )
}

pub fn js_async_generator_await(
    ctx: &JSContext,
    s: &mut ObjAsyncGenerator,
    value: JSValueConst,
) -> Result<(), i32> {
    let promise: Promise = {
        let cc = CallContext::new(
            ctx,
            ctx.0.borrow().promise_ctor.clone(),
            std::slice::from_ref(&value),
            0,
            None,
        );
        js_promise_resolve(ctx, cc)?.try_into()?
    };

    let resolving_funcs = unsafe {
        js_async_generator_resolve_function_create(
            ctx,
            JSValue::Object(s.generator.upgrade().unwrap()),
            false,
        )?
    };

    promise.reaction(
        ctx,
        &resolving_funcs,
        // no need to create 'thrownawayCapability' as in the spec
        &[JSValue::UNDEFINED, JSValue::UNDEFINED],
    )
}

/// create [resolve, reject] functions for async `generator`.
/// `generator` is not consumed.
pub unsafe fn js_async_generator_resolve_function_create(
    ctx: &JSContext,
    generator: JSValue,
    is_resume_next: bool,
) -> Result<[JSValue; 2], i32> {
    let resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    for i in 0..2 {
        let func = ctx.create_native_function_with_data(
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
    let mut isa = unsafe { val.as_object_unchecked().0.payload.borrow_mut() };

    if let ObjectPayload::ASYNC_GENERATOR(Some(p)) = isa.deref_mut() {
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
                // return value of await.
                unsafe {
                    std::ptr::replace(
                        p.func_state
                            .as_ref()
                            .unwrap()
                            .0
                            .borrow_mut()
                            .frame
                            // HINT: stack_cur_ptr advances the addr of stack top value by 1.
                            // subtract 1 to access stack top value addr.
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

    if let Some(ObjectPayload::ASYNC_GENERATOR(p)) = cc
        .this()
        .as_object()
        .map(|o| o.0.payload.borrow_mut())
        .as_deref_mut()
    {
        let p = p.as_mut().unwrap();

        let req = Box::new(JSAsyncGeneratorRequest {
            completion_type: cc.magic(),
            result: cc.arg(0).clone(),
            promise: promise.clone().into(),
            resolving_funcs,
        });
        p.queue.push(req);

        if p.state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING {
            p.resume_next(ctx);
        }
    } else {
        ctx.throw_type_error("not an AsyncGeneratorValue object");
        let err = ctx.take_exception();
        resolving_funcs[1].call(ctx, &JSValue::UNDEFINED, &[err])?;
    }

    Ok(promise.into())
}

pub fn js_iterator_proto_iterator(ctx: &JSContext, cc: CallContext) -> JSResult {
    Ok(cc.this().clone())
}

pub static js_async_iterator_proto_funcs: [FieldDef; 1] = [JS_CFUNC_NATIVE_DEF!(
    "[Symbol.asyncIterator]",
    0,
    js_iterator_proto_iterator,
    0
)];

pub static js_async_generator_proto_funcs: [FieldDef; 4] = [
    JS_CFUNC_NATIVE_DEF!("next", 1, js_async_generator_next, GEN_MAGIC_NEXT as _),
    JS_CFUNC_NATIVE_DEF!("return", 1, js_async_generator_next, GEN_MAGIC_RETURN as _),
    JS_CFUNC_NATIVE_DEF!("throw", 1, js_async_generator_next, GEN_MAGIC_THROW as _),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "AsyncGeneratorValue",
        JS_PROP_CONFIGURABLE
    ),
];

pub static js_async_generator_function_proto_funcs: [FieldDef; 1] = [JS_PROP_STRING_DEF!(
    "[Symbol.toStringTag]",
    "AsyncGeneratorFunction",
    JS_PROP_CONFIGURABLE
)];
