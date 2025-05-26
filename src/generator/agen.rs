use crate::{
    context::JSContext,
    error::JSResult,
    function::{
        async_func_free, async_func_init, async_func_resume, CallArgs, JSAsyncFunctionState,
        JSCFunctionListEntry, JS_Call,
    },
    gc::{JS_MarkFunc, JS_MarkValue},
    iterator::{js_create_iterator_result, GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW},
    list_entry,
    mem::js_malloc_typed,
    promise::{js_promise_resolve, PromiseValue},
    runtime::JSRuntime,
    types::{
        init_list_head, list_add_tail, list_del, list_empty, list_foreach, list_foreach_safe,
        list_head,
    },
    utils::UncheckedInto,
    value::{
        object::{JSObject, JSObjectIsa, JS_PROP_CONFIGURABLE},
        JSTag, JSValue, JSValueConst, JSValueRc, JS_FreeValue, JS_FreeValueRT, JS_TAG_INT,
        JS_TAG_OBJECT, JS_VALUE_GET_INT, JS_VALUE_GET_TAG,
    },
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_MKPTR, JS_PROP_STRING_DEF,
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

pub struct JSAsyncGeneratorData {
    /// back pointer to the object (const)
    pub generator: *mut JSObject,

    pub state: JSAsyncGeneratorStateEnum,
    /// func_state is NULL is state AWAITING_RETURN and COMPLETED
    pub func_state: *mut JSAsyncFunctionState,

    /// list of JSAsyncGeneratorRequest.link
    /// TODO: use Vec<Box<JSAsyncGeneratorRequest>>
    pub queue: list_head,
}

#[repr(C)]
pub struct JSAsyncGeneratorRequest {
    pub link: list_head,
    /// completion: GEN_MAGIC_x
    pub completion_type: i32,
    pub result: JSValue,

    /// promise capability
    pub promise: JSValue,
    pub resolving_funcs: [JSValue; 2],
}

impl JSAsyncGeneratorData {
    /// drop the generator data and all holding requests
    pub fn dispose(mut self: Box<Self>, rt: &mut JSRuntime) {
        unsafe {
            list_foreach_safe(&mut self.queue, |el| {
                let req = list_entry!(el, JSAsyncGeneratorRequest, link);

                JS_FreeValueRT(rt, (*req).result);
                JS_FreeValueRT(rt, (*req).promise);
                JS_FreeValueRT(rt, (*req).resolving_funcs[0]);
                JS_FreeValueRT(rt, (*req).resolving_funcs[1]);

                drop(Box::from_raw(req));
            });

            if !self.func_state.is_null() {
                async_func_free(rt, self.func_state);
            }
        }
    }
}

/// An AsyncGeneratorValue value
#[repr(transparent)]
#[derive(Debug, Copy, Clone)]
pub struct AsyncGeneratorValue(JSValue);

impl JSValue {
    /// interpret value as AsyncGeneratorValue object.
    /// you must ensure that `value` is an AsyncGeneratorValue object.
    #[inline(always)]
    pub unsafe fn as_async_generator_unchecked(self) -> AsyncGeneratorValue {
        debug_assert_eq!(JS_VALUE_GET_TAG(self), JS_TAG_OBJECT);
        debug_assert!(matches!(
            self.object_unchecked().class_id(),
            JS_CLASS_ASYNC_GENERATOR | JS_CLASS_ASYNC_GENERATOR_FUNCTION
        ));
        std::mem::transmute(self)
    }
}

//
// Hooks
//
impl AsyncGeneratorValue {
    pub unsafe fn hook_call(
        ctx: &mut JSContext,
        func_obj: JSValue,
        this_obj: JSValue,
        argc: i32,
        argv: *const JSValue,
        flags: i32,
    ) -> JSValue {
        let o = JSAsyncGeneratorData {
            generator: std::ptr::null_mut(), // unkown for now
            state: JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START,
            func_state: {
                let st = async_func_init(ctx, func_obj, this_obj, argc, argv);
                if st.is_null() {
                    return JSValue::EXCEPTION;
                }
                st
            },
            queue: list_head::default(),
        };
        let s = if let Some(mut p) = js_malloc_typed::<JSAsyncGeneratorData>(ctx, 1, true) {
            p.write(o);
            init_list_head(&mut p.as_mut().queue);
            Box::from_raw(p.as_ptr())
        } else {
            return JSValue::EXCEPTION;
        };

        // execute the function up to 'OP_initial_yield' (no yield nor await are possible)
        let func_ret = async_func_resume(ctx, s.func_state);
        if func_ret.is_exception() {
            s.dispose(ctx.rt_mut());
            return JSValue::EXCEPTION;
        }
        JS_FreeValue(ctx, func_ret);

        // create AsyncGenerator instance
        match ctx
            .create_instance_from_ctor(func_obj, JSObjectIsa::ASYNC_GENERATOR(Box::into_raw(s)))
        {
            Ok(obj) => {
                let ob = obj.object_unchecked();
                let obptr = ob as *mut JSObject;
                // fix generator field.
                if let JSObjectIsa::ASYNC_GENERATOR(s) = ob.isa_mut() {
                    (*s).as_mut().unwrap().generator = obptr;
                } else {
                    #[cfg(debug_assertions)]
                    unreachable!()
                }
                obj
            }
            Err(isa) => {
                if let JSObjectIsa::ASYNC_GENERATOR(s) = isa {
                    Box::from_raw(s).dispose(ctx.rt_mut());
                }
                JSValue::EXCEPTION
            }
        }
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        if let Some(JSObjectIsa::ASYNC_GENERATOR(p)) = val.object().map(|o| o.isa_mut()) {
            Box::from_raw(*p).dispose(rt.as_mut().unwrap());
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        if let Some(JSObjectIsa::ASYNC_GENERATOR(p)) = val.object().map(|o| o.isa()) {
            list_foreach(&mut p.as_mut().unwrap().queue, |el| {
                let req = list_entry!(el, JSAsyncGeneratorRequest, link);

                JS_MarkValue(rt, (*req).result, mark_func);
                JS_MarkValue(rt, (*req).promise, mark_func);
                JS_MarkValue(rt, (*req).resolving_funcs[0], mark_func);
                JS_MarkValue(rt, (*req).resolving_funcs[1], mark_func);
            });

            if !p.as_ref().unwrap().func_state.is_null() {
                mark_func(rt, &mut (*p.as_ref().unwrap().func_state).header);
            }
        }
    }
}

impl JSAsyncGeneratorData {
    pub fn resume_next(&mut self, ctx: &mut JSContext) {
        enum State {
            ResumeExec,
            Done,
        }

        loop {
            let state: State = loop {
                if list_empty(&mut self.queue) {
                    break State::Done;
                }

                let next = unsafe {
                    list_entry!(self.queue.next, JSAsyncGeneratorRequest, link)
                        .as_mut()
                        .unwrap()
                };

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
                            unsafe {
                                (*self.func_state).throw_flag = false;
                            }
                            self.state =
                                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                            break State::ResumeExec;
                        } else {
                            self.complete(ctx);
                        }
                    }
                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED => {
                        if next.completion_type == GEN_MAGIC_NEXT {
                            self.resolve(ctx, JSValue::UNDEFINED, true);
                        } else if next.completion_type == GEN_MAGIC_RETURN {
                            self.state =
                                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN;
                            unsafe {
                                js_async_generator_completed_return(ctx, self, next.result)
                                    .unwrap();
                            }
                        } else {
                            self.reject(ctx, next.result);
                        }
                        break State::Done;
                    }
                    JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                    | JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR => {
                        let value = next.result.dup();
                        if self.state
                            == JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                            && next.completion_type == GEN_MAGIC_THROW
                        {
                            ctx.throw(value);
                            unsafe {
                                (*self.func_state).throw_flag = true;
                            }
                        } else {
                            // 'yield' returns a value. 'yield *' also returns a value
                            // in case the 'throw' method is called
                            unsafe {
                                *(*self.func_state)
                                    .frame
                                    .stack_current_ptr()
                                    .offset(-1)
                                    .as_mut() = value;
                                *(*self.func_state).frame.stack_current_ptr().as_mut() =
                                    JSValue::from_i32((*next).completion_type);
                                (*self.func_state).frame.cur_sp_offset += 1;
                                (*self.func_state).throw_flag = false;
                            }
                        }
                        self.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                        break State::ResumeExec;
                    }
                }
            }; // inner loop

            match state {
                State::Done => {
                    return;
                }
                State::ResumeExec => loop {
                    unsafe {
                        let func_ret = async_func_resume(ctx, self.func_state);
                        if (*self.func_state).is_completed {
                            if func_ret.is_exception() {
                                let value = (*ctx).take_exception();
                                self.complete(ctx);
                                self.reject(ctx, value);
                                JS_FreeValue(ctx, value);
                            } else {
                                // end of function
                                self.complete(ctx);
                                self.resolve(ctx, func_ret, true);
                                JS_FreeValue(ctx, func_ret);
                            }
                        } else {
                            debug_assert_eq!(JS_VALUE_GET_TAG(func_ret), JS_TAG_INT);
                            let func_ret_code = JS_VALUE_GET_INT(func_ret);
                            let value = (*self.func_state)
                                .frame
                                .stack_current_ptr()
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
                                    JS_FreeValue(ctx, value);
                                }
                                FUNC_RET_AWAIT => {
                                    let ret = js_async_generator_await(ctx, self, value);
                                    JS_FreeValue(ctx, value);
                                    if ret.is_err() {
                                        // exception: throw it
                                        (*self.func_state).throw_flag = true;
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
    pub fn complete(&mut self, ctx: &mut JSContext) {
        if self.state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED {
            self.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED;
            unsafe {
                async_func_free(ctx.rt_mut(), self.func_state);
            }
            self.func_state = std::ptr::null_mut();
        }
    }

    /// `resule` is not consumed.
    unsafe fn resolve_or_reject(
        &mut self,
        ctx: &mut JSContext,
        mut result: JSValue,
        is_reject: bool,
    ) {
        // XXX: better exception handling ?

        let next = list_entry!(self.queue.next, JSAsyncGeneratorRequest, link);

        list_del(&mut (*next).link);

        let ret = JS_Call(
            ctx,
            (*next).resolving_funcs[is_reject as usize],
            JSValue::UNDEFINED,
            1,
            &mut result,
        );

        JS_FreeValue(ctx, ret);
        JS_FreeValue(ctx, (*next).result);
        JS_FreeValue(ctx, (*next).promise);
        JS_FreeValue(ctx, (*next).resolving_funcs[0]);
        JS_FreeValue(ctx, (*next).resolving_funcs[1]);

        drop(Box::from_raw(next));
    }

    /// `value` is not consumed.
    pub fn resolve(&mut self, ctx: &mut JSContext, value: JSValue, done: bool) {
        unsafe {
            let result = js_create_iterator_result(ctx, value.dup(), done);
            self.resolve_or_reject(ctx, result, false);
            JS_FreeValue(ctx, result);
        }
    }

    /// `exceptrion` is not consumed
    #[inline(always)]
    pub fn reject(self: &mut JSAsyncGeneratorData, ctx: &mut JSContext, exception: JSValue) {
        unsafe {
            self.resolve_or_reject(ctx, exception, true);
        }
    }
}

#[deprecated(note = "use JSAsyncGeneratorData::resume_next() instead")]
pub unsafe fn js_async_generator_resume_next(ctx: *mut JSContext, s: *mut JSAsyncGeneratorData) {
    (*s).resume_next(ctx.as_mut().unwrap());
}

#[deprecated(note = "use JSAsyncGeneratorData::complete() instead")]
pub unsafe fn js_async_generator_complete(ctx: *mut JSContext, s: *mut JSAsyncGeneratorData) {
    (*s).complete(ctx.as_mut().unwrap());
}

/// `value` is not consumed.
#[deprecated(note = "use JSAsyncGeneratorData::resolve() instead")]
pub unsafe fn js_async_generator_resolve(
    ctx: *mut JSContext,
    s: *mut JSAsyncGeneratorData,
    value: JSValueConst,
    done: bool,
) {
    (*s).resolve(ctx.as_mut().unwrap(), value, done);
}

#[inline(always)]
#[deprecated(note = "use JSAsyncGeneratorData::reject() instead")]
pub unsafe fn js_async_generator_reject(
    ctx: *mut JSContext,
    s: *mut JSAsyncGeneratorData,
    exception: JSValueConst,
) {
    (*s).reject(ctx.as_mut().unwrap(), exception);
}

#[deprecated(note = "use JSAsyncGeneratorData::resolve_or_reject() instead")]
pub unsafe fn js_async_generator_resolve_or_reject(
    ctx: *mut JSContext,
    s: *mut JSAsyncGeneratorData,
    mut result: JSValueConst,
    is_reject: bool,
) {
    (*s).resolve_or_reject(ctx.as_mut().unwrap(), result, is_reject);
}

pub unsafe fn js_async_generator_completed_return(
    ctx: &mut JSContext,
    s: *mut JSAsyncGeneratorData,
    value: JSValue,
) -> Result<(), i32> {
    let promise: JSValueRc = match {
        let a = [value];
        let cc = CallArgs::new(
            ctx.handle(),
            ctx.promise_ctor,
            &a,
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
            let err: JSValueRc = (*ctx).take_exception().into_rc(ctx.rt());

            let a = [*err];
            let cc = CallArgs::new(
                ctx.handle(),
                ctx.promise_ctor,
                &a,
                1, // is_reject=1
                None,
            );
            js_promise_resolve(ctx, cc)?
        }
    }
    .into_rc(ctx.rt());

    //let mut promise: JSValueRc = js_promise_resolve(ctx, cc)?.into_rc(ctx.rt());
    // A poisoned .constructor property is observable and the resulting
    // exception should be delivered to the catch handler.
    // if promise.is_exception() {
    //     let mut err = (*ctx).take_exception().into_rc(ctx.rt());
    //     promise = js_promise_resolve(
    //         ctx,
    //         ctx.promise_ctor,
    //         1,
    //         err.deref_mut(),
    //         1, // is_reject
    //     )
    //     .check_exception()?
    //     .into_rc(ctx.rt());
    // }

    let resolving_funcs1 = js_async_generator_resolve_function_create(
        ctx,
        JS_MKPTR!(JSTag::Object, (*s).generator.cast()),
        true,
    )?;

    let res = UncheckedInto::<PromiseValue>::unchecked_into(*promise).reaction(
        ctx,
        &resolving_funcs1,
        &[JSValue::UNDEFINED, JSValue::UNDEFINED],
    );

    JS_FreeValue(ctx, resolving_funcs1[0]);
    JS_FreeValue(ctx, resolving_funcs1[1]);

    res
}

pub unsafe fn js_async_generator_await(
    ctx: &mut JSContext,
    s: *mut JSAsyncGeneratorData,
    value: JSValueConst,
) -> Result<(), i32> {
    let promise: JSValueRc = {
        let a = [value];
        let cc = CallArgs::new(ctx.handle(), ctx.promise_ctor, &a, 0, None);
        js_promise_resolve(ctx, cc)?.into_rc((*ctx).rt())
    };

    let resolving_funcs = js_async_generator_resolve_function_create(
        ctx,
        JS_MKPTR!(JSTag::Object, (*s).generator.cast()),
        false,
    )?;

    let res = UncheckedInto::<PromiseValue>::unchecked_into(*promise).reaction(
        ctx,
        &resolving_funcs,
        // no need to create 'thrownawayCapability' as in the spec
        &[JSValue::UNDEFINED, JSValue::UNDEFINED],
    );

    JS_FreeValue(ctx, resolving_funcs[0]);
    JS_FreeValue(ctx, resolving_funcs[1]);

    res
}

/// create [resolve, reject] functions for async `generator`.
/// `generator` is not consumed.
pub unsafe fn js_async_generator_resolve_function_create(
    ctx: *mut JSContext,
    generator: JSValueConst,
    is_resume_next: bool,
) -> Result<[JSValue; 2], i32> {
    let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    for i in 0..2 {
        if let Ok(func) = (*ctx).create_native_callable_object(
            js_async_generator_resolve_function,
            1,
            i + if is_resume_next { 2 } else { 0 },
            &[generator],
        ) {
            resolving_funcs[i as usize] = func;
        } else {
            JS_FreeValue(ctx, resolving_funcs[0]);
            JS_FreeValue(ctx, resolving_funcs[1]);
            return Err(-1);
        }
    }

    Ok(resolving_funcs)
}

pub fn js_async_generator_resolve_function(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let is_reject = args.magic & 1 != 0;
    let val = args.get_raw(0);

    let p = unsafe {
        if let JSObjectIsa::ASYNC_GENERATOR(s) = val.object_unchecked().isa_mut() {
            (*s).as_mut().unwrap()
        } else {
            unreachable!()
        }
    };

    if args.magic >= 2 {
        // resume next case in AWAITING_RETURN state
        debug_assert!(matches!(
            p.state,
            JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN
                | JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED
        ));

        p.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED;
        if is_reject {
            p.reject(ctx, val);
        } else {
            p.resolve(ctx, val, true);
        }
    } else {
        // restart function execution after await()
        debug_assert_eq!(
            p.state,
            JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING
        );

        unsafe {
            (*p.func_state).throw_flag = is_reject;
        }

        if is_reject {
            ctx.throw(val.dup());
        } else {
            // return value of await
            unsafe {
                *(*p.func_state)
                    .frame
                    .stack_current_ptr()
                    .offset(-1)
                    .as_mut() = val.dup();
            }
        }

        p.resume_next(ctx);
    }

    Ok(JSValue::UNDEFINED)
}

pub unsafe fn js_async_generator_next(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let (promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(JSValue::UNDEFINED)
    {
        (pr, rr)
    } else {
        return JSValue::EXCEPTION;
    };

    if let Some(JSObjectIsa::ASYNC_GENERATOR(p)) = this_val.object().map(|o| o.isa_mut()) {
        let p = (*p).as_mut().unwrap();

        let mut req = Box::new(JSAsyncGeneratorRequest {
            link: list_head::default(),
            completion_type: magic,
            result: (*argv.offset(0)).dup(),
            promise: promise.as_value().dup(),
            resolving_funcs,
        });

        list_add_tail(&mut req.link, &mut (*p).queue);
        let _ = Box::into_raw(req); // leak

        if p.state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING {
            p.resume_next(ctx.as_mut().unwrap());
        }
    } else {
        (*ctx).JS_ThrowTypeError(c"not an AsyncGeneratorValue object");
        let mut err = (*ctx).take_exception();
        let res2 = JS_Call(
            ctx.as_mut().unwrap(),
            resolving_funcs[1],
            JSValue::UNDEFINED,
            1,
            &mut err,
        );
        JS_FreeValue(ctx, err);
        JS_FreeValue(ctx, res2);
        JS_FreeValue(ctx, resolving_funcs[0]);
        JS_FreeValue(ctx, resolving_funcs[1]);
    }

    promise.into()
}

pub unsafe fn js_iterator_proto_iterator(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    this_val.dup()
}

pub const js_async_iterator_proto_funcs: [JSCFunctionListEntry; 1] = [JS_CFUNC_DEF!(
    c"[Symbol.asyncIterator]",
    0,
    js_iterator_proto_iterator
)];

pub const js_async_generator_proto_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_MAGIC_DEF!(c"next", 1, js_async_generator_next, GEN_MAGIC_NEXT as _),
    JS_CFUNC_MAGIC_DEF!(c"return", 1, js_async_generator_next, GEN_MAGIC_RETURN as _),
    JS_CFUNC_MAGIC_DEF!(c"throw", 1, js_async_generator_next, GEN_MAGIC_THROW as _),
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
