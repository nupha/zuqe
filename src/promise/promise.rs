use std::{
    cell::Cell,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use crate::{
    JS_CLASS_PROMISE,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_Call, JS_CallConstructor, js_function_set_properties},
    gc::GCMarkFunc,
    js_core::error::js_aggregate_error_constructor,
    list_entry,
    promise::JSPromiseReactionData,
    runtime::JSRuntime,
    utils::{
        UncheckedFrom, UncheckedInto, init_list_head, list_del, list_foreach, list_foreach_safe,
        list_head,
    },
    value::{
        JS_NewString, JS_ToInt32, JSValue, JSValueConst,
        object::{JS_PROP_C_W_E, JSObjectIsa},
        set_value,
    },
};

use super::{JSPromiseFunctionData, job::promise_reaction_job};

pub type JSHostPromiseRejectionTracker = fn(
    ctx: &JSContext,
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

#[derive(Debug)]
pub struct JSPromiseData {
    pub promise_state: JSPromiseStateEnum,

    /// list of JSPromiseReactionData.link: #0 for fulfill, #1 for reject.
    pub promise_reactions: [Vec<Box<JSPromiseReactionData>>; 2],

    /// Note: will be freed in PromiseValue::hook_finalizer().
    pub promise_result: JSValue,

    /// Note: only useful to debug
    pub is_handled: bool,
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct PromiseValue(pub(super) JSValue);

impl TryFrom<JSValue> for PromiseValue {
    type Error = i32;

    /// interpret value as PromiseValue.
    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if value.is_object() && unsafe { value.object_unchecked().class_id() } == JS_CLASS_PROMISE {
            Ok(unsafe { std::mem::transmute::<JSValue, PromiseValue>(value) })
        } else {
            Err(-1)
        }
    }
}

impl UncheckedFrom<JSValue> for PromiseValue {
    #[inline(always)]
    unsafe fn unchecked_from(value: JSValue) -> Self {
        debug_assert!(value.is_object());
        debug_assert_eq!(value.object_unchecked().class_id(), JS_CLASS_PROMISE);
        std::mem::transmute(value)
    }
}

impl From<PromiseValue> for JSValue {
    #[inline(always)]
    fn from(value: PromiseValue) -> Self {
        value.0
    }
}

impl PromiseValue {
    /// create new Promise object with given executor function.
    /// `new_target` - the 'new' operator target.
    /// Note:  `executor` is not consumed, and must be callable.
    pub(super) fn new_with_executor(
        ctx: &JSContext,
        executor: JSValue,
        new_target: &JSValue,
    ) -> Result<Self, i32> {
        // debug_assert_eq!(
        //     executor.object_unchecked().class_id(),
        //     JS_CLASS_C_FUNCTION_DATA | JS_CLASS_BYTECODE_FUNCTION
        // );

        executor.ensure_callable(ctx)?;

        // make promise payload
        let s = Box::new(JSPromiseData {
            promise_state: JSPromiseStateEnum::JS_PROMISE_PENDING,
            promise_reactions: [Vec::new(), Vec::new()],
            promise_result: JSValue::UNDEFINED,
            is_handled: false,
        });

        // create js object of promise class
        let promise = Self(
            ctx.create_instance_from_ctor(new_target, JSObjectIsa::PROMISE(s))
                .map_err(|_| -1)?,
        );

        let args = promise.create_resolving_functions(ctx)?;
        let ret = JS_Call(ctx, &executor, &JSValue::UNDEFINED, &args);
        if ret.is_exception() {
            let error = ctx.take_exception();
            let ret2 = JS_Call(ctx, &args[1], &JSValue::UNDEFINED, &[error]).check_exception()?;
        }

        Ok(promise)
    }

    /// create new Promise object and its resolving function pairs.
    /// returns Promise object and [resolve, reject] functions.
    /// `ctor` is a customized constructor function value to instantiate Promise object:
    ///  - when provised, will call this function to create customized Promise object,
    ///  - when it is `undefined`, creates default Promise object.
    pub unsafe fn new(
        ctx: &JSContext,
        ctor: &JSValue,
    ) -> Result<(PromiseValue, [JSValue; 2]), i32> {
        let executor = ctx.create_native_callable_object(
            PromiseValue::hook_executor,
            2,
            0,
            &[JSValue::UNDEFINED, JSValue::UNDEFINED], // resolving functions, initially undefined
        )?;

        let result_promise = if ctor.is_undefined() {
            Self::new_with_executor(ctx, executor.clone(), &JSValue::UNDEFINED)?.0
        } else {
            JS_CallConstructor(ctx, ctor, std::slice::from_ref(&executor)).check_exception()?
        };

        if let JSObjectIsa::C_FUNCTION_DATA(s) = executor.object_unchecked().0.isa.borrow().deref()
        {
            let data = s.data();
            debug_assert!(!data.is_empty());

            for item in data.iter().take(2) {
                item.ensure_callable(ctx)?;
            }

            let resolving_funcs = [data[0].clone(), data[1].clone()];
            Ok((result_promise.unchecked_into(), resolving_funcs))
        } else {
            unreachable!()
        }
    }

    #[inline(always)]
    pub fn as_value(self) -> JSValue {
        self.0
    }

    /// create [resolve, reject] function pairs for `self` promise object.
    /// Note: `self` is not consumed.
    pub(super) fn create_resolving_functions(&self, ctx: &JSContext) -> Result<[JSValue; 2], i32> {
        // [resolve, reject]
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        let resolved = Rc::new(Cell::new(false));

        for i in 0..2 {
            let s = JSPromiseFunctionData {
                promise: self.clone(),
                resolved: resolved.clone(), // shared between two functions
            };

            let function_proto = ctx.0.borrow().function_proto.clone();
            match ctx.create_instance_from_proto(
                &function_proto,
                if i == 0 {
                    JSObjectIsa::PROMISE_RESOLVE_FUNCTION(s)
                } else {
                    JSObjectIsa::PROMISE_REJECT_FUNCTION(s)
                },
            ) {
                Ok(func_obj) => unsafe {
                    js_function_set_properties(ctx, &func_obj, JS_ATOM_empty_string, 1);
                    resolving_funcs[i as usize] = func_obj;
                },
                Err(isa) => {
                    return Err(-1);
                }
            }
        }

        Ok(resolving_funcs)
    }

    /// get Promise meta data.
    fn with_meta<R>(&self, f: impl FnOnce(&JSPromiseData) -> R) -> Option<R> {
        unsafe {
            if let JSObjectIsa::PROMISE(p) = self.0.object_unchecked().0.isa.borrow().deref() {
                Some(f(p))
            } else {
                None
            }
        }
    }

    /// get Promise meta data.
    fn with_meta_mut<R>(&self, f: impl FnOnce(&mut JSPromiseData) -> R) -> Option<R> {
        unsafe {
            if let JSObjectIsa::PROMISE(p) =
                self.0.object_unchecked().0.isa.borrow_mut().deref_mut()
            {
                Some(f(p))
            } else {
                None
            }
        }
    }

    /// get Promise state.
    #[inline(always)]
    pub fn state(&self) -> Result<JSPromiseStateEnum, i32> {
        self.with_meta(|s| s.promise_state).ok_or(-1)
    }

    /// get Promise result.
    /// returned value is duplicated, caller must free it.
    #[inline(always)]
    pub fn result(&self) -> JSValue {
        self.with_meta(|s| s.promise_result.clone())
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// save resolving resultand update promise status to 'fulfilled' or 'rejected'.
    /// call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    /// Note: `value` is not consumed (duplicated inside).
    pub(super) fn fulfill_or_reject(self, ctx: &JSContext, value: JSValue, is_reject: bool) {
        #[cfg(DUMP_PROMISE)]
        {
            println!("fulfill_or_reject_promise: is_reject={}", is_reject);
        }

        let (promise_state, is_handled) = self
            .with_meta_mut(|s| {
                debug_assert_eq!(s.promise_state, JSPromiseStateEnum::JS_PROMISE_PENDING);

                s.promise_result = value.clone();
                s.promise_state = if is_reject {
                    JSPromiseStateEnum::JS_PROMISE_REJECTED
                } else {
                    JSPromiseStateEnum::JS_PROMISE_FULFILLED
                };

                (s.promise_state, s.is_handled)
            })
            .unwrap();

        if promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !is_handled {
            if let Some(cb) = ctx.rt().host_promise_rejection_tracker {
                cb(
                    ctx,
                    self.clone(),
                    value.clone(),
                    0,
                    ctx.rt().host_promise_rejection_tracker_opaque,
                );
            }
        }

        // call chained reactions registered by `then()`, `catch()` or `finally()`
        let mut rd_list = self
            .with_meta_mut(|s| {
                std::mem::replace(&mut s.promise_reactions[is_reject as usize], Vec::new())
            })
            .unwrap();

        for rd in rd_list.drain(..) {
            ctx.enqueue_job(
                promise_reaction_job,
                &[
                    rd.resolving_funcs[0].clone(),
                    rd.resolving_funcs[1].clone(),
                    rd.handler,
                    JSValue::Bool(is_reject),
                    value.clone(),
                ],
            )
            .unwrap();
        }
    }
}

impl JSContext {
    /// create Promise object and its resolving function paris.
    /// returns Promise object and [resolve, reject] functions.
    /// this is just a shortcut for `PromiseValue::new(ctx, ctor)`.
    #[inline(always)]
    pub fn create_promise(&self, ctor: &JSValueConst) -> Result<(PromiseValue, [JSValue; 2]), i32> {
        unsafe { PromiseValue::new(self, ctor) }
    }
}

//
// Hooks for Promise
//
impl PromiseValue {
    /// the native callback for Promise constructor function
    pub fn hook_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
        let executor = cc.arg(0).clone();
        PromiseValue::new_with_executor(ctx, executor, cc.this()).map(|p| p.0)
    }

    /// callback to execute a promise:
    /// setting resolve and reject functions as attaching data of 'executor'.
    /// arg#0: resolve function, arg#1: reject function
    fn hook_executor(ctx: &JSContext, mut cc: CallContext) -> JSResult {
        for i in 0..2 {
            if !cc.data_raw(i).is_undefined() {
                ctx.JS_ThrowTypeError("resolving function already set");
                return Err(-1);
            } else {
                let _ = cc.replace_data(i, cc.arg(i).clone());
            }
        }
        Ok(JSValue::UNDEFINED)
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {
        // if let JSObjectIsa::PROMISE(s) = isa {
        //     for i in 0..2 {
        //         list_foreach_safe(&mut s.as_mut().unwrap().promise_reactions[i], |el| {
        //             let rd = list_entry!(el, JSPromiseReactionData, link);
        //             Box::from_raw(rd).dispose(rt.as_mut().unwrap());
        //         });
        //     }
        // }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        if let JSObjectIsa::PROMISE(s) = val.object_unchecked().0.isa.borrow().deref() {
            for i in 0..2 {
                for rd in s.promise_reactions[i].iter() {
                    (*rt).gc_mark_value(&rd.resolving_funcs[0], mark_func);
                    (*rt).gc_mark_value(&rd.resolving_funcs[1], mark_func);
                    (*rt).gc_mark_value(&rd.handler, mark_func);
                }
            }

            (*rt).gc_mark_value(&(*s).promise_result, mark_func);
        }
    }
}

pub const PROMISE_MAGIC_all: i32 = 0;
pub const PROMISE_MAGIC_allSettled: i32 = 1;
pub const PROMISE_MAGIC_any: i32 = 2;

pub fn remainingElementsCount_add(
    ctx: &JSContext,
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

pub fn js_promise_all_resolve_element(ctx: &JSContext, mut cc: CallContext) -> JSResult {
    let resolve_type = cc.magic() & 3;
    let is_reject = cc.magic() & 4;

    let alreadyCalled = cc.data_raw(0).coerce_to_bool(ctx)?;
    let values = cc.data_raw(2);
    let resolve = cc.data_raw(3);
    let resolve_element_env = cc.data_raw(4);
    let index = unsafe { JS_ToInt32(ctx, &cc.data_raw(1))? };

    if alreadyCalled {
        return Ok(JSValue::UNDEFINED);
    }

    let _ = cc.replace_data(0, JSValue::Bool(true));

    let obj: JSValue = if resolve_type == PROMISE_MAGIC_allSettled {
        let ob = ctx.create_base_object()?;
        unsafe {
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
                cc.arg(0).clone(),
                JS_PROP_C_W_E,
            )?;

            ob.into()
        }
    } else {
        cc.arg(0).clone()
    };

    values.define_property_value_u32(ctx, index as u32, obj, JS_PROP_C_W_E)?;

    match remainingElementsCount_add(ctx, resolve_element_env, -1)? {
        1 => {
            JS_Call(
                ctx,
                &resolve,
                &JSValue::UNDEFINED,
                std::slice::from_ref(&if resolve_type == PROMISE_MAGIC_any {
                    let error = js_aggregate_error_constructor(ctx, values).check_exception()?;
                    error
                } else {
                    values
                }),
            )
            .check_exception()?;
        }
        0 => {}
        _ => {
            unreachable!()
        }
    }

    Ok(JSValue::UNDEFINED)
}
