use std::{ops::DerefMut, ptr::NonNull};

use crate::{
    array::JS_NewArray,
    atom::consts::*,
    class::js_create_from_ctor,
    context::JSContext,
    error::JSResult,
    function::{
        js_function_set_properties, JSCFunctionDataRecord, JS_Call, JS_CallConstructor, JS_Invoke,
        JS_InvokeFree, JS_NewCFunctionData,
    },
    gc::{JS_MarkFunc, JS_MarkValue},
    iterator::{JS_GetIterator, JS_IteratorClose, JS_IteratorNext},
    js_core::error::js_aggregate_error_constructor,
    list_entry,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{init_list_head, list_add_tail, list_del, list_foreach, list_foreach_safe, list_head},
    value::{
        object::{
            JS_DefinePropertyValue, JS_DefinePropertyValueUint32, JS_GetOpaque, JS_GetOpaque2,
            JS_NewObject, JS_NewObjectProtoClass, JS_SetOpaque, JS_SpeciesConstructor,
            JS_PROP_C_W_E,
        },
        set_value, JSValue, JSValueConst, JSValueRc, JS_FreeValue, JS_FreeValueRT, JS_NewString,
        JS_ToBool, JS_ToInt32,
    },
    JS_CLASS_C_FUNCTION_DATA, JS_CLASS_PROMISE, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
};

use super::{
    job::promise_reaction_job, js_promise_resolve, js_promise_resolve_function_free_resolved,
    JSPromiseFunctionData, JSPromiseFunctionDataResolved,
};

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

pub struct JSPromiseReactionData {
    // not used in promise_reaction_job
    pub link: list_head,

    pub resolving_funcs: [JSValue; 2],
    pub handler: JSValue,
}

#[derive(Debug, Clone, Copy)]
pub struct PromiseValue(JSValue);

impl PromiseValue {
    /// create a Promise object with executor function.
    /// `new_target` - the 'new' operator target.
    /// Note:  `executor` is not consumed.
    pub(super) unsafe fn new_with_executor(
        ctx: &mut JSContext,
        executor: JSValue,
        new_target: JSValue,
    ) -> Result<Self, i32> {
        debug_assert_eq!(executor.object().class_id(), JS_CLASS_C_FUNCTION_DATA);
        executor.ensure_callable(ctx)?;

        let obj = js_create_from_ctor(ctx, new_target, JS_CLASS_PROMISE).check_exception()?;

        if let Some(p) = js_malloc_typed::<JSPromiseData>(ctx, 1, true) {
            let mut s = Box::from_raw(p.as_ptr());

            s.promise_state = JSPromiseStateEnum::JS_PROMISE_PENDING;
            s.is_handled = false;
            s.promise_result = JSValue::UNDEFINED;
            init_list_head(&mut s.promise_reactions[0]);
            init_list_head(&mut s.promise_reactions[1]);

            JS_SetOpaque(obj, Box::into_raw(s).cast());
        } else {
            JS_FreeValue(ctx, obj);
            return Err(-1);
        };

        let promise = Self(obj);

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
    pub unsafe fn new(ctx: &mut JSContext, ctor: JSValue) -> Result<(JSValue, [JSValue; 2]), i32> {
        // create executor function
        let mut executor = ctx.create_cfunction_with_data(
            PromiseValue::hook_executor,
            2,
            0,
            &[JSValue::UNDEFINED, JSValue::UNDEFINED],
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

        let s = JS_GetOpaque(executor, JS_CLASS_C_FUNCTION_DATA).cast::<JSCFunctionDataRecord>();
        debug_assert!(!s.is_null());

        let cdata = (*s).data();

        for i in 0..2 {
            if cdata[i].ensure_callable(ctx).is_err() {
                JS_FreeValue(ctx, executor);
                JS_FreeValue(ctx, result_promise);
                return Err(-1);
            }
        }

        let resolving_funcs = [cdata[0].dup(), cdata[1].dup()];
        JS_FreeValue(ctx, executor);

        Ok((result_promise, resolving_funcs))
    }

    /// create and return [resolve, reject] functions for `self` promise object.
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
            let obj = unsafe {
                JS_NewObjectProtoClass(
                    ctx,
                    ctx.function_proto,
                    JS_CLASS_PROMISE_RESOLVE_FUNCTION + i,
                )
            };

            if obj.is_exception() {
                if i != 0 {
                    unsafe {
                        JS_FreeValue(ctx, resolving_funcs[0]);
                    }
                }
                unsafe {
                    js_promise_resolve_function_free_resolved(sr_ptr.as_ptr());
                }
                return Err(-1);
            }

            unsafe {
                sr_ptr.as_mut().ref_count += 1;
            }

            let s = Box::new(JSPromiseFunctionData {
                promise: self.0.dup(),
                presolved: sr_ptr,
            });

            unsafe {
                JS_SetOpaque(obj, Box::into_raw(s).cast()); // leak ownership of JSPromiseFunctionData
                js_function_set_properties(ctx, obj, JS_ATOM_empty_string, 1);
            }

            resolving_funcs[i as usize] = obj;
        }

        unsafe {
            js_promise_resolve_function_free_resolved(sr_ptr.as_ptr());
        }

        Ok(resolving_funcs)
    }
}

