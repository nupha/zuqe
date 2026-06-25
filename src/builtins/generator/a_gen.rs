// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::RefCell;

use gc_lite::{GcRef, GcTrace, GcTraceCtx, GcWeak};

use crate::{
    Atom, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    builtins::{
        iterator::make_iterator_next_result,
        promise::{Promise, js_promise_resolve},
    },
    class::ClassID,
    context::ScopedContext,
    error::JSResult,
    function::{
        AsyncState, BytecodeFunction, CallContext, Callback, FieldDef, FunctionKind, NativeFunction,
    },
    object::{JS_PROP_CONFIGURABLE, Object, ObjectImpl, ObjectPayload},
    object_cast,
    utils::unlikely,
    value::JSValue,
};

use super::{
    FUNC_RET_AWAIT, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR, GEN_MAGIC_NEXT, GEN_MAGIC_RETURN,
    GEN_MAGIC_THROW,
};

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

impl GcTrace for JSAsyncGeneratorRequest {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.result.trace(tr);
        self.promise.trace(tr);
        self.resolving_funcs[0].trace(tr);
        self.resolving_funcs[1].trace(tr);
    }
}

#[derive(Debug)]
pub struct AGenPayload {
    /// back ref to the generator object (const)
    generator: RefCell<GcWeak<ObjectImpl>>,
    /// generator state
    state: AGenState,

    /// Async execution state for bytecode running
    async_state: AsyncState,

    /// list of JSAsyncGeneratorRequest.
    /// the queue's element is consumed in resolve() or reject() call.
    queue: Vec<Box<JSAsyncGeneratorRequest>>,
}

impl GcTrace for AGenPayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.async_state.trace(gcx);
        self.queue.iter().for_each(|x| x.trace(gcx));
    }
}

impl AGenPayload {
    #[inline(always)]
    const fn state(&self) -> AGenState {
        self.state
    }

    #[inline]
    const fn set_state(&mut self, state: AGenState) {
        self.state = state;
    }

