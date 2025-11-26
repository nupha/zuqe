use std::cell::{Cell, RefCell};

use gc_lite::{Collect, GcRef, GcWeak};

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_PROP_STRING_DEF,
    branch_hints::unlikely,
    builtins::promise::{Promise, js_promise_resolve},
    callable::NativeFunctionWithData,
    class::ClassID,
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{
        AsyncFuncState, BytecodeFunction, CallContext, FieldDef, JSFunctionKindEnum,
        NativeFunctionType,
    },
    iterator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW, js_create_iterator_result},
    object::{JS_PROP_CONFIGURABLE, ObjectImpl, ObjectPayload, ObjectRef},
    object_cast,
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

unsafe impl Collect for JSAsyncGeneratorRequest {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.result.trace(tracer);
        self.promise.trace(tracer);
        self.resolving_funcs[0].trace(tracer);
        self.resolving_funcs[1].trace(tracer);
    }
}

#[derive(Debug)]
pub struct AGenPayload {
    /// back ref to the generator object (const)
    generator: RefCell<GcWeak<ObjectImpl>>,

    state: Cell<AGenState>,

    /// None for state AWAITING_RETURN and COMPLETED
    func_state: AsyncFuncState,

    /// list of JSAsyncGeneratorRequest.
    /// the queue's element is consumed in resolve() or reject() call.
    queue: RefCell<Vec<Box<JSAsyncGeneratorRequest>>>,
}

unsafe impl Collect for AGenPayload {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.func_state.trace(tracer);
        self.queue.borrow().trace(tracer);
    }
}

impl AGenPayload {
    #[inline]
    fn set_throw_flag(&mut self, is_reject: bool) {
        self.func_state.set_throw_flag(is_reject);
    }

