use std::ops::DerefMut;

use crate::{
    class::js_create_from_ctor,
    context::JSContext,
    function::{
        async_func_free, async_func_init, async_func_resume, js_create_iterator_result,
        JSAsyncFunctionState, JSCFunctionListEntry, JS_Call, JS_NewCFunctionData,
    },
    gc::{JS_MarkFunc, JS_MarkValue},
    iterator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW},
    js_core::promise::js_promise_resolve,
    list_entry,
    mem::js_malloc_typed,
    promise::perform_promise_then,
    runtime::JSRuntime,
    types::{
        init_list_head, list_add_tail, list_del, list_empty, list_foreach, list_foreach_safe,
        list_head,
    },
    value::{
        object::{JSObject, JS_GetOpaque, JS_SetOpaque, JS_PROP_CONFIGURABLE},
        JSTag, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_TAG_INT, JS_TAG_OBJECT,
        JS_VALUE_GET_INT, JS_VALUE_GET_TAG,
    },
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CLASS_ASYNC_GENERATOR, JS_MKPTR, JS_PROP_STRING_DEF,
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
    /// completion
    pub completion_type: i32, // GEN_MAGIC_x
    pub result: JSValue,

    /// promise capability
    pub promise: JSValue,
    pub resolving_funcs: [JSValue; 2],
}

impl JSAsyncGeneratorData {
    pub fn free(mut self: Box<Self>, rt: &mut JSRuntime) {
        unsafe {
            list_foreach_safe(&mut self.queue, |el| {
                let req = list_entry!(el, JSAsyncGeneratorRequest, link);

                JS_FreeValueRT(rt, (*req).result);
                JS_FreeValueRT(rt, (*req).promise);
                JS_FreeValueRT(rt, (*req).resolving_funcs[0]);
                JS_FreeValueRT(rt, (*req).resolving_funcs[1]);

                todo!("free req");
                //js_free_rt(rt, req as *mut _);
            });

            if !self.func_state.is_null() {
                async_func_free(rt, self.func_state);
            }
        }
    }
}

/// An AsyncGeneratorValue value
#[derive(Debug, Copy, Clone)]
pub struct AsyncGeneratorValue(JSValue);

impl JSValue {
    /// convert value to AsyncGeneratorValue value.
    /// you must ensure that `value` is an AsyncGeneratorValue object.
    pub unsafe fn to_async_generator_unchecked(self) -> AsyncGeneratorValue {
        debug_assert_eq!(JS_VALUE_GET_TAG(self), JS_TAG_OBJECT);
        debug_assert_eq!(self.object().class_id(), JS_CLASS_ASYNC_GENERATOR);
        AsyncGeneratorValue(self)
    }
}

impl AsyncGeneratorValue {
    pub unsafe fn call(
        ctx: &mut JSContext,
        func_obj: JSValue,
        this_obj: JSValue,
        argc: i32,
        argv: *const JSValue,
        flags: i32,
    ) -> JSValue {
        let mut s = if let Some(p) = js_malloc_typed::<JSAsyncGeneratorData>(ctx, 1, true) {
            Box::from_raw(p.as_ptr())
        } else {
            return JSValue::EXCEPTION;
        };

        s.state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START;
        init_list_head(&mut s.queue);
        s.func_state = async_func_init(ctx, func_obj, this_obj, argc, argv);
        if s.func_state.is_null() {
            s.free(ctx.rt_mut());
            return JSValue::EXCEPTION;
        }

        // execute the function up to 'OP_initial_yield' (no yield nor await are possible)
        let func_ret = async_func_resume(ctx, s.func_state);
        if func_ret.is_exception() {
            s.free(ctx.rt_mut());
            return JSValue::EXCEPTION;
        }
        JS_FreeValue(ctx, func_ret);

        let obj = js_create_from_ctor(ctx, func_obj, JS_CLASS_ASYNC_GENERATOR);
        if obj.is_exception() {
            s.free(ctx.rt_mut());
            return JSValue::EXCEPTION;
        }

        s.generator = obj.object();
        JS_SetOpaque(obj, Box::into_raw(s).cast());

        obj
    }

