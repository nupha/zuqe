use std::{ops::DerefMut, ptr::NonNull};

use crate::{
    atom::consts::*,
    callable::JSCFunctionDataRecord,
    class::js_create_from_ctor,
    context::JSContext,
    error::JSResult,
    function::{js_function_set_properties, CallArgs, JS_Call, JS_CallConstructor},
    gc::{JS_MarkFunc, JS_MarkValue},
    js_core::error::js_aggregate_error_constructor,
    list_entry,
    mem::js_malloc_typed,
    promise::JSPromiseReactionData,
    runtime::JSRuntime,
    types::{init_list_head, list_del, list_foreach, list_foreach_safe, list_head},
    value::{
        object::{JS_NewObject, JS_NewObjectProtoClass, JS_PROP_C_W_E},
        set_value, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_NewString, JS_ToBool,
        JS_ToInt32,
    },
    JS_CLASS_C_FUNCTION_DATA, JS_CLASS_PROMISE, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
};

use super::{
    job::promise_reaction_job, js_promise_resolve_function_free_resolved, JSPromiseFunctionData,
    JSPromiseFunctionDataResolved,
};

pub type JSHostPromiseRejectionTracker = unsafe fn(
    ctx: *mut JSContext,
    promise: PromiseValue,
    reason: JSValue,
    is_handled: ::core::ffi::c_int,
    opaque: *mut ::core::ffi::c_void,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSPromiseStateEnum {
    JS_PROMISE_PENDING,
    JS_PROMISE_FULFILLED,
    JS_PROMISE_REJECTED,
}

pub struct JSPromiseData {
    pub promise_state: JSPromiseStateEnum,

    /// 0=fulfill, 1=reject, list of JSPromiseReactionData.link.
    /// TODO: use [Vec<Box<JSPromiseReactionData>>; 2]
    pub promise_reactions: [list_head; 2],

    /// Note: only useful to debug
    pub is_handled: bool,

    pub promise_result: JSValue,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct PromiseValue(pub(super) JSValue);

impl From<PromiseValue> for JSValue {
    #[inline(always)]
    fn from(value: PromiseValue) -> Self {
        value.0
    }
}

impl PromiseValue {
    /// create new Promise object with given executor function.
    /// `new_target` - the 'new' operator target.
    /// Note:  `executor` is not consumed.
    pub(super) unsafe fn new_with_executor(
        ctx: &mut JSContext,
        executor: JSValue,
        new_target: JSValue,
    ) -> Result<Self, i32> {
        debug_assert_eq!(
            executor.object_unchecked().class_id(),
            JS_CLASS_C_FUNCTION_DATA
        );
        executor.ensure_callable(ctx)?;

        // make promise payload
        let mut s = Box::from_raw(
            js_malloc_typed::<JSPromiseData>(ctx, 1, true)
                .ok_or(-1)?
                .as_ptr(),
        );
        s.promise_state = JSPromiseStateEnum::JS_PROMISE_PENDING;
        s.is_handled = false;
        s.promise_result = JSValue::UNDEFINED;
        init_list_head(&mut s.promise_reactions[0]);
        init_list_head(&mut s.promise_reactions[1]);

        // create js object of promise class
        let pr_obj = js_create_from_ctor(ctx, new_target, JS_CLASS_PROMISE).check_exception()?;
        pr_obj
            .object_unchecked()
            .set_opaque(Box::into_raw(s).cast());

        let promise = Self(pr_obj);

        let mut args = promise.create_resolving_functions(ctx).map_err(|e| {
            JS_FreeValue(ctx, promise.0);
            e
        })?;

        let ret = JS_Call(ctx, executor, JSValue::UNDEFINED, 2, args.as_mut_ptr());
        if ret.is_exception() {
            let mut error = ctx.get_exception();
            let ret2 = JS_Call(ctx, args[1], JSValue::UNDEFINED, 1, &mut error);
            JS_FreeValue(ctx, error);
            if ret2.is_exception() {
                JS_FreeValue(ctx, args[0]);
                JS_FreeValue(ctx, args[1]);
                JS_FreeValue(ctx, promise.0);
                return Err(-1);
            }
            JS_FreeValue(ctx, ret2);
        }

        JS_FreeValue(ctx, ret);
        JS_FreeValue(ctx, args[0]);
        JS_FreeValue(ctx, args[1]);

        Ok(promise)
    }

    /// create new Promise object and its resolving function pairs.
    /// returns Promise object and [resolve, reject] functions.
    /// `ctor` is a customized constructor function value to instantiate Promise object:
    ///  - when provised, will call this function to create customized Promise object,
    ///  - when it is `undefined`, creates default Promise object.
    pub unsafe fn new(
        ctx: &mut JSContext,
        ctor: JSValue,
    ) -> Result<(PromiseValue, [JSValue; 2]), i32> {
        let mut executor = ctx.create_native_callable_object(
            PromiseValue::hook_executor,
            2,
            0,
            &[JSValue::UNDEFINED, JSValue::UNDEFINED], // resolving functions, initially undefined
        )?;

        let result_promise = if ctor.is_undefined() {
            Self::new_with_executor(ctx, executor, JSValue::UNDEFINED)
                .map_or(JSValue::EXCEPTION, |p| p.0)
        } else {
            JS_CallConstructor(ctx, ctor, 1, &mut executor)
        };
        if result_promise.is_exception() {
            JS_FreeValue(ctx, executor);
            JS_FreeValue(ctx, result_promise);
            return Err(-1);
        }

        let s = executor
            .object_unchecked()
            .opaque::<JSCFunctionDataRecord>(JS_CLASS_C_FUNCTION_DATA)
            .unwrap();

        let data = s.as_ref().data();
        debug_assert!(!data.is_empty());

        for i in 0..2 {
            if data[i].ensure_callable(ctx).is_err() {
                JS_FreeValue(ctx, executor);
                JS_FreeValue(ctx, result_promise);
                return Err(-1);
            }
        }

        let resolving_funcs = [data[0].dup(), data[1].dup()];
        JS_FreeValue(ctx, executor);

        Ok((result_promise.as_promise_unchecked(), resolving_funcs))
    }

    #[inline(always)]
    pub fn as_value(self) -> JSValue {
        unsafe { std::mem::transmute(self.0) }
    }

    #[inline(always)]
    pub fn dup(self) -> Self {
        Self(self.0.dup())
    }

    /// create [resolve, reject] function pairs for `self` promise object.
    /// Note: `self` is not consumed.
    pub(super) fn create_resolving_functions(
        self,
        ctx: &mut JSContext,
    ) -> Result<[JSValue; 2], i32> {
        let sr = Box::new(JSPromiseFunctionDataResolved {
            ref_count: 1,
            already_resolved: false, // must be shared between the two functions
        });

        let mut sr_ptr = unsafe { NonNull::new_unchecked(Box::into_raw(sr)) };

        // [resolve, reject]
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        for i in 0..2 {
            let func_obj = unsafe {
                JS_NewObjectProtoClass(
                    ctx,
                    ctx.function_proto,
                    JS_CLASS_PROMISE_RESOLVE_FUNCTION + i,
                )
            };

            if func_obj.is_exception() {
                unsafe {
                    if i != 0 {
                        JS_FreeValue(ctx, resolving_funcs[0]);
                    }

                    // consume sr_ptr
                    js_promise_resolve_function_free_resolved(sr_ptr.as_ptr());
                }
                return Err(-1);
            }

            unsafe {
                sr_ptr.as_mut().ref_count += 1;
            }

            let s = Box::new(JSPromiseFunctionData {
                promise: self.dup(),
                presolved: sr_ptr,
            });

            unsafe {
                func_obj
                    .object_unchecked()
                    .set_opaque(Box::into_raw(s).cast()); // <-- leak Box<JSPromiseFunctionData>

                js_function_set_properties(ctx, func_obj, JS_ATOM_empty_string, 1);
            }

            resolving_funcs[i as usize] = func_obj;
        }

        unsafe {
            js_promise_resolve_function_free_resolved(sr_ptr.as_ptr());
        }

        Ok(resolving_funcs)
    }

    /// get Promise current state.
    pub fn state(self) -> Result<JSPromiseStateEnum, i32> {
        unsafe {
            self.0
                .object_unchecked()
                .opaque::<JSPromiseData>(JS_CLASS_PROMISE)
                .map(|x| x.as_ref().promise_state)
                .ok_or(-1)
        }
    }

    /// get Promise current result.
    /// returned value is duplicated, caller must free it.
    pub fn result(self) -> JSValue {
        unsafe {
            self.0
                .object_unchecked()
                .opaque::<JSPromiseData>(JS_CLASS_PROMISE)
                .map_or(JSValue::UNDEFINED, |s| s.as_ref().promise_result.dup())
        }
    }

    /// save resolving resultand update promise status to 'fulfilled' or 'rejected'.
    /// call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    /// Note: `value` is not consumed (duplicated inside).
    pub(super) fn fulfill_or_reject(self, ctx: &mut JSContext, value: JSValue, is_reject: bool) {
        let s = unsafe {
            self.0
                .object_unchecked()
                .opaque::<JSPromiseData>(JS_CLASS_PROMISE)
                .unwrap()
                .as_mut()
        };
        debug_assert_eq!(s.promise_state, JSPromiseStateEnum::JS_PROMISE_PENDING);

        #[cfg(DUMP_PROMISE)]
        {
            println!("fulfill_or_reject_promise: is_reject={}", is_reject);
        }

        unsafe {
            set_value(ctx, &mut s.promise_result, value.dup());
        }

        s.promise_state = if is_reject {
            JSPromiseStateEnum::JS_PROMISE_REJECTED
        } else {
            JSPromiseStateEnum::JS_PROMISE_FULFILLED
        };

        if s.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !s.is_handled {
            if let Some(cb) = ctx.rt().host_promise_rejection_tracker {
                unsafe {
                    cb(
                        ctx,
                        self,
                        value,
                        0,
                        ctx.rt().host_promise_rejection_tracker_opaque,
                    );
                }
            }
        }

        // call chained reactions registered by `then()`, `catch()` or `finally()`
        unsafe {
            list_foreach_safe(&mut s.promise_reactions[is_reject as usize], |el| {
                let rd = list_entry!(el, JSPromiseReactionData, link);

                ctx.enqueue_job(
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
                Box::from_raw(rd).free(ctx.rt_mut());
            });
        }
    }
}

impl JSValue {
    /// interpret value `self` as PromiseValue.
    /// returns None if `self` is not a JS_CLASS_PROMISE class object
    pub fn as_promise(self) -> Option<PromiseValue> {
        if self.is_object() && unsafe { self.object_unchecked().class_id() } == JS_CLASS_PROMISE {
            unsafe { Some(std::mem::transmute(self)) }
        } else {
            None
        }
    }

    /// interpret value `self` as PromiseValue.
    /// must ensure `self` is a JS_CLASS_PROMISE class object, otherwise behavior is undefined.
    #[inline(always)]
    pub unsafe fn as_promise_unchecked(self) -> PromiseValue {
        debug_assert!(self.is_object());
        debug_assert_eq!(self.object_unchecked().class_id(), JS_CLASS_PROMISE);
        std::mem::transmute(self)
    }
}

impl JSContext {
    /// create Promise object and its resolving function paris.
    /// returns Promise object and [resolve, reject] functions.
    /// this is just a shortcut for `PromiseValue::new(ctx, ctor)`.
    #[inline(always)]
    pub fn create_promise(
        &mut self,
        ctor: JSValueConst,
    ) -> Result<(PromiseValue, [JSValue; 2]), i32> {
        unsafe { PromiseValue::new(self, ctor) }
    }
}

//
// Hooks for Promise
//
impl PromiseValue {
    /// the native callback for Promise constructor function
    pub unsafe fn hook_constructor(
        ctx: *mut JSContext,
        new_target: JSValueConst,
        argc: i32,
        argv: *mut JSValueConst,
    ) -> JSValue {
        let executor = *argv.offset(0);
        PromiseValue::new_with_executor(ctx.as_mut().unwrap(), executor, new_target)
            .map_or(JSValue::EXCEPTION, |p| p.0)
    }

    /// callback to execute a promise:
    /// setting resolve and reject functions as attaching data of 'executor'.
    /// arg#0: resolve function, arg#1: reject function
    fn hook_executor(ctx: &mut JSContext, mut args: CallArgs) -> JSResult {
        for i in 0..2 {
            if !args.data_raw(i).is_undefined() {
                ctx.JS_ThrowTypeError(c"resolving function already set");
                return Err(-1);
            } else {
                let _ = args.replace_data(i, args.get_raw(i).dup());
            }
        }
        Ok(JSValue::UNDEFINED)
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        if let Some(mut s) = val
            .object_unchecked()
            .opaque::<JSPromiseData>(JS_CLASS_PROMISE)
        {
            for i in 0..2 {
                list_foreach_safe(&mut s.as_mut().promise_reactions[i], |el| {
                    let rd = list_entry!(el, JSPromiseReactionData, link);
                    Box::from_raw(rd).free(rt);
                });
            }
            JS_FreeValueRT(rt, s.as_mut().promise_result);
            drop(Box::from_raw(s.as_ptr()));
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        if let Some(mut s) = val
            .object_unchecked()
            .opaque::<JSPromiseData>(JS_CLASS_PROMISE)
        {
            for i in 0..2 {
                list_foreach(&mut s.as_mut().promise_reactions[i], |el| {
                    let rd = list_entry!(el, JSPromiseReactionData, link);
                    JS_MarkValue(rt, (*rd).resolving_funcs[0], mark_func);
                    JS_MarkValue(rt, (*rd).resolving_funcs[1], mark_func);
                    JS_MarkValue(rt, (*rd).handler, mark_func);
                });
            }

            JS_MarkValue(rt, s.as_mut().promise_result, mark_func);
        }
    }
}

pub const PROMISE_MAGIC_all: i32 = 0;
pub const PROMISE_MAGIC_allSettled: i32 = 1;
pub const PROMISE_MAGIC_any: i32 = 2;

pub unsafe fn remainingElementsCount_add(
    ctx: &mut JSContext,
    resolve_element_env: JSValue,
    addend: i32,
) -> Result<i32, i32> {
    let val = resolve_element_env.get_property_u32(ctx, 0);
    if val.is_exception() {
        return Err(-1);
    }

    let mut remainingElementsCount = val.coerce_to_i32_free(ctx)?;
    remainingElementsCount += addend;
    resolve_element_env.set_property_u32(ctx, 0, JSValue::from_i32(remainingElementsCount))?;

    Ok(if remainingElementsCount == 0 { 1 } else { 0 })
}

pub fn js_promise_all_resolve_element(ctx: &mut JSContext, mut args: CallArgs) -> JSResult {
    let resolve_type = args.magic & 3;
    let is_reject = args.magic & 4;

    let alreadyCalled = unsafe { JS_ToBool(ctx, args.data_raw(0))? };
    let mut values = args.data_raw(2);
    let resolve = args.data_raw(3);
    let resolve_element_env = args.data_raw(4);
    let index = unsafe { JS_ToInt32(ctx, args.data_raw(1))? };

    if alreadyCalled {
        return Ok(JSValue::UNDEFINED);
    }

    args.replace_data(0, JSValue::TRUE);

    let obj: JSValue = if resolve_type == PROMISE_MAGIC_allSettled as i32 {
        unsafe {
            let ob = JS_NewObject(ctx).check_exception()?.into_rc(ctx.rt());

            let str = JS_NewString(
                ctx,
                if is_reject != 0 {
                    c"rejected"
                } else {
                    c"fulfilled"
                },
            )
            .check_exception()?;

            ob.define_property_value(ctx, JS_ATOM_status, str, JS_PROP_C_W_E)?;
            ob.define_property_value(
                ctx,
                if is_reject != 0 {
                    JS_ATOM_reason
                } else {
                    JS_ATOM_value
                },
                args.get_raw(0).dup(),
                JS_PROP_C_W_E,
            )?;

            ob.into()
        }
    } else {
        args.get(0).dup()
    };

    values.define_property_value_u32(ctx, index as u32, obj, JS_PROP_C_W_E)?;

    match unsafe { remainingElementsCount_add(ctx, resolve_element_env, -1)? } {
        1 => {
            if resolve_type == PROMISE_MAGIC_any as i32 {
                let mut error = js_aggregate_error_constructor(ctx, values)
                    .check_exception()?
                    .into_rc(ctx.rt());

                unsafe {
                    JS_Call(ctx, resolve, JSValue::UNDEFINED, 1, error.deref_mut())
                        .check_exception()?
                        .into_rc((*ctx).rt())
                }
            } else {
                unsafe {
                    JS_Call(ctx, resolve, JSValue::UNDEFINED, 1, &mut values)
                        .check_exception()?
                        .into_rc((*ctx).rt())
                }
            };
        }
        0 => {}
        _ => {
            unreachable!()
        }
    }

    Ok(JSValue::UNDEFINED)
}
