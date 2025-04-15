use std::ptr::NonNull;

use crate::{
    atom::consts::JS_ATOM_empty_string,
    context::JSContext,
    error::JSResult,
    function::{js_function_set_properties, JSCFunctionDataRecord, JS_CallConstructor},
    gc::{JS_MarkFunc, JS_MarkValue},
    js_core::promise::js_promise_constructor,
    list_entry,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{list_add_tail, list_del, list_foreach, list_foreach_safe, list_head},
    value::{
        object::{JS_GetOpaque, JS_NewObjectProtoClass, JS_SetOpaque},
        set_value, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT,
    },
    JS_CLASS_C_FUNCTION_DATA, JS_CLASS_PROMISE, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
};

use super::job::promise_reaction_job;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSPromiseStateEnum {
    JS_PROMISE_PENDING,
    JS_PROMISE_FULFILLED,
    JS_PROMISE_REJECTED,
}

pub struct JSPromiseData {
    pub promise_state: JSPromiseStateEnum,

    /// 0=fulfill, 1=reject, list of JSPromiseReactionData.link
    pub promise_reactions: [list_head; 2],

    /// Note: only useful to debug
    pub is_handled: bool,
    pub promise_result: JSValue,
}

pub struct JSPromiseFunctionDataResolved {
    pub ref_count: i32,
    pub already_resolved: bool,
}

pub struct JSPromiseFunctionData {
    pub promise: JSValue,
    pub presolved: NonNull<JSPromiseFunctionDataResolved>,
}

pub struct JSPromiseReactionData {
    // not used in promise_reaction_job
    pub link: list_head,

    pub resolving_funcs: [JSValue; 2],
    pub handler: JSValue,
}

impl JSContext {
    pub(super) fn create_promise_executor(&mut self) -> JSResult {
        self.create_function_with_data(
            js_promise_executor,
            2,
            0,
            &[JSValue::UNDEFINED, JSValue::UNDEFINED],
        )
    }

    /// create Promise.
    /// returns Promise object and [resolve, reject] functions
    pub unsafe fn create_promise(
        &mut self,
        ctor: JSValueConst,
    ) -> Result<(JSValue, [JSValue; 2]), i32> {
        let mut executor = self.create_promise_executor()?;

        let result_promise = if ctor.is_undefined() {
            js_promise_constructor(self, ctor, 1, &mut executor)
        } else {
            JS_CallConstructor(self, ctor, 1, &mut executor)
        };
        if result_promise.is_exception() {
            JS_FreeValue(self, executor);
            JS_FreeValue(self, result_promise);
            return Err(-1);
        }

        let s = JS_GetOpaque(executor, JS_CLASS_C_FUNCTION_DATA).cast::<JSCFunctionDataRecord>();
        debug_assert!(!s.is_null());

        let cdata = (*s).data();

        for i in 0..2 {
            if cdata[i].ensure_callable(self).is_err() {
                JS_FreeValue(self, executor);
                JS_FreeValue(self, result_promise);
                return Err(-1);
            }
        }

        let resolving_funcs = [cdata[0].dup(), cdata[1].dup()];
        JS_FreeValue(self, executor);

        Ok((result_promise, resolving_funcs))
    }
}

#[deprecated(note = "use JSContext::create_promise_executor() instead")]
unsafe fn js_promise_executor_new(ctx: *mut JSContext) -> JSValue {
    unreachable!("use JSContext::create_promise_executor() instead");
    // let func_data = [JSValue::UNDEFINED; 2];
    // JS_NewCFunctionData(ctx, js_promise_executor, 2, 0, 2, func_data.as_ptr())
}

