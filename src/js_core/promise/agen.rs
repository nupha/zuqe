use std::ops::DerefMut;

use crate::{
    atom::consts::{JS_ATOM_return, JS_ATOM_throw},
    class::js_create_from_ctor,
    context::JSContext,
    function::{
        async_func_free, async_func_init, async_func_resume, js_create_iterator_result,
        JSCFunctionListEntry, JS_Call, JS_NewCFunctionData,
    },
    generator::{
        JSAsyncGeneratorData, JSAsyncGeneratorRequest, JSAsyncGeneratorStateEnum, FUNC_RET_AWAIT,
        FUNC_RET_YIELD, FUNC_RET_YIELD_STAR,
    },
    iterator::{JSAsyncFromSyncIteratorData, JS_IteratorGetCompleteValue, JS_IteratorNext2},
    list_entry,
    mem::js_malloc_typed,
    promise::perform_promise_then,
    runtime::JSRuntime,
    types::{init_list_head, list_add_tail, list_del, list_empty, list_head},
    value::{
        object::{JS_GetOpaque, JS_SetOpaque, JS_PROP_CONFIGURABLE},
        JSTag, JSValue, JSValueConst, JSValueRc, JS_FreeValue, JS_ToBool, JS_TAG_INT,
        JS_VALUE_GET_INT, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_GENERATOR,
    JS_MKPTR, JS_PROP_STRING_DEF,
};

use super::js_promise_resolve;

pub unsafe fn js_async_generator_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
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

    s.generator = JS_VALUE_GET_OBJ(obj);
    JS_SetOpaque(obj, Box::into_raw(s).cast());

    obj
}