    pub(crate) fn resume_next(&mut self, ctx: ContextRef) {
        enum State {
            Resume,
            Done,
        }

        while let Some((completion_type, result)) = {
            // Note: borrow() is enclosed in this code block
            self.queue
                .borrow()
                .first()
                .map(|x| (x.completion_type, x.result))
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
                        self.resolve(&ctx, JSValue::UNDEFINED, true);
                    } else if completion_type == GEN_MAGIC_RETURN {
                        self.state
                            .set(AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN);
                        if unlikely(self.completed_return(ctx, result).is_err()) {
                            eprintln!(
                                "{}",
                                ctx.rt()
                                    .exception()
                                    .to_string(&ctx)
                                    .map(|s| s.content().to_utf8(false).to_string())
                                    .unwrap_or("Error".to_string())
                            );
                        }
                    } else {
                        self.reject(&ctx, result);
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
                        self.func_state.set_throw_flag(false);
                        self.set_throw_flag(false);

                        // to update stack:
                        // SP[-1] <- value
                        // SP[0] <- next completion type
                        // SP++
                        unsafe {
                            std::ptr::replace(
                                self.func_state.frame.stack_cur_ptr().offset(-1).as_ptr(),
                                result,
                            );
                            std::ptr::replace(
                                self.func_state.frame.stack_cur_ptr().as_ptr(),
                                JSValue::Int(completion_type),
                            );

                            self.func_state
                                .frame
                                .as_mut()
                                .get_unchecked_mut()
                                .cur_sp_offset += 1;
                        }
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
                    let func_ret = self.func_state.resume(&ctx);
                    if self.func_state.is_completed {
                        // end of function
                        match func_ret {
                            Ok(val) => {
                                self.complete();
                                self.resolve(&ctx, val, true);
                            }
                            Err(_) => {
                                let exc = ctx.rt().take_exception();
                                self.complete();
                                self.reject(&ctx, exc);
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
                                self.resolve(&ctx, value, false);
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

    fn completed_return(&self, mut ctx: ContextRef, value: JSValue) -> Result<(), i32> {
        let cc = CallContext::new(
            ctx,
            ctx.promise_ctor,
            std::slice::from_ref(&value),
            0, // is_reject=0
            None,
        );
        let mut promise: Promise = match js_promise_resolve(ctx, cc) {
            Ok(r) => r,
            Err(_) => {
                // Fail looking up JS_ATOM_constructor when is_reject==0.
                // A poisoned .constructor property is observable and the resulting
                // exception should be delivered to the catch handler.
                let err = ctx.rt().take_exception();
                let cc = CallContext::new(
                    ctx,
                    ctx.promise_ctor,
                    std::slice::from_ref(&err),
                    1, // is_reject=1
                    None,
                );
                js_promise_resolve(ctx, cc)?
            }
        }
        .try_into()?;

        let resolving_funcs = js_async_generator_resolve_function_create(
            &ctx,
            JSValue::Object(ObjectRef(
                self.generator.borrow().upgrade(ctx.rt().gc_heap()).unwrap(),
            )),
            true,
        )?;
        debug_assert!(resolving_funcs[0].is_function());
        debug_assert!(resolving_funcs[1].is_function());

        promise.reaction(
            &mut ctx,
            resolving_funcs,
            [JSValue::UNDEFINED, JSValue::UNDEFINED],
        )
    }

    fn _await(&self, mut ctx: ContextRef, value: JSValue) -> Result<(), i32> {
        let mut promise: Promise = {
            let cc = CallContext::new_simple(ctx, ctx.promise_ctor, std::slice::from_ref(&value));
            js_promise_resolve(ctx, cc)?.try_into()?
        };

        let resolving_funcs = js_async_generator_resolve_function_create(
            &ctx,
            JSValue::Object(ObjectRef(
                self.generator.borrow().upgrade(ctx.rt().gc_heap()).unwrap(),
            )),
            false,
        )?;
        debug_assert!(resolving_funcs[0].is_function());
        debug_assert!(resolving_funcs[1].is_function());

        promise.reaction(
            &mut ctx,
            resolving_funcs,
            // no need to create 'thrownawayCapability' as in the spec
            [JSValue::UNDEFINED, JSValue::UNDEFINED],
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

    fn resolve_or_reject(&self, ctx: &ContextImpl, result: &JSValue, is_reject: bool) {
        let req = self.queue.borrow_mut().remove(0);
        req.resolving_funcs[is_reject as usize]
            .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(result))
            .unwrap();
    }

    pub fn resolve(&self, ctx: &ContextImpl, value: JSValue, done: bool) {
        let result = js_create_iterator_result(ctx, value, done).unwrap_or(JSValue::EXCEPTION);
        self.resolve_or_reject(ctx, &result, false);
    }

    #[inline(always)]
    pub fn reject(&self, ctx: &ContextImpl, exception: JSValue) {
        self.resolve_or_reject(ctx, &exception, true);
    }
}

/// An AsyncGeneratorFunction object
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct AsyncGeneratorFunction(GcRef<ObjectImpl>);

object_cast!(AsyncGeneratorFunction, JS_CLASS_ASYNC_GENERATOR_FUNCTION);

impl AsyncGeneratorFunction {
    pub fn class_call(
        ctx: ContextRef,
        func_obj: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut s = Box::new(AGenPayload {
            generator: RefCell::new(GcWeak::default()),
            state: Cell::new(AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START),
            func_state: AsyncFuncState::new(
                &ctx,
                *func_obj.as_object_BCFunction().unwrap(),
                this_obj,
                args,
            )?,
            queue: RefCell::new(Vec::with_capacity(2)),
        });

        // execute the function till 'OpCode::INITIAL_YIELD' (no yield nor await are possible)
        s.func_state.resume(&ctx)?;

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

    pub fn class_initialize(
        mut ctx: ContextRef,
        class_id: ClassID,
    ) -> Result<Option<ObjectRef>, i32> {
        debug_assert!(
            matches!(
                class_id,
                JS_CLASS_ASYNC_GENERATOR | JS_CLASS_ASYNC_GENERATOR_FUNCTION
            ),
            "{class_id}"
        );

        if ctx.class_proto[class_id as usize].is_object() {
            // already initialized
            return Ok(None);
        }

        // AsyncGenerator
        let aiter_proto = ctx.async_iterator_proto()?;
        let agen_proto: JSValue = {
            let mut o = ctx.create_base_object_with_proto(Some(aiter_proto))?;
            o.install_field_defs(&ctx, &js_async_generator_proto_funcs)?;
            o.into()
        };

        let func_proto = ctx.function_proto();

        // AsyncGeneratorFunction
        let mut o = ctx.create_base_object_with_proto(Some(func_proto))?;
        o.install_field_defs(&ctx, &js_async_generator_function_proto_funcs)?;
        let agen_func: JSValue = o.into();

        ctx.bind_ctor_proto_ex(
            agen_func,
            agen_proto,
            JS_PROP_CONFIGURABLE,
            JS_PROP_CONFIGURABLE,
        )?;

        let ctor: JSValue = ctx
            .create_native_function_with_proto(
                NativeFunctionType::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "AsyncGeneratorFunction",
                1,
                JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR as i16,
                Some(func_proto),
            )
            .unwrap()
            .into();
        ctx.bind_ctor_proto_ex(ctor, agen_func, 0, JS_PROP_CONFIGURABLE)?;

        ctx.class_proto[JS_CLASS_ASYNC_GENERATOR as usize] = agen_proto;
        ctx.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize] = agen_func;

        Ok(None)
    }
}

/// create pair of functions to handle fullfil and reject.
fn js_async_generator_resolve_function_create(
    ctx: &ContextImpl,
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
            [generator].into(),
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

fn js_async_generator_resolve_function(ctx: ContextRef, cc: CallContext) -> JSResult {
    let is_reject = cc.magic() & 1 != 0;
    let arg = *cc.arg(0);

    let mut data0 = *cc.data(0).as_object().unwrap();
    if let ObjectPayload::ASYNC_GENERATOR(s) = data0.payload_mut() {
        if cc.magic() >= 2 {
            // resume next case in AWAITING_RETURN state
            debug_assert!(matches!(
                s.state.get(),
                AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN
                    | AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED
            ));

            s.state.set(AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED);
            if is_reject {
                s.reject(&ctx, arg);
            } else {
                s.resolve(&ctx, arg, true);
            }
        } else {
            // restart function execution after await()
            debug_assert_eq!(s.state.get(), AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING);

            s.set_throw_flag(is_reject);

            if is_reject {
                ctx.throw(arg);
            } else {
                // return value of await.
                unsafe {
                    std::ptr::replace(
                        s.func_state
                            .frame
                            // HINT: stack_cur_ptr advances the addr of stack top value by 1.
                            // subtract 1 to access stack top value addr.
                            .stack_cur_ptr()
                            .offset(-1)
                            .as_ptr(),
                        arg,
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

fn js_async_generator_next(ctx: ContextRef, cc: CallContext) -> JSResult {
    let (promise, resolving_funcs) = Promise::create(&ctx)?;

    let mut this = *cc.this();
    if let Ok(isa) = this.retrive_object_payload_mut(&ctx, JS_CLASS_ASYNC_GENERATOR) {
        let ObjectPayload::ASYNC_GENERATOR(s) = isa else {
            unreachable!()
        };

        let req = Box::new(JSAsyncGeneratorRequest {
            completion_type: cc.magic(),
            result: *cc.arg(0),
            promise: promise.into(),
            resolving_funcs,
        });
        s.queue.borrow_mut().push(req);

        if s.state.get() != AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING {
            s.resume_next(ctx);
        }
    } else {
        let err = ctx.rt().take_exception();
        resolving_funcs[1].call(&ctx, JSValue::UNDEFINED, std::slice::from_ref(&err))?;
    }

    Ok(promise.into())
}

fn js_iterator_proto_iterator(_: ContextRef, cc: CallContext) -> JSResult {
    Ok(*cc.this())
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