    pub unsafe fn finalizer(rt: *mut JSRuntime, val: JSValue) {
        let s = JS_GetOpaque(val, JS_CLASS_ASYNC_GENERATOR).cast::<JSAsyncGeneratorData>();
        if !s.is_null() {
            Box::from_raw(s).free(rt.as_mut().unwrap());
        }
    }

    pub unsafe fn mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        let s = JS_GetOpaque(val, JS_CLASS_ASYNC_GENERATOR).cast::<JSAsyncGeneratorData>();

        if !s.is_null() {
            list_foreach(&mut (*s).queue, |el| {
                let req = list_entry!(el, JSAsyncGeneratorRequest, link);
                JS_MarkValue(rt, (*req).result, mark_func);
                JS_MarkValue(rt, (*req).promise, mark_func);
                JS_MarkValue(rt, (*req).resolving_funcs[0], mark_func);
                JS_MarkValue(rt, (*req).resolving_funcs[1], mark_func);
            });

            if !(*s).func_state.is_null() {
                mark_func(rt, &mut (*(*s).func_state).header);
            }
        }
    }
}

#[deprecated(note = "use JSAsyncGeneratorData::free() instead")]
pub unsafe fn js_async_generator_free(rt: *mut JSRuntime, s: *mut JSAsyncGeneratorData) {
    unreachable!("use JSAsyncGeneratorData::free() instead");
}

#[deprecated(note = "use AsyncGeneratorValue::function_call() instead")]
pub unsafe fn js_async_generator_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    unreachable!("use AsyncGeneratorValue::function_call() instead");
}