#[deprecated(note = "use JSAsyncGeneratorData::free() instead")]
pub unsafe fn js_async_generator_free(rt: *mut JSRuntime, s: *mut JSAsyncGeneratorData) {
    unreachable!("use JSAsyncGeneratorData::free() instead");
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

const GEN_MAGIC_NEXT: i32 = 0;
const GEN_MAGIC_RETURN: i32 = 1;
const GEN_MAGIC_THROW: i32 = 2;

pub unsafe fn js_async_from_sync_iterator_unwrap(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue {
    js_create_iterator_result(ctx, (*argv).dup(), JS_ToBool(ctx, *func_data).unwrap())
}

pub unsafe fn js_async_from_sync_iterator_unwrap_func_create(
    ctx: &mut JSContext,
    done: bool,
) -> JSValue {
    JS_NewCFunctionData(
        ctx,
        js_async_from_sync_iterator_unwrap,
        1,
        0,
        1,
        [if done { JSValue::TRUE } else { JSValue::FALSE }].as_mut_ptr(),
    )
}

pub unsafe fn js_async_from_sync_iterator_next(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let done_resolve =
        |rr: [JSValueRc; 2], mut err: JSValue, is_reject: usize, promise: JSValueRc| -> JSValue {
            let res2 = JS_Call(
                ctx.as_mut().unwrap(),
                *rr[is_reject],
                JSValue::UNDEFINED,
                1,
                &mut err,
            );
            JS_FreeValue(ctx, err);
            JS_FreeValue(ctx, res2);
            return promise.into();
        };

    let (promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(JSValue::UNDEFINED)
    {
        (
            pr.into_rc((*ctx).rt()),
            [rr[0].into_rc((*ctx).rt()), rr[1].into_rc((*ctx).rt())],
        )
    } else {
        return JSValue::EXCEPTION;
    };

    let s = JS_GetOpaque(this_val, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR)
        .cast::<JSAsyncFromSyncIteratorData>();
    if s.is_null() {
        (*ctx).JS_ThrowTypeError(c"not an Async-from-Sync Iterator");
        let err = (*ctx).get_exception();
        return done_resolve(resolving_funcs, err, 1, promise);
    }

    let method: JSValueRc;
    if magic == GEN_MAGIC_NEXT {
        method = (*s).next_method.dup().into_rc((*ctx).rt());
    } else {
        method = (*s)
            .sync_iter
            .get_property(
                ctx.as_mut().unwrap(),
                if magic == GEN_MAGIC_RETURN {
                    JS_ATOM_return
                } else {
                    JS_ATOM_throw
                },
            )
            .into_rc((*ctx).rt());

        if method.is_exception() {
            let err = (*ctx).get_exception();
            return done_resolve(resolving_funcs, err, 1, promise);
        }

        if method.is_undefined_or_null() {
            let err: JSValue;
            let is_reject: usize;
            if magic == GEN_MAGIC_RETURN {
                err = js_create_iterator_result(ctx, (*argv.offset(0)).dup(), true);
                is_reject = 0;
            } else {
                err = (*argv.offset(0)).dup();
                is_reject = 1;
            }
            return done_resolve(resolving_funcs, err, is_reject, promise);
        }
    }

    let mut done = 0;
    let mut value = JS_IteratorNext2(
        ctx,
        (*s).sync_iter,
        *method,
        if argc >= 1 { 1 } else { 0 },
        argv,
        &mut done,
    )
    .into_rc((*ctx).rt());
    if value.is_exception() {
        let err = (*ctx).get_exception();
        return done_resolve(resolving_funcs, err, 1, promise);
    }

    if done == 2 {
        let obj = value;
        let mut _done = false;
        value = JS_IteratorGetCompleteValue(ctx, *obj, &mut _done).into_rc((*ctx).rt());
        if value.is_exception() {
            let err = (*ctx).get_exception();
            return done_resolve(resolving_funcs, err, 1, promise);
        }
        done = _done as i32;
    }

    if value.is_exception() {
        let err = (*ctx).get_exception();
        return done_resolve(resolving_funcs, err, 1, promise);
    }

    let value_wrapper_promise =
        js_promise_resolve(ctx, (*ctx).promise_ctor, 1, value.deref_mut(), 0).into_rc((*ctx).rt());
    if value_wrapper_promise.is_exception() {
        let err = (*ctx).get_exception();
        return done_resolve(resolving_funcs, err, 1, promise);
    }

    let resolve_reject: [JSValueRc; 2] = [
        {
            let f =
                js_async_from_sync_iterator_unwrap_func_create(ctx.as_mut().unwrap(), done != 0)
                    .into_rc((*ctx).rt());
            if f.is_exception() {
                return JSValue::EXCEPTION;
            }
            f
        },
        JSValueRc::undefined((*ctx).rt()),
    ];

    let res = perform_promise_then(
        ctx.as_mut().unwrap(),
        *value_wrapper_promise,
        &[*resolve_reject[0], *resolve_reject[1]],
        &[*resolving_funcs[0], *resolving_funcs[1]],
    );
    if res.is_err() {
        return JSValue::EXCEPTION;
    }

    promise.into()
}

pub const js_async_from_sync_iterator_proto_funcs: [JSCFunctionListEntry; 3] = [
    JS_CFUNC_MAGIC_DEF!(
        c"next",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_NEXT as _
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"return",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_RETURN as _
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"throw",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_THROW as _
    ),
];

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
                        );
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
        (*ctx).JS_ThrowTypeError(c"not an AsyncGenerator object");
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
    Box::into_raw(req); // leak

    if (*s).state != JSAsyncGeneratorStateEnum::JS_ASYNC_GENERATOR_STATE_EXECUTING {
        js_async_generator_resume_next(ctx, s);
    }

    return promise;
}

pub const js_async_generator_proto_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_MAGIC_DEF!(c"next", 1, js_async_generator_next, GEN_MAGIC_NEXT as _),
    JS_CFUNC_MAGIC_DEF!(c"return", 1, js_async_generator_next, GEN_MAGIC_RETURN as _),
    JS_CFUNC_MAGIC_DEF!(c"throw", 1, js_async_generator_next, GEN_MAGIC_THROW as _),
    JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"AsyncGenerator",
        JS_PROP_CONFIGURABLE
    ),
];

pub const js_async_generator_function_proto_funcs: [JSCFunctionListEntry; 1] =
    [JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"AsyncGeneratorFunction",
        JS_PROP_CONFIGURABLE
    )];
