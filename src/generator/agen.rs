use std::cell::{Cell, RefCell};

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_PROP_STRING_DEF,
    branch_hints::unlikely,
    callable::NativeFunctionWithData,
    context::JSContext,
    error::JSResult,
    function::{AsyncFuncState, CallContext, FieldDef},
    iterator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW, js_create_iterator_result},
    object::{JS_PROP_CONFIGURABLE, JSObject, JSObjectWeakref, ObjectPayload},
    object_cast,
    promise::{Promise, js_promise_resolve},
    runtime::JSRuntime,
    value::JSValue,
};

use super::{FUNC_RET_AWAIT, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum AGenState {
    JS_ASYNC_GENERATOR_STATE_SUSPENDED_START,
    JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD,
    JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR,
    JS_ASYNC_GENERATOR_STATE_EXECUTING,
    JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN,
    JS_ASYNC_GENERATOR_STATE_COMPLETED,
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

#[derive(Debug)]
pub struct AGenPayload {
    /// reference back to the generator object (const)
    generator: RefCell<JSObjectWeakref>,

    state: Cell<AGenState>,

    /// None for state AWAITING_RETURN and COMPLETED
    func_state: AsyncFuncState,

    /// list of JSAsyncGeneratorRequest.
    /// the queue's element is consumed in resolve() or reject() call.
    queue: RefCell<Vec<Box<JSAsyncGeneratorRequest>>>,
}

impl AGenPayload {
    #[inline]
    fn set_throw_flag(&self, is_reject: bool) {
        self.func_state.set_throw_flag(is_reject);
    }

    pub(crate) fn resume_next(&self, ctx: &JSContext) {
        enum State {
            Resume,
            Done,
        }

        while let Some((completion_type, result)) = {
            // Note: borrow() is enclosed in this code block
            self.queue
                .borrow()
                .first()
                .map(|x| (x.completion_type, x.result.clone()))
        } {
            let state = match self.state.get() {
                AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING => {
                    // only happens when restarting execution after await()
                    State::Resume
                }
                AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN => State::Done,
                AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START => {
                    if completion_type == GEN_MAGIC_NEXT {
                        self.set_throw_flag(false);
                        self.state
                            .set(AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING);
                        State::Resume
                    } else {
                        self.complete();
                        continue;
                    }
                }
                AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED => {
                    if completion_type == GEN_MAGIC_NEXT {
                        self.resolve(ctx, JSValue::UNDEFINED, true);
                    } else if completion_type == GEN_MAGIC_RETURN {
                        self.state
                            .set(AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN);
                        if unlikely(self.completed_return(ctx, result).is_err()) {
                            eprintln!(
                                "{}",
                                ctx.exception()
                                    .to_string(ctx)
                                    .map(|s| s.content().to_utf8(false).to_string())
                                    .unwrap_or("Error".to_string())
                            );
                        }
                    } else {
                        self.reject(ctx, result);
                    }
                    State::Done
                }
                AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                | AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR => {
                    if self.state.get() == AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                        && completion_type == GEN_MAGIC_THROW
                    {
                        ctx.throw(result);
                        self.set_throw_flag(true);
                    } else {
                        // 'yield' returns a value. 'yield *' also returns a value
                        // in case the 'throw' method is called
                        unsafe {
                            let mut s = self.func_state.0.borrow_mut();
                            let s = s.as_mut().get_unchecked_mut();
                            s.throw_flag = false;

                            // stack change:
                            // SP[-1] <- value
                            // SP[0] <- next completion type
                            // SP++
                            std::ptr::replace(s.frame.stack_cur_ptr().offset(-1).as_ptr(), result);
                            std::ptr::replace(
                                s.frame.stack_cur_ptr().as_ptr(),
                                JSValue::Int(completion_type),
                            );
                            s.frame.cur_sp_offset += 1;
                        }
                        self.set_throw_flag(false);
                    }
                    self.state
                        .set(AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING);
                    State::Resume
                }
            };

            match state {
                State::Done => {
                    return;
                }
                State::Resume => 'resume: loop {
                    let func_ret = self.func_state.resume(ctx);
                    if self.func_state.0.borrow().is_completed {
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
                                self.state.set(if code == FUNC_RET_YIELD_STAR {
                                    AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR
                                } else {
                                    AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                                });
                                self.resolve(ctx, value, false);
                            }
                            FUNC_RET_AWAIT => {
                                if self._await(ctx, value).is_err() {
                                    // exception: throw it
                                    self.set_throw_flag(true);
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

    fn completed_return(&self, ctx: &JSContext, value: JSValue) -> Result<(), i32> {
        let cc = CallContext::new(
            ctx,
            ctx.0.borrow().promise_ctor.clone(),
            std::slice::from_ref(&value),
            0, // is_reject=0
            None,
        );
        let promise: Promise = match js_promise_resolve(ctx, cc) {
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

        let resolving_funcs = js_async_generator_resolve_function_create(
            ctx,
            JSValue::Object(self.generator.borrow().upgrade().unwrap()),
            true,
        )?;
        debug_assert!(resolving_funcs[0].is_function());
        debug_assert!(resolving_funcs[1].is_function());

        promise.reaction(
            ctx,
            &resolving_funcs,
            &[JSValue::UNDEFINED, JSValue::UNDEFINED],
        )
    }

    fn _await(&self, ctx: &JSContext, value: JSValue) -> Result<(), i32> {
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

        let resolving_funcs = js_async_generator_resolve_function_create(
            ctx,
            JSValue::Object(self.generator.borrow().upgrade().unwrap()),
            false,
        )?;
        debug_assert!(resolving_funcs[0].is_function());
        debug_assert!(resolving_funcs[1].is_function());

        promise.reaction(
            ctx,
            &resolving_funcs,
            // no need to create 'thrownawayCapability' as in the spec
            &[JSValue::UNDEFINED, JSValue::UNDEFINED],
        )
    }

    /// mark async generator is completed
    pub fn complete(&self) {
        if self.state.get() != AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED {
            self.state
                .set(AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED);
            // self.func_state.take();
        }
    }

    fn resolve_or_reject(&self, ctx: &JSContext, result: &JSValue, is_reject: bool) {
        let req = self.queue.borrow_mut().remove(0);
        req.resolving_funcs[is_reject as usize]
            .call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(result))
            .unwrap();
    }

    pub fn resolve(&self, ctx: &JSContext, value: JSValue, done: bool) {
        let result = js_create_iterator_result(ctx, value, done).unwrap_or(JSValue::EXCEPTION);
        self.resolve_or_reject(ctx, &result, false);
    }

    #[inline(always)]
    pub fn reject(&self, ctx: &JSContext, exception: JSValue) {
        self.resolve_or_reject(ctx, &exception, true);
    }
}

/// An AsyncGeneratorFunction object
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct AsyncGeneratorFunction(JSObject);

object_cast!(AsyncGeneratorFunction, JS_CLASS_ASYNC_GENERATOR_FUNCTION);

impl AsyncGeneratorFunction {
    pub fn class_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let s = Box::new(AGenPayload {
            generator: RefCell::new(JSObjectWeakref::default()),
            state: Cell::new(AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START),
            func_state: AsyncFuncState::new(
                ctx,
                func_obj.as_object_BCFunction().unwrap().clone(),
                this_obj.clone(),
                args,
            )?,
            queue: RefCell::new(Vec::with_capacity(2)),
        });

        // execute the function till 'OpCode::INITIAL_YIELD' (no yield nor await are possible)
        s.func_state.resume(ctx)?;

        // create AsyncGenerator object
        let obj = ctx
            .create_object_from_ctor(func_obj, ObjectPayload::ASYNC_GENERATOR(s))
            .map_err(|_| -1)?;

        // set generator to obj weakref
        if let ObjectPayload::ASYNC_GENERATOR(s) = obj.payload() {
            *s.generator.borrow_mut() = obj.downgrade();
        } else {
            unreachable!()
        }

        Ok(obj.into())
    }

    pub fn class_finalizer(_rt: &mut JSRuntime, _isa: &ObjectPayload, _id: usize) {}

    pub fn class_mark(val: JSValue) {
        if let ObjectPayload::ASYNC_GENERATOR(s) = unsafe { val.as_object_unchecked().payload() } {
            s.func_state.gc_mark();

            for req in s.queue.borrow().iter() {
                req.result.gc_mark();
                req.promise.gc_mark();
                req.resolving_funcs[0].gc_mark();
                req.resolving_funcs[1].gc_mark();
            }
        }
    }
}

/// create pair of functions to handle fullfil and reject.
fn js_async_generator_resolve_function_create(
    ctx: &JSContext,
    generator: JSValue,
    is_resume_next: bool,
) -> Result<[JSValue; 2], i32> {
    let resolving_funcs: [JSValue; 2] = [
        // handle fullfil
        NativeFunctionWithData::create(
            ctx,
            js_async_generator_resolve_function,
            1,
            if is_resume_next { 2 } else { 0 },
            [generator.clone()].into(),
        )?
        .into(),
        // handle reject
        NativeFunctionWithData::create(
            ctx,
            js_async_generator_resolve_function,
            1,
            if is_resume_next { 3 } else { 1 },
            [generator].into(),
        )?
        .into(),
    ];

    Ok(resolving_funcs)
}

fn js_async_generator_resolve_function(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_reject = cc.magic() & 1 != 0;
    let arg = cc.arg(0);

    if let ObjectPayload::ASYNC_GENERATOR(s) = cc.data(0).as_object().unwrap().payload() {
        if cc.magic() >= 2 {
            // resume next case in AWAITING_RETURN state
            debug_assert!(matches!(
                s.state.get(),
                AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN
                    | AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED
            ));

            s.state.set(AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED);
            if is_reject {
                s.reject(ctx, arg.clone());
            } else {
                s.resolve(ctx, arg.clone(), true);
            }
        } else {
            // restart function execution after await()
            debug_assert_eq!(s.state.get(), AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING);

            s.set_throw_flag(is_reject);

            if is_reject {
                ctx.throw(arg.clone());
            } else {
                // return value of await.
                unsafe {
                    std::ptr::replace(
                        s.func_state
                            .0
                            .borrow_mut()
                            .frame
                            // HINT: stack_cur_ptr advances the addr of stack top value by 1.
                            // subtract 1 to access stack top value addr.
                            .stack_cur_ptr()
                            .offset(-1)
                            .as_ptr(),
                        arg.clone(),
                    );
                }
            }

            s.resume_next(ctx);
        }

        Ok(JSValue::UNDEFINED)
    } else {
        unreachable!()
    }
}

fn js_async_generator_next(ctx: &JSContext, cc: CallContext) -> JSResult {
    let (promise, resolving_funcs) = Promise::create(ctx)?;

    if let Ok(isa) = cc
        .this()
        .retrive_object_payload(ctx, JS_CLASS_ASYNC_GENERATOR)
    {
        let ObjectPayload::ASYNC_GENERATOR(s) = isa else {
            unreachable!()
        };

        let req = Box::new(JSAsyncGeneratorRequest {
            completion_type: cc.magic(),
            result: cc.arg(0).clone(),
            promise: promise.clone().into(),
            resolving_funcs,
        });
        s.queue.borrow_mut().push(req);

        if s.state.get() != AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING {
            s.resume_next(ctx);
        }
    } else {
        let err = ctx.take_exception();
        resolving_funcs[1].call(ctx, &JSValue::UNDEFINED, &[err])?;
    }

    Ok(promise.into())
}

fn js_iterator_proto_iterator(_ctx: &JSContext, cc: CallContext) -> JSResult {
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
        "AsyncGenerator",
        JS_PROP_CONFIGURABLE
    ),
];

pub static js_async_generator_function_proto_funcs: [FieldDef; 1] = [JS_PROP_STRING_DEF!(
    "[Symbol.toStringTag]",
    "AsyncGeneratorFunction",
    JS_PROP_CONFIGURABLE
)];