pub unsafe fn js_async_generator_resume_next(ctx: *mut JSContext, s: *mut JSAsyncGeneratorData) {
    enum State {
        ResumeExec,
        Done,
    }

    loop {
        let state: State = loop {
            if list_empty(&mut (*s).queue) {
                break State::Done;
            }

            let next = list_entry!((*s).queue.next, JSAsyncGeneratorRequest, link);

            match (*s).state {
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING => {
                    // only happens when restarting execution after await()
                    break State::ResumeExec;
                }
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN => {
                    break State::Done;
                }
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_START => {
                    if (*next).completion_type == GEN_MAGIC_NEXT {
                        (*(*s).func_state).throw_flag = false;
                        (*s).state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                        break State::ResumeExec;
                    } else {
                        js_async_generator_complete(ctx, s);
                    }
                }
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED => {
                    if (*next).completion_type == GEN_MAGIC_NEXT {
                        js_async_generator_resolve(ctx, s, JSValue::UNDEFINED, true);
                    } else if (*next).completion_type == GEN_MAGIC_RETURN {
                        (*s).state =
                            JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN;
                        js_async_generator_completed_return(
                            ctx.as_mut().unwrap(),
                            s,
                            (*next).result,
                        )
                        .unwrap();
                    } else {
                        js_async_generator_reject(ctx, s, (*next).result);
                    }
                    break State::Done;
                }
                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                | JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR => {
                    let value = (*next).result.dup();
                    if (*next).completion_type == GEN_MAGIC_THROW
                        && (*s).state
                            == JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                    {
                        (*ctx).JS_Throw(value);
                        (*(*s).func_state).throw_flag = true;
                    } else {
                        // 'yield' returns a value. 'yield *' also returns a value
                        // in case the 'throw' method is called
                        *(*(*s).func_state)
                            .frame
                            .stack_current_ptr()
                            .offset(-1)
                            .as_mut() = value;
                        *(*(*s).func_state).frame.stack_current_ptr().as_mut() =
                            JSValue::from_i32((*next).completion_type);
                        (*(*s).func_state).frame.cur_sp_offset += 1;
                        (*(*s).func_state).throw_flag = false;
                    }
                    (*s).state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING;
                    break State::ResumeExec;
                }
            }
        }; // inner loop

        match state {
            State::Done => {
                return;
            }
            State::ResumeExec => loop {
                let func_ret = async_func_resume(ctx.as_mut().unwrap(), (*s).func_state);
                if (*(*s).func_state).is_completed {
                    if func_ret.is_exception() {
                        let value = (*ctx).get_exception();
                        js_async_generator_complete(ctx, s);
                        js_async_generator_reject(ctx, s, value);
                        JS_FreeValue(ctx, value);
                    } else {
                        // end of function
                        js_async_generator_complete(ctx, s);
                        js_async_generator_resolve(ctx, s, func_ret, true);
                        JS_FreeValue(ctx, func_ret);
                    }
                } else {
                    debug_assert_eq!(JS_VALUE_GET_TAG(func_ret), JS_TAG_INT);
                    let func_ret_code = JS_VALUE_GET_INT(func_ret);
                    let value = (*(*s).func_state)
                        .frame
                        .stack_current_ptr()
                        .offset(-1)
                        .as_mut()
                        .replace(JSValue::UNDEFINED);

                    match func_ret_code {
                        FUNC_RET_YIELD | FUNC_RET_YIELD_STAR => {
                            (*s).state = if func_ret_code == FUNC_RET_YIELD_STAR {
                                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD_STAR
                            } else {
                                JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_SUSPENDED_YIELD
                            };
                            js_async_generator_resolve(ctx, s, value, false);
                            JS_FreeValue(ctx, value);
                        }
                        FUNC_RET_AWAIT => {
                            let ret = js_async_generator_await(ctx.as_mut().unwrap(), s, value);
                            JS_FreeValue(ctx, value);
                            if ret.is_err() {
                                // exception: throw it
                                (*(*s).func_state).throw_flag = true;
                                continue; // State::ResumeExec loop
                            } else {
                                return;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            },
        } // state match
    } // outer loop
}

pub unsafe fn js_async_generator_complete(ctx: *mut JSContext, s: *mut JSAsyncGeneratorData) {
    if (*s).state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED {
        (*s).state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED;
        async_func_free((*ctx).rt_mut(), (*s).func_state);
        (*s).func_state = std::ptr::null_mut();
    }
}

/// `value` is not consumed.
pub unsafe fn js_async_generator_resolve(
    ctx: *mut JSContext,
    s: *mut JSAsyncGeneratorData,
    value: JSValueConst,
    done: bool,
) {
    let result = js_create_iterator_result(ctx, value.dup(), done);
    /* XXX: better exception handling ? */
    js_async_generator_resolve_or_reject(ctx, s, result, false);
    JS_FreeValue(ctx, result);
}

#[inline(always)]
pub unsafe fn js_async_generator_reject(
    ctx: *mut JSContext,
    s: *mut JSAsyncGeneratorData,
    exception: JSValueConst,
) {
    js_async_generator_resolve_or_reject(ctx, s, exception, true)
}

pub unsafe fn js_async_generator_resolve_or_reject(
    ctx: *mut JSContext,
    s: *mut JSAsyncGeneratorData,
    mut result: JSValueConst,
    is_reject: bool,
) {
    let next = list_entry!((*s).queue.next, JSAsyncGeneratorRequest, link);
    list_del(&mut (*next).link);

    let ret = JS_Call(
        ctx.as_mut().unwrap(),
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

pub unsafe fn js_async_generator_completed_return(
    ctx: &mut JSContext,
    s: *mut JSAsyncGeneratorData,
    mut value: JSValueConst,
) -> Result<(), i32> {
    // Can fail looking up JS_ATOM_constructor when is_reject==0.
    let mut promise = js_promise_resolve(
        ctx,
        ctx.promise_ctor,
        1,
        &mut value,
        0, // is_reject
    )
    .into_rc(ctx.rt());

    // A poisoned .constructor property is observable and the resulting
    // exception should be delivered to the catch handler.
    if promise.is_exception() {
        let mut err = (*ctx).get_exception().into_rc(ctx.rt());
        promise = js_promise_resolve(
            ctx,
            ctx.promise_ctor,
            1,
            err.deref_mut(),
            1, // is_reject
        )
        .check_exception()?
        .into_rc(ctx.rt());
    }

    let resolving_funcs1 = js_async_generator_resolve_function_create(
        ctx,
        JS_MKPTR!(JSTag::Object, (*s).generator.cast()),
        true,
    )?;

    let res = perform_promise_then(
        ctx,
        *promise,
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
    mut value: JSValueConst,
) -> Result<(), i32> {
    let promise = js_promise_resolve(ctx, ctx.promise_ctor, 1, &mut value, 0)
        .check_exception()?
        .into_rc((*ctx).rt());

    let resolving_funcs = js_async_generator_resolve_function_create(
        ctx,
        JS_MKPTR!(JSTag::Object, (*s).generator.cast()),
        false,
    )?;

    let res = perform_promise_then(
        ctx,
        *promise,
        &resolving_funcs,
        &[JSValue::UNDEFINED, JSValue::UNDEFINED], // no need to create 'thrownawayCapability' as in the spec
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
        let func = JS_NewCFunctionData(
            ctx,
            js_async_generator_resolve_function,
            1,
            i + if is_resume_next { 2 } else { 0 },
            1,
            &generator,
        );
        if func.is_exception() {
            JS_FreeValue(ctx, resolving_funcs[0]);
            JS_FreeValue(ctx, resolving_funcs[1]);
            return Err(-1);
        }

        resolving_funcs[i as usize] = func;
    }

    Ok(resolving_funcs)
}

pub unsafe fn js_async_generator_resolve_function(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue {
    let is_reject = (magic & 1) != 0;
    let arg = *argv.offset(0);
    let s =
        JS_GetOpaque(*func_data.offset(0), JS_CLASS_ASYNC_GENERATOR).cast::<JSAsyncGeneratorData>();

    debug_assert!(!s.is_null());

    if magic >= 2 {
        // resume next case in AWAITING_RETURN state
        debug_assert!(matches!(
            (*s).state,
            JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_AWAITING_RETURN
                | JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED
        ));

        (*s).state = JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_COMPLETED;
        if is_reject {
            js_async_generator_reject(ctx, s, arg);
        } else {
            js_async_generator_resolve(ctx, s, arg, true);
        }
    } else {
        // restart function execution after await()
        debug_assert_eq!(
            (*s).state,
            JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING
        );

        (*(*s).func_state).throw_flag = is_reject;
        if is_reject {
            (*ctx).JS_Throw(arg.dup());
        } else {
            // return value of await
            *(*(*s).func_state)
                .frame
                .stack_current_ptr()
                .offset(-1)
                .as_mut() = arg.dup();
        }

        js_async_generator_resume_next(ctx, s);
    }

    JSValue::UNDEFINED
}

pub unsafe fn js_async_generator_next(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let s = JS_GetOpaque(this_val, JS_CLASS_ASYNC_GENERATOR).cast::<JSAsyncGeneratorData>();

    let (promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(JSValue::UNDEFINED)
    {
        (pr, rr)
    } else {
        return JSValue::EXCEPTION;
    };

    if s.is_null() {
        (*ctx).JS_ThrowTypeError(c"not an AsyncGeneratorValue object");
        let mut err = (*ctx).get_exception();
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
        return promise;
    }

    let mut req = Box::new(JSAsyncGeneratorRequest {
        link: list_head::default(),
        completion_type: magic,
        result: (*argv.offset(0)).dup(),
        promise: promise.dup(),
        resolving_funcs,
    });
    // // malloc fail
    // if req.is_null() {
    //     JS_FreeValue(ctx, resolving_funcs[0]);
    //     JS_FreeValue(ctx, resolving_funcs[1]);
    //     JS_FreeValue(ctx, promise);
    //     return JSValue::EXCEPTION;
    // }

    list_add_tail(&mut req.link, &mut (*s).queue);
    let _ = Box::into_raw(req); // leak

    if (*s).state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING {
        js_async_generator_resume_next(ctx, s);
    }

    promise
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