unsafe fn js_promise_executor(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue {
    for i in 0..2 {
        if !(*func_data.add(i)).is_undefined() {
            return (*ctx).JS_ThrowTypeError(c"resolving function already set");
        }
        *func_data.add(i) = (*argv.add(i)).dup();
    }
    JSValue::UNDEFINED
}

pub unsafe fn JS_PromiseState(
    ctx: *const JSContext,
    promise: JSValue,
) -> Result<JSPromiseStateEnum, i32> {
    let s = JS_GetOpaque(promise, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    if s.is_null() {
        return Err(-1);
    }
    Ok((*s).promise_state)
}

pub unsafe fn JS_PromiseResult(ctx: *mut JSContext, promise: JSValue) -> JSValue {
    let s = JS_GetOpaque(promise, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    if s.is_null() {
        return JSValue::UNDEFINED;
    }
    (*s).promise_result.dup()
}

#[deprecated(note = "use JSContext::create_promise() instead")]
pub unsafe fn JS_NewPromiseCapability(
    ctx: *mut JSContext,
    resolving_funcs: *mut JSValue,
) -> JSValue {
    if let Ok((promise, rr)) = (*ctx).create_promise(JSValue::UNDEFINED) {
        *resolving_funcs.add(0) = rr[0];
        *resolving_funcs.add(1) = rr[1];
        promise
    } else {
        JSValue::EXCEPTION
    }
}

#[deprecated(note = "use JSContext::create_promise() instead")]
pub unsafe fn js_new_promise_capability(
    ctx: *mut JSContext,
    resolving_funcs: *mut JSValue,
    ctor: JSValueConst,
) -> JSValue {
    unreachable!("use JSContext::create_promise() instead")
}

/// create and return [resolve, reject] functions for `promise` object.
/// Note: `promise` is not consumed.
pub unsafe fn js_create_resolving_functions(
    ctx: &mut JSContext,
    promise: JSValueConst,
) -> Result<[JSValue; 2], i32> {
    let sr = Box::new(JSPromiseFunctionDataResolved {
        ref_count: 1,
        already_resolved: false, // must be shared between the two functions
    });

    let mut sr_ptr = NonNull::new_unchecked(Box::into_raw(sr));

    // [resolve, reject]
    let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    for i in 0..2 {
        let obj = JS_NewObjectProtoClass(
            ctx,
            ctx.function_proto,
            JS_CLASS_PROMISE_RESOLVE_FUNCTION + i,
        );

        if obj.is_exception() {
            if i != 0 {
                JS_FreeValue(ctx, resolving_funcs[0]);
            }
            js_promise_resolve_function_free_resolved(sr_ptr.as_ptr());
            return Err(-1);
        }

        sr_ptr.as_mut().ref_count += 1;

        let s = Box::new(JSPromiseFunctionData {
            promise: promise.dup(),
            presolved: sr_ptr,
        });

        JS_SetOpaque(obj, Box::into_raw(s).cast()); // leak ownership of JSPromiseFunctionData
        js_function_set_properties(ctx, obj, JS_ATOM_empty_string, 1);
        resolving_funcs[i as usize] = obj;
    }

    js_promise_resolve_function_free_resolved(sr_ptr.as_ptr());

    Ok(resolving_funcs)
}

/// Note: `value` is duplicated inside.
pub unsafe fn fulfill_or_reject_promise(
    ctx: *mut JSContext,
    promise: JSValueConst,
    value: JSValueConst,
    is_reject: bool,
) {
    let s = JS_GetOpaque(promise, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    debug_assert!(!s.is_null());
    debug_assert_eq!((*s).promise_state, JSPromiseStateEnum::JS_PROMISE_PENDING);

    let s = s.as_mut().unwrap();

    #[cfg(DUMP_PROMISE)]
    {
        println!("fulfill_or_reject_promise: is_reject={}", is_reject);
    }

    set_value(ctx, &mut s.promise_result, value.dup());
    s.promise_state = if is_reject {
        JSPromiseStateEnum::JS_PROMISE_REJECTED
    } else {
        JSPromiseStateEnum::JS_PROMISE_FULFILLED
    };

    if s.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !s.is_handled {
        let rt = (*ctx).rt_mut();
        if let Some(cb) = rt.host_promise_rejection_tracker {
            cb(
                ctx,
                promise,
                value,
                0,
                rt.host_promise_rejection_tracker_opaque,
            );
        }
    }

    list_foreach_safe(&mut s.promise_reactions[is_reject as usize], |el| {
        let rd = list_entry!(el, JSPromiseReactionData, link);
        (*ctx)
            .enqueue_job(
                promise_reaction_job,
                &[
                    (*rd).resolving_funcs[0],
                    (*rd).resolving_funcs[1],
                    (*rd).handler,
                    if is_reject {
                        JSValue::TRUE
                    } else {
                        JSValue::FALSE
                    },
                    value,
                ],
            )
            .unwrap();

        list_del(&mut (*rd).link);
        promise_reaction_data_free((*ctx).rt_mut(), rd);
    });
}

unsafe fn promise_reaction_data_free(rt: *mut JSRuntime, rd: *mut JSPromiseReactionData) {
    JS_FreeValueRT(rt, (*rd).resolving_funcs[0]);
    JS_FreeValueRT(rt, (*rd).resolving_funcs[1]);
    JS_FreeValueRT(rt, (*rd).handler);
    drop(Box::from_raw(rd));
}

#[inline(always)]
pub unsafe fn reject_promise(ctx: *mut JSContext, promise: JSValueConst, value: JSValueConst) {
    fulfill_or_reject_promise(ctx, promise, value, true);
}

pub unsafe fn perform_promise_then(
    ctx: &mut JSContext,
    promise: JSValueConst,
    resolve_reject: &[JSValue; 2],
    cap_resolving_funcs: &[JSValue; 2],
) -> Result<(), i32> {
    let s = JS_GetOpaque(promise, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    let mut rd_array: [*mut JSPromiseReactionData; 2] = [std::ptr::null_mut(); 2];

    for i in 0..2 {
        let mut rd = if let Some(p) = js_malloc_typed::<JSPromiseReactionData>(ctx, 1, true) {
            Box::from_raw(p.as_ptr())
        } else {
            if i == 1 {
                promise_reaction_data_free(ctx.rt_mut(), rd_array[0]);
            }
            return Err(-1);
        };

        rd.resolving_funcs[0] = cap_resolving_funcs[0].dup();
        rd.resolving_funcs[1] = cap_resolving_funcs[1].dup();

        let mut handler = resolve_reject[i];
        if !handler.is_function(ctx) {
            handler = JSValue::UNDEFINED;
        }

        rd.handler = handler.dup();
        rd_array[i] = Box::into_raw(rd);
    }

    if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_PENDING {
        for i in 0..2 {
            list_add_tail(
                &mut (*rd_array[i as usize]).link,
                &mut (*s).promise_reactions[i as usize],
            );
        }
    } else {
        let mut args: [JSValueConst; 5] = [JSValue::UNDEFINED; 5];

        if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !(*s).is_handled {
            if let Some(cb) = (*ctx).rt().host_promise_rejection_tracker {
                cb(
                    ctx,
                    promise,
                    (*s).promise_result,
                    true as i32,
                    (*ctx).rt().host_promise_rejection_tracker_opaque,
                );
            }
        }

        let i = if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
            0 // resolve
        } else {
            1 // reject
        };
        let rd = rd_array[i as usize];
        let args: [JSValue; 5] = [
            (*rd).resolving_funcs[0],
            (*rd).resolving_funcs[1],
            (*rd).handler,
            if i == 0 {
                JSValue::FALSE
            } else {
                JSValue::TRUE
            },
            (*s).promise_result,
        ];
        (*ctx).enqueue_job(promise_reaction_job, &args).unwrap();

        promise_reaction_data_free(ctx.rt_mut(), rd_array[0]);
        promise_reaction_data_free(ctx.rt_mut(), rd_array[1]);
    }

    (*s).is_handled = true;
    Ok(())
}

pub unsafe fn js_promise_resolve_function_free_resolved(sr: *mut JSPromiseFunctionDataResolved) {
    debug_assert!((*sr).ref_count > 0);
    (*sr).ref_count -= 1;
    if (*sr).ref_count == 0 {
        drop(Box::from_raw(sr));
    }
}

pub unsafe fn js_promise_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let s = JS_GetOpaque(val, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    if s.is_null() {
        return;
    }

    for i in 0..2 {
        list_foreach_safe(&mut (*s).promise_reactions[i], |el| {
            let rd = list_entry!(el, JSPromiseReactionData, link);
            promise_reaction_data_free(rt, rd);
        });
    }

    JS_FreeValueRT(rt, (*s).promise_result);
    drop(Box::from_raw(s));
}

pub unsafe fn js_promise_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let s = JS_GetOpaque(val, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    if s.is_null() {
        return;
    }

    for i in 0..2 {
        list_foreach(&mut (*s).promise_reactions[i], |el| {
            let rd = list_entry!(el, JSPromiseReactionData, link);
            JS_MarkValue(rt, (*rd).resolving_funcs[0], mark_func);
            JS_MarkValue(rt, (*rd).resolving_funcs[1], mark_func);
            JS_MarkValue(rt, (*rd).handler, mark_func);
        });
    }

    JS_MarkValue(rt, (*s).promise_result, mark_func);
}

pub unsafe fn js_promise_resolve_function_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let s = val.object().u.promise_function_data;
    if !s.is_null() {
        js_promise_resolve_function_free_resolved((*s).presolved.as_ptr());
        JS_FreeValueRT(rt, (*s).promise);
        drop(Box::from_raw(s));
    }
}

pub unsafe fn js_promise_resolve_function_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    let s = val.object().u.promise_function_data;
    if !s.is_null() {
        JS_MarkValue(rt, (*s).promise, mark_func);
    }
}