// Hooks
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

    /// callback to execute a promise
    unsafe fn hook_executor(
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

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
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

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
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
}

impl JSValue {
    pub unsafe fn to_promise(self) -> PromiseValue {
        debug_assert!(self.is_object());
        debug_assert_eq!(self.object().class_id(), JS_CLASS_PROMISE);

        PromiseValue(self)
    }
}

impl JSContext {
    /// create Promise object and its resolving function paris.
    /// returns Promise object and [resolve, reject] functions.
    /// this is just a shortcut for `PromiseValue::new(ctx, ctor)`.
    #[inline(always)]
    pub unsafe fn create_promise(
        &mut self,
        ctor: JSValueConst,
    ) -> Result<(JSValue, [JSValue; 2]), i32> {
        PromiseValue::new(self, ctor)
    }
}

#[deprecated(note = "unused")]
unsafe fn js_promise_executor_new(ctx: *mut JSContext) -> JSValue {
    unreachable!("unused");
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

    let mut remainingElementsCount = val.to_i32_free(ctx)?;
    remainingElementsCount += addend;
    resolve_element_env.set_property_u32(ctx, 0, JSValue::from_i32(remainingElementsCount))?;

    Ok(if remainingElementsCount == 0 { 1 } else { 0 })
}

pub unsafe fn js_promise_all(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }
    let is_promise_any = magic == PROMISE_MAGIC_any as i32;

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(this_val) {
        (
            pr.into_rc((*ctx).rt()),
            [rr[0].into_rc((*ctx).rt()), rr[1].into_rc((*ctx).rt())],
        )
    } else {
        return JSValue::EXCEPTION;
    };

    let fail_reject = |promise: JSValueRc| -> JSValue {
        let mut error = (*ctx).get_exception().into_rc((*ctx).rt());
        let ret = JS_Call(
            ctx.as_mut().unwrap(),
            *resolving_funcs[1],
            JSValue::UNDEFINED,
            1,
            error.deref_mut(),
        )
        .into_rc((*ctx).rt());
        return if ret.is_exception() {
            JSValue::EXCEPTION
        } else {
            promise.into()
        };
    };

    let promise_resolve = this_val
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_resolve)
        .into_rc((*ctx).rt());

    if promise_resolve.is_exception()
        || promise_resolve
            .ensure_callable(ctx.as_mut().unwrap())
            .is_err()
    {
        return fail_reject(promise_resolve);
    }

    let iter = JS_GetIterator(ctx, *argv.offset(0), false).into_rc((*ctx).rt());
    if iter.is_exception() {
        return fail_reject(promise_resolve);
    }

    let next_method = iter
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_next)
        .into_rc((*ctx).rt());
    if next_method.is_exception() {
        return fail_reject(promise_resolve);
    };

    let mut values = JS_NewArray(ctx).into_rc((*ctx).rt());
    if values.is_exception() {
        return fail_reject(promise_resolve);
    }

    let resolve_element_env = JS_NewArray(ctx).into_rc((*ctx).rt());
    if resolve_element_env.is_exception() {
        return fail_reject(promise_resolve);
    } else if resolve_element_env
        .define_property_value_u32(ctx.as_mut().unwrap(), 0, JSValue::from_i32(1), 0x7)
        .is_err()
    {
        return fail_reject(promise_resolve);
    }

    let mut index = 0;

    loop {
        let mut done = false;
        let item = JS_IteratorNext(ctx, *iter, *next_method, 0, std::ptr::null_mut(), &mut done)
            .into_rc((*ctx).rt());

        if item.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        if done {
            break;
        }

        let next_promise = JS_Call(
            ctx.as_mut().unwrap(),
            *promise_resolve,
            this_val,
            1,
            [*item].as_mut_ptr(),
        )
        .into_rc((*ctx).rt());
        if next_promise.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        let resolve_element_data = [
            JSValue::FALSE,
            JSValue::from_i32(index),
            *values,
            *resolving_funcs[is_promise_any as usize],
            *resolve_element_env,
        ];

        let mut resolve_element = JS_NewCFunctionData(
            ctx,
            js_promise_all_resolve_element,
            1,
            magic,
            5,
            resolve_element_data.as_ptr(),
        )
        .into_rc((*ctx).rt());
        if resolve_element.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        let reject_element: JSValueRc = match magic {
            PROMISE_MAGIC_allSettled => {
                let r = JS_NewCFunctionData(
                    ctx,
                    js_promise_all_resolve_element,
                    1,
                    magic | 4,
                    5,
                    resolve_element_data.as_ptr(),
                )
                .into_rc((*ctx).rt());
                if r.is_exception() {
                    JS_IteratorClose(ctx, *iter, true);
                    return fail_reject(promise_resolve);
                }
                r
            }
            PROMISE_MAGIC_any => {
                if values
                    .define_property_value_u32(
                        ctx.as_mut().unwrap(),
                        index as u32,
                        JSValue::UNDEFINED,
                        0x7,
                    )
                    .is_err()
                {
                    JS_IteratorClose(ctx, *iter, true);
                    return fail_reject(promise_resolve);
                }
                std::mem::replace(&mut resolve_element, resolving_funcs[0].clone())
            }
            _ => resolving_funcs[1].clone(),
        };

        if remainingElementsCount_add(ctx.as_mut().unwrap(), *resolve_element_env, 1).is_err() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        let ret = JS_Invoke(
            ctx.as_mut().unwrap(),
            *next_promise,
            JS_ATOM_then,
            2,
            [*resolve_element, *reject_element].as_mut_ptr(),
        )
        .into_rc((*ctx).rt());
        if ret.is_exception() {
            JS_IteratorClose(ctx, *iter, true);
            return fail_reject(promise_resolve);
        }

        index += 1;
    } // loop

    match remainingElementsCount_add(ctx.as_mut().unwrap(), *resolve_element_env, -1) {
        Ok(1) => {
            if magic == PROMISE_MAGIC_any as i32 {
                let error = js_aggregate_error_constructor(ctx.as_mut().unwrap(), *values)
                    .into_rc((*ctx).rt());
                if error.is_exception() {
                    return fail_reject(promise_resolve);
                }
                values = error;
            }

            let ret = JS_Call(
                ctx.as_mut().unwrap(),
                *resolving_funcs[is_promise_any as usize],
                JSValue::UNDEFINED,
                1,
                values.deref_mut(),
            )
            .into_rc((*ctx).rt());

            if ret.is_exception() {
                return fail_reject(promise_resolve);
            }
        }
        Ok(0) => {}
        Err(_) => {
            return fail_reject(promise_resolve);
        }
        Ok(_) => {
            unreachable!()
        }
    }

    result_promise.into()
}