    pub(crate) fn resume_next(&mut self, ctx: &mut ScopedContext) {
        enum State {
            Resume,
            Done,
        }

        while let Some((completion_type, result)) =
            self.queue.first().map(|x| (x.completion_type, x.result))
        {
            let state = match self.state() {
                AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING => {
                    // only happens when restarting execution after await()
                    State::Resume
                }
                AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN => State::Done,
                AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START => {
                    if completion_type == GEN_MAGIC_NEXT {
                        self.async_state.set_throw_flag(false);
                        self.set_state(AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING);
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
                        self.set_state(AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN);
                        if unlikely(self.completed_return(ctx, result).is_err()) {
                            eprintln!("{}", ctx.rt().exception().to_text(ctx).message);
                        }
                    } else {
                        self.reject(ctx, result);
                    }
                    State::Done
                }
                AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                | AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR => {
                    if self.state() == AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                        && completion_type == GEN_MAGIC_THROW
                    {
                        ctx.throw_value(result);
                        self.async_state.set_throw_flag(true);
                    } else {
                        // 'yield' returns a value. 'yield *' also returns a value
                        // in case the 'throw' method is called
                        self.async_state.set_throw_flag(false);

                        // update stack:
                        // SP[-1] <- value
                        // SP[0]  <- completion type
                        // SP++
                        let sf = unsafe {
                            self.async_state
                                .stack_frame
                                .as_mut()
                                .get_unchecked_mut()
                        };
                        sf.store(-1, result);
                        sf.push(JSValue::Int(completion_type));
                    }

                    self.set_state(AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING);
                    State::Resume
                }
            };

            match state {
                State::Done => {
                    return;
                }
                State::Resume => 'resume: loop {
                    let func_ret = self.async_state.resume(ctx);
                    if self.async_state.is_completed {
                        // end of function
                        match func_ret {
                            Ok(val) => {
                                self.complete();
                                // Protect the function return value; it was taken
                                // from the async stack in AsyncStateImpl::resume()
                                // via ptr::replace without protection.
                                if let Some(n) = val.gc_node_ptr() {
                                    ctx.gc_state().add_non_local(n);
                                }
                                self.resolve(ctx, val, true);
                            }
                            Err(_) => {
                                let err_val = ctx.take_exception().to_value(ctx);
                                self.complete();
                                self.reject(ctx, err_val);
                            }
                        }
                    } else {
                        let code = if let Ok(JSValue::Int(i)) = func_ret {
                            i
                        } else {
                            unreachable!()
                        };

                        // take stack top value with GC protection.
                        let value = self.async_state.take_top(ctx);

                        match code {
                            FUNC_RET_YIELD | FUNC_RET_YIELD_STAR => {
                                self.set_state(if code == FUNC_RET_YIELD_STAR {
                                    AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR
                                } else {
                                    AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                                });
                                self.resolve(ctx, value, false);
                            }
                            FUNC_RET_AWAIT => {
                                if self._await(ctx, value).is_err() {
                                    debug_assert!(ctx.rt().exception() != crate::Exception::No);
                                    self.async_state.set_throw_flag(true);
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

    fn completed_return(&self, ctx: &mut ScopedContext, value: JSValue) -> Result<(), i32> {
        let cc = CallContext::new(
            ctx.promise_ctor().into(),
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
                let err_val = ctx.take_exception().to_value(ctx);
                js_promise_resolve(
                    ctx,
                    CallContext::new(
                        ctx.promise_ctor().into(),
                        std::slice::from_ref(&err_val),
                        1, // is_reject=1
                        None,
                    ),
                )?
            }
        }
        .try_into()?;

        let generator = JSValue::Object(Object(
            self.generator
                .borrow()
                .upgrade(ctx.rt_ptr().gc_heap())
                .unwrap()
                .into_raw(),
        ));
        let resolving_funcs = js_async_generator_resolve_function_create(ctx, generator, true)?;
        debug_assert!(resolving_funcs[0].is_callable());
        debug_assert!(resolving_funcs[1].is_callable());

        promise.reaction(
            ctx,
            resolving_funcs,
            [JSValue::UNDEFINED, JSValue::UNDEFINED],
        )
    }

    fn _await(&self, ctx: &mut ScopedContext, value: JSValue) -> Result<(), i32> {
        let mut promise: Promise = js_promise_resolve(
            ctx,
            CallContext::new_simple(ctx.promise_ctor().into(), std::slice::from_ref(&value)),
        )?
        .try_into()?;

        let gen_obj: Object = self
            .generator
            .borrow()
            .upgrade(ctx.rt().gc_heap())
            .unwrap()
            .into_raw()
            .into();

        let resolving_funcs =
            js_async_generator_resolve_function_create(ctx, gen_obj.into(), false)?;

        debug_assert!(resolving_funcs[0].is_callable());
        debug_assert!(resolving_funcs[1].is_callable());

        promise.reaction(
            ctx,
            resolving_funcs,
            // no need to create 'thrownawayCapability' as in the spec
            [JSValue::UNDEFINED, JSValue::UNDEFINED],
        )
    }

    /// mark async generator is completed
    pub fn complete(&mut self) {
        if self.state() != AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED {
            self.set_state(AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED);
        }
    }

    /// Dequeue a request from the queue, protecting all its GC-referenced
    /// fields in the current scope. The fields lose GC trace protection
    /// when removed from AGenPayload::queue; this method ensures they
    /// survive GC during the subsequent resolve/reject call.
    pub fn dequeue(&mut self, ctx: &ScopedContext) -> Box<JSAsyncGeneratorRequest> {
        let req = self.queue.remove(0);
        for v in [
            req.result,
            req.promise,
            req.resolving_funcs[0],
            req.resolving_funcs[1],
        ]
        .iter()
        {
            if let Some(n) = v.gc_node_ptr() {
                ctx.gc_state().add_non_local(n);
            }
        }
        req
    }

    fn resolve_or_reject(&mut self, ctx: &mut ScopedContext, result: &JSValue, is_reject: bool) {
        // result is created by make_iterator_next_result (in resolve()) as a
        // LOCAL in the current scope — already protected. Verify with assert.
        debug_assert!(
            result.gc_node_ptr().map_or(true, |n| unsafe {
                n.as_ref().is_root_or_local()
            }),
            "result must be gc local or root"
        );

        let req = self.dequeue(ctx);
        let func = req.resolving_funcs[is_reject as usize];
        func.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(result))
            .unwrap();
    }

    #[inline(always)]
    pub(crate) fn resolve(&mut self, ctx: &mut ScopedContext, value: JSValue, done: bool) {
        debug_assert!(
            value.gc_node_ptr().map_or(true, |n| unsafe {
                n.as_ref().is_root_or_local()
            }),
            "value must be gc local or root"
        );
        let result = make_iterator_next_result(ctx, value, done).unwrap_or(JSValue::EXCEPTION);
        self.resolve_or_reject(ctx, &result, false);
    }

    #[inline(always)]
    pub(crate) fn reject(&mut self, ctx: &mut ScopedContext, exception: JSValue) {
        self.resolve_or_reject(ctx, &exception, true);
    }
}

/// An AsyncGeneratorFunction object
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct AsyncGeneratorFunction(GcRef<ObjectImpl>);

object_cast!(AsyncGeneratorFunction, JS_CLASS_ASYNC_GENERATOR_FUNCTION);

impl AsyncGeneratorFunction {
    pub fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        match class_id {
            JS_CLASS_ASYNC_GENERATOR => {
                let aiter_proto = ctx.async_iterator_proto()?;
                let proto = ctx.create_base_object_with_proto(Some(aiter_proto))?;
                ctx.init_class_proto_internal(class_id, proto, js_async_generator_proto_funcs)
            }
            JS_CLASS_ASYNC_GENERATOR_FUNCTION => {
                let mut agen_func_proto =
                    ctx.create_base_object_with_proto(Some(ctx.function_proto()))?;
                agen_func_proto.install_fields(ctx, js_async_generator_function_proto_funcs)?;
                ctx.set_class_proto(class_id, agen_func_proto);

                let agen_proto = if let Some(o) = ctx.class_proto(JS_CLASS_ASYNC_GENERATOR) {
                    o
                } else {
                    // init AsyncGenerator first
                    const {
                        debug_assert!(JS_CLASS_ASYNC_GENERATOR_FUNCTION > JS_CLASS_ASYNC_GENERATOR);
                    }
                    Self::class_init(ctx, JS_CLASS_ASYNC_GENERATOR)?;
                    ctx.class_proto(JS_CLASS_ASYNC_GENERATOR).unwrap()
                };
                ctx.bind_ctor_proto_ex(
                    agen_func_proto,
                    agen_proto,
                    JS_PROP_CONFIGURABLE,
                    JS_PROP_CONFIGURABLE,
                )?;

                let agen_func_ctor = NativeFunction::create(
                    ctx,
                    Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
                    Atom::CONST_AsyncGeneratorFunction,
                    1,
                    FunctionKind::AsyncGenerator as i16,
                    ctx.function_ctor(),
                    None,
                )?;
                ctx.bind_ctor_proto_ex(
                    agen_func_ctor.into(),
                    agen_func_proto,
                    0,
                    JS_PROP_CONFIGURABLE,
                )?;
                ctx.class_slots[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize].ctor =
                    Some(agen_func_ctor.into());

                Ok(())
            }
            _ => {
                unreachable!();
            }
        }
    }

    pub fn class_call(
        ctx: &mut ScopedContext,
        func_obj: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut s = Box::new(AGenPayload {
            generator: RefCell::new(GcWeak::default()),
            state: AGenState::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START,
            async_state: AsyncState::new(
                ctx,
                [const { JSValue::UNDEFINED }; 2],
                func_obj.as_object_BCWrapper().copied().unwrap(),
                this_obj,
                args,
            )?,
            queue: Vec::with_capacity(2),
        });

        // execute the function till 'OpCode::INITIAL_YIELD' (no yield nor await are possible)
        s.async_state.resume(ctx)?;

        // create AsyncGenerator object
        let mut obj = ctx
            .create_instance_with_ctor(func_obj, ObjectPayload::ASYNC_GENERATOR(s))
            .map_err(|_| -1)?;

        // set generator to obj weakref
        let w = obj.downgrade();
        *obj.payload_mut()
            .async_generator_mut()
            .unwrap()
            .generator
            .borrow_mut() = w;

        Ok(obj.into())
    }
}

/// create pair of functions to handle fullfil and reject.
fn js_async_generator_resolve_function_create(
    ctx: &mut ScopedContext,
    generator: JSValue,
    is_resume_next: bool,
) -> Result<[JSValue; 2], i32> {
    let resolving_funcs: [JSValue; 2] = [
        // handle fullfil
        NativeFunction::create(
            ctx,
            Callback::Func(js_async_generator_resolve_function),
            Atom::CONST_empty_string,
            1,
            if is_resume_next { 2 } else { 0 },
            ctx.function_proto(),
            Some([generator].into()),
        )?
        .into(),
        // handle reject
        NativeFunction::create(
            ctx,
            Callback::Func(js_async_generator_resolve_function),
            Atom::CONST_empty_string,
            1,
            if is_resume_next { 3 } else { 1 },
            ctx.function_proto(),
            Some([generator].into()),
        )?
        .into(),
    ];

    Ok(resolving_funcs)
}

fn js_async_generator_resolve_function(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_reject = cc.magic() & 1 != 0;

    let arg = cc.arg(0);
    let mut data0 = *cc.data(0).as_object().unwrap();
    if let ObjectPayload::ASYNC_GENERATOR(s) = data0.payload_mut() {
        if cc.magic() >= 2 {
            // resume next case in AWAITING_RETURN state
            debug_assert!(matches!(
                s.state(),
                AGenState::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN
                    | AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED
            ));

            s.set_state(AGenState::JS_ASYNC_GENERATOR_STATE_COMPLETED);
            if is_reject {
                s.reject(ctx, arg);
            } else {
                // Protect resolved value before resolve(), which may allocate.
                if let Some(n) = arg.gc_node_ptr() {
                    ctx.gc_state().add_non_local(n);
                }
                s.resolve(ctx, arg, true);
            }
        } else {
            // restart function execution after await()
            debug_assert_eq!(s.state(), AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING);

            if unlikely(is_reject) {
                ctx.throw_value(arg);
                s.async_state.set_throw_flag(true);
            } else {
                s.async_state.set_throw_flag(false);

                // set return value of await.
                s.async_state.stack_frame.store(-1, arg);
            }

            s.resume_next(ctx);
        }

        Ok(JSValue::UNDEFINED)
    } else {
        unreachable!()
    }
}

fn js_async_generator_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (promise, resolving_funcs) = Promise::create(ctx)?;

    let mut this = cc.this();
    if let Ok(isa) = this.retrive_object_payload_mut(ctx, JS_CLASS_ASYNC_GENERATOR) {
        let ObjectPayload::ASYNC_GENERATOR(s) = isa else {
            unreachable!()
        };

        let req = Box::new(JSAsyncGeneratorRequest {
            completion_type: cc.magic(),
            result: cc.arg(0),
            promise: promise.into(),
            resolving_funcs,
        });
        s.queue.push(req);

        if s.state() != AGenState::JS_ASYNC_GENERATOR_STATE_EXECUTING {
            s.resume_next(ctx);
        }
    } else {
        let err_val = ctx.take_exception().to_value(ctx);
        resolving_funcs[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
    }

    Ok(promise.into())
}

const js_async_generator_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("next", 1, js_async_generator_next, GEN_MAGIC_NEXT as _),
    FieldDef::FUNC("return", 1, js_async_generator_next, GEN_MAGIC_RETURN as _),
    FieldDef::FUNC("throw", 1, js_async_generator_next, GEN_MAGIC_THROW as _),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "AsyncGenerator",
        JS_PROP_CONFIGURABLE,
    ),
];

const js_async_generator_function_proto_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "AsyncGeneratorFunction",
        JS_PROP_CONFIGURABLE,
    ),
];