pub unsafe fn js_promise_race(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(this_val) {
        (
            pr.into_rc((*ctx).rt()),
            [rr[0].into_rc((*ctx).rt()), rr[1].into_rc((*ctx).rt())],
        )
    } else {
        return JSValue::EXCEPTION;
    };

    let fail_reject = |promise: JSValueRc| -> JSValue {
        let mut error = (*ctx).get_exception().into_rc((*ctx).rt());
        let ret = JS_Call(
            ctx.as_mut().unwrap(),
            *resolving_funcs[1],
            JSValue::UNDEFINED,
            1,
            error.deref_mut(),
        )
        .into_rc((*ctx).rt());
        return if ret.is_exception() {
            JSValue::EXCEPTION
        } else {
            promise.into()
        };
    };

    let promise_resolve = this_val
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_resolve)
        .into_rc((*ctx).rt());

    if promise_resolve.is_exception()
        || promise_resolve
            .ensure_callable(ctx.as_mut().unwrap())
            .is_err()
    {
        return fail_reject(promise_resolve);
    }

    let iter = JS_GetIterator(ctx, *argv.offset(0), false).into_rc((*ctx).rt());
    if iter.is_exception() {
        return fail_reject(promise_resolve);
    }

    let next_method = iter
        .get_property(ctx.as_mut().unwrap(), JS_ATOM_next)
        .into_rc((*ctx).rt());
    if next_method.is_exception() {
        return fail_reject(promise_resolve);
    }

    loop {
        let mut done = false;
        let mut item =
            JS_IteratorNext(ctx, *iter, *next_method, 0, std::ptr::null_mut(), &mut done)
                .into_rc((*ctx).rt());

        if item.is_exception() {
            return fail_reject(promise_resolve);
        }
        if done {
            break;
        }

        let next_promise = JS_Call(
            ctx.as_mut().unwrap(),
            *promise_resolve,
            this_val,
            1,
            item.deref_mut(),
        )
        .into_rc((*ctx).rt());
        if next_promise.is_exception() {
            return fail_reject(promise_resolve);
        }

        let ret = JS_Invoke(
            ctx.as_mut().unwrap(),
            *next_promise,
            JS_ATOM_then,
            2,
            [*resolving_funcs[0], *resolving_funcs[1]].as_mut_ptr(),
        )
        .into_rc((*ctx).rt());
        if ret.is_exception() {
            return fail_reject(promise_resolve);
        }
    }

    result_promise.into()
}

pub unsafe fn js_promise_all_resolve_element(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue {
    let resolve_type = magic & 3;
    let is_reject = magic & 4;
    let alreadyCalled = JS_ToBool(ctx, *func_data.offset(0)).unwrap();
    let mut values = *func_data.offset(2);
    let resolve = *func_data.offset(3);
    let resolve_element_env = *func_data.offset(4);
    let index = if let Ok(n) = JS_ToInt32(ctx, *func_data.offset(1)) {
        n
    } else {
        return JSValue::EXCEPTION;
    };

    if alreadyCalled {
        return JSValue::UNDEFINED;
    }

    *func_data.offset(0) = JSValue::TRUE;

    let obj: JSValue;

    if resolve_type == PROMISE_MAGIC_allSettled as i32 {
        obj = JS_NewObject(ctx);
        if obj.is_exception() {
            return JSValue::EXCEPTION;
        }

        let str = JS_NewString(
            ctx,
            if is_reject != 0 {
                c"rejected"
            } else {
                c"fulfilled"
            },
        );
        if str.is_exception() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if JS_DefinePropertyValue(ctx, obj, JS_ATOM_status, str, JS_PROP_C_W_E) < 0 {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if JS_DefinePropertyValue(
            ctx,
            obj,
            if is_reject != 0 {
                JS_ATOM_reason
            } else {
                JS_ATOM_value
            },
            (*argv.offset(0)).dup(),
            JS_PROP_C_W_E,
        ) < 0
        {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    } else {
        obj = (*argv.offset(0)).dup();
    }

    if JS_DefinePropertyValueUint32(ctx, values, index as u32, obj, JS_PROP_C_W_E) < 0 {
        return JSValue::EXCEPTION;
    }

    match remainingElementsCount_add(ctx.as_mut().unwrap(), resolve_element_env, -1) {
        Ok(1) => {
            let ret: JSValueRc;

            if resolve_type == PROMISE_MAGIC_any as i32 {
                let mut error = js_aggregate_error_constructor(ctx.as_mut().unwrap(), values);
                if error.is_exception() {
                    return JSValue::EXCEPTION;
                }
                ret = JS_Call(
                    ctx.as_mut().unwrap(),
                    resolve,
                    JSValue::UNDEFINED,
                    1,
                    &mut error,
                )
                .into_rc((*ctx).rt());
                JS_FreeValue(ctx, error);
            } else {
                ret = JS_Call(
                    ctx.as_mut().unwrap(),
                    resolve,
                    JSValue::UNDEFINED,
                    1,
                    &mut values,
                )
                .into_rc((*ctx).rt());
            }
            if ret.is_exception() {
                return JSValue::EXCEPTION;
            }
        }
        Ok(0) => {}
        Err(_) => {
            return JSValue::EXCEPTION;
        }
        Ok(_) => {
            unreachable!()
        }
    }

    JSValue::UNDEFINED
}

pub(super) unsafe fn promise_reaction_data_free(
    rt: *mut JSRuntime,
    rd: *mut JSPromiseReactionData,
) {
    JS_FreeValueRT(rt, (*rd).resolving_funcs[0]);
    JS_FreeValueRT(rt, (*rd).resolving_funcs[1]);
    JS_FreeValueRT(rt, (*rd).handler);
    drop(Box::from_raw(rd));
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

pub unsafe fn js_promise_withResolvers(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if !this_val.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(this_val) {
        (pr, rr)
    } else {
        return JSValue::EXCEPTION;
    };

    let obj = JS_NewObject(ctx);
    if obj.is_exception() {
        JS_FreeValue(ctx, resolving_funcs[0]);
        JS_FreeValue(ctx, resolving_funcs[1]);
        JS_FreeValue(ctx, result_promise);
        return JSValue::EXCEPTION;
    }

    obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_promise,
        result_promise,
        JS_PROP_C_W_E,
    )
    .unwrap();

    obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_resolve,
        resolving_funcs[0],
        JS_PROP_C_W_E,
    )
    .unwrap();

    obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_reject,
        resolving_funcs[1],
        JS_PROP_C_W_E,
    )
    .unwrap();

    obj
}

pub unsafe fn js_promise_then(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let s = JS_GetOpaque2(ctx, this_val, JS_CLASS_PROMISE).cast::<JSPromiseData>();
    if s.is_null() {
        return JSValue::EXCEPTION;
    }

    let ctor = JS_SpeciesConstructor(ctx, this_val, JSValue::UNDEFINED).into_rc((*ctx).rt());
    if ctor.is_exception() {
        return JSValue::EXCEPTION;
    }

    let (result_promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(*ctor) {
        (pr, rr)
    } else {
        return JSValue::EXCEPTION;
    };

    let ret = perform_promise_then(
        ctx.as_mut().unwrap(),
        this_val,
        &[*argv.add(0), *argv.add(1)],
        &resolving_funcs,
    );
    JS_FreeValue(ctx, resolving_funcs[0]);
    JS_FreeValue(ctx, resolving_funcs[1]);

    if ret.is_err() {
        JS_FreeValue(ctx, result_promise);
        JSValue::EXCEPTION
    } else {
        result_promise
    }
}

pub unsafe fn js_promise_catch(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_Invoke(
        ctx.as_mut().unwrap(),
        this_val,
        JS_ATOM_then,
        2,
        [JSValue::UNDEFINED, JSValue::UNDEFINED].as_mut_ptr(),
    )
}

pub unsafe fn js_promise_finally_value_thunk(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue {
    (*func_data.offset(0)).dup()
}

pub unsafe fn js_promise_finally_thrower(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue {
    (*ctx).JS_Throw((*func_data.offset(0)).dup())
}

pub unsafe fn js_promise_then_finally_func(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue {
    let ctor = *func_data.offset(0);
    let onFinally = *func_data.offset(1);

    let mut res = JS_Call(
        ctx.as_mut().unwrap(),
        onFinally,
        JSValue::UNDEFINED,
        0,
        std::ptr::null_mut(),
    );
    if res.is_exception() {
        return JSValue::EXCEPTION;
    }

    let promise = js_promise_resolve(ctx, ctor, 1, &mut res, 0);
    JS_FreeValue(ctx, res);
    if promise.is_exception() {
        return promise;
    }

    let mut then_func = if magic == 0 {
        JS_NewCFunctionData(ctx, js_promise_finally_value_thunk, 0, 0, 1, argv)
    } else {
        JS_NewCFunctionData(ctx, js_promise_finally_thrower, 0, 0, 1, argv)
    };
    if then_func.is_exception() {
        JS_FreeValue(ctx, promise);
        return then_func;
    }

    let ret = JS_InvokeFree(
        ctx.as_mut().unwrap(),
        promise,
        JS_ATOM_then,
        1,
        &mut then_func,
    );
    JS_FreeValue(ctx, then_func);

    ret
}

pub unsafe fn js_promise_finally(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let onFinally = *argv.offset(0);

    let ctor = JS_SpeciesConstructor(ctx, this_val, JSValue::UNDEFINED);
    if ctor.is_exception() {
        return ctor;
    }

    let mut then_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];

    if !onFinally.is_function(ctx.as_ref().unwrap()) {
        then_funcs = [onFinally.dup(), onFinally.dup()];
    } else {
        let func_data = [ctor, onFinally];
        for i in 0..2 {
            if let Ok(f) = (*ctx).create_cfunction_with_data(
                js_promise_then_finally_func,
                1,
                i as _,
                &func_data,
            ) {
                then_funcs[i] = f;
            } else {
                if i != 0 {
                    JS_FreeValue(ctx, then_funcs[0]);
                }
                JS_FreeValue(ctx, ctor);
                return JSValue::EXCEPTION;
            }
        }
    }

    JS_FreeValue(ctx, ctor);

    let ret = JS_Invoke(
        ctx.as_mut().unwrap(),
        this_val,
        JS_ATOM_then,
        2,
        then_funcs.as_mut_ptr(),
    );
    JS_FreeValue(ctx, then_funcs[0]);
    JS_FreeValue(ctx, then_funcs[1]);

    ret
}
