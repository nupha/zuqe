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
    function::{CallContext, JS_CallConstructor, js_function_set_properties},
    js_core::error::js_aggregate_error_constructor,
    object::{JS_PROP_C_W_E, JSObject, ObjectPayload},
    object_cast,
    promise::JSPromiseReactionData,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

use super::{ObjPromiseResolvingState, job::promise_reaction_job};

pub type JSHostPromiseRejectionTracker = fn(
    ctx: &JSContext,
    promise: Promise,
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
pub struct PromiseObjPayload {
    pub promise_state: JSPromiseStateEnum,

    /// list of JSPromiseReactionData.link: #0 for fulfill, #1 for reject.
    pub promise_reactions: [Vec<Box<JSPromiseReactionData>>; 2],

    /// Note: will be freed in Promise::hook_finalizer().
    pub promise_result: JSValue,

    /// Note: only useful to debug
    pub is_handled: bool,
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct Promise(pub(crate) JSObject);

object_cast!(Promise, JS_CLASS_PROMISE);

impl Promise {
    /// create new Promise object with given executor function.
    /// `new_target` - the 'new' operator target.
    /// `executor` must be a callable object.
    pub(super) fn new_with_executor(
        ctx: &JSContext,
        executor: JSObject,
        new_target: &JSValue,
    ) -> Result<Self, i32> {
        executor.ensure_callable(ctx)?;

        // make promise payload
        let s = Box::new(PromiseObjPayload {
            promise_state: JSPromiseStateEnum::JS_PROMISE_PENDING,
            promise_reactions: [Vec::new(), Vec::new()],
            promise_result: JSValue::UNDEFINED,
            is_handled: false,
        });

        // create js object of promise class
        let promise = Self(
            ctx.create_object_apply_ctor(new_target, ObjectPayload::PROMISE(s))
                .map_err(|_| -1)?,
        );

        let args = promise.create_resolving_functions(ctx)?;

        if JSValue::from(executor)
            .call(ctx, &JSValue::UNDEFINED, &args)
            .is_err()
        {
            let exc = ctx.take_exception();
            args[1].call(ctx, &JSValue::UNDEFINED, &[exc])?;
        }

        Ok(promise)
    }

    /// create new Promise object and its resolving function pairs.
    /// returns Promise object and [resolve, reject] functions.
    /// `ctor` is a customized constructor function value to instantiate Promise object:
    ///  - when provided, will call this function to create customized Promise object,
    ///  - when it is `undefined`, creates default Promise object.
    pub fn new(ctx: &JSContext, ctor: &JSValue) -> Result<(Promise, [JSValue; 2]), i32> {
        let executor = ctx.create_native_callable_object(
            Promise::hook_executor,
            2,
            0,
            &[JSValue::UNDEFINED, JSValue::UNDEFINED], // resolving functions, initially undefined
        )?;

        let result_promise: Self = if ctor.is_undefined() {
            Self::new_with_executor(ctx, executor.clone(), &JSValue::UNDEFINED)?
        } else {
            JS_CallConstructor(ctx, ctor, std::slice::from_ref(&executor.clone().into()))
                .err()?
                .try_into()?
        };

        if let ObjectPayload::C_FUNCTION_DATA(s) = executor.0.payload.borrow().deref() {
            let data = s.data();
            debug_assert!(!data.is_empty());

            for item in data.iter().take(2) {
                item.ensure_callable(ctx)?;
            }

            let resolving_funcs = [data[0].clone(), data[1].clone()];
            Ok((result_promise.into(), resolving_funcs))
        } else {
            unreachable!()
        }
    }

    /// create [resolve, reject] function pairs for `self` promise object.
    /// Note: `self` is not consumed.
    pub(super) fn create_resolving_functions(&self, ctx: &JSContext) -> Result<[JSValue; 2], i32> {
        // [resolve, reject]
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        let resolved = Rc::new(Cell::new(false));

        for i in 0..2 {
            let s = ObjPromiseResolvingState {
                promise: self.clone(),
                resolved: resolved.clone(), // shared between two functions
            };

            let function_proto = ctx.0.borrow().function_proto.clone();
            let func_obj = ctx
                .create_object_from_proto(
                    function_proto,
                    if i == 0 {
                        ObjectPayload::PROMISE_RESOLVE_FUNCTION(s)
                    } else {
                        ObjectPayload::PROMISE_REJECT_FUNCTION(s)
                    },
                )
                .map_err(|_| -1)?;
            js_function_set_properties(ctx, &func_obj, JS_ATOM_empty_string, 1);
            resolving_funcs[i as usize] = func_obj.into();
        }

        Ok(resolving_funcs)
    }

    /// get Promise meta data.
    fn with_meta<R>(&self, f: impl FnOnce(&PromiseObjPayload) -> R) -> Option<R> {
        if let ObjectPayload::PROMISE(p) = self.0.0.payload.borrow().deref() {
            Some(f(p))
        } else {
            None
        }
    }

    /// get Promise meta data.
    fn with_meta_mut<R>(&self, f: impl FnOnce(&mut PromiseObjPayload) -> R) -> Option<R> {
        if let ObjectPayload::PROMISE(p) = self.0.0.payload.borrow_mut().deref_mut() {
            Some(f(p))
        } else {
            None
        }
    }

    /// get Promise state.
    #[inline(always)]
    pub fn state(&self) -> Result<JSPromiseStateEnum, i32> {
        self.with_meta(|s| s.promise_state).ok_or(-1)
    }

    /// get Promise result.
    /// returned value is duplicated, caller must free it.
    #[inline]
    pub fn result(&self) -> JSValue {
        self.with_meta(|s| s.promise_result.clone())
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// save resolving resultand update promise status to 'fulfilled' or 'rejected'.
    /// call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    /// Note: `value` is not consumed (duplicated inside).
    pub(super) fn fulfill_or_reject(&self, ctx: &JSContext, value: JSValue, is_reject: bool) {
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
            ctx.queue_job(
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
    /// create Promise and associated resolving functions.
    /// returns Promise and [resolve, reject] functions pair.
    /// this is a shortcut for `Promise::new(ctx, ctor)`.
    #[inline(always)]
    pub fn create_promise(&self, ctor: &JSValueConst) -> Result<(Promise, [JSValue; 2]), i32> {
        Promise::new(self, ctor)
    }
}

//
// Hooks for Promise
//
impl Promise {
    /// the native callback for Promise constructor function
    pub fn hook_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
        Promise::new_with_executor(
            ctx,
            cc.arg(0).ensure_callable(ctx)?.object().unwrap().clone(),
            cc.this(),
        )
        .map(|p| p.into())
    }

    /// callback to execute a promise:
    /// setting resolve and reject functions as attaching data of 'executor'.
    /// arg#0: resolve function, arg#1: reject function
    fn hook_executor(ctx: &JSContext, mut cc: CallContext) -> JSResult {
        for i in 0..2 {
            if !cc.data(i).is_undefined() {
                ctx.throw_type_error("resolving function already set");
                return Err(-1);
            } else {
                let _ = cc.replace_data(i, cc.arg(i).clone());
            }
        }
        Ok(JSValue::UNDEFINED)
    }

    pub fn hook_finalizer(_: &mut JSRuntime, _: &ObjectPayload, _: usize) {
        // if let ObjectPayload::PROMISE(s) = isa {
        //     for i in 0..2 {
        //         list_foreach_safe(&mut s.as_mut().unwrap().promise_reactions[i], |el| {
        //             let rd = list_entry!(el, JSPromiseReactionData, link);
        //             Box::from_raw(rd).dispose(rt.as_mut().unwrap());
        //         });
        //     }
        // }
    }

    pub fn hook_mark(val: JSValue) {
        let p = unsafe { val.object_unchecked() };
        if let ObjectPayload::PROMISE(s) = p.0.payload.borrow().deref() {
            for i in 0..2 {
                for rd in s.promise_reactions[i].iter() {
                    rd.resolving_funcs[0].gc_mark();
                    rd.resolving_funcs[1].gc_mark();
                    rd.handler.gc_mark();
                }
            }

            s.promise_result.gc_mark();
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
    let mut remainingElementsCount = resolve_element_env.get_property_u32(ctx, 0)?.to_i32(ctx)?;

    remainingElementsCount += addend;
    resolve_element_env.set_property_u32(ctx, 0, JSValue::Int(remainingElementsCount))?;

    Ok(if remainingElementsCount == 0 { 1 } else { 0 })
}

pub fn js_promise_all_resolve_element(ctx: &JSContext, mut cc: CallContext) -> JSResult {
    let resolve_type = cc.magic() & 3;
    let is_reject = cc.magic() & 4 != 0;

    let alreadyCalled = cc.data(0).to_bool()?;
    let values = cc.data(2);
    let resolve = cc.data(3);
    let resolve_element_env = cc.data(4);
    let index = cc.data(1).to_i32(ctx)?;

    if alreadyCalled {
        return Ok(JSValue::UNDEFINED);
    }

    let _ = cc.replace_data(0, JSValue::Bool(true));

    let obj: JSValue = if resolve_type == PROMISE_MAGIC_allSettled {
        let ob = ctx.create_base_object()?;

        ob.define_property_value(
            ctx,
            JS_ATOM_status,
            ctx.create_string_from_str(if is_reject { "rejected" } else { "fulfilled" })?
                .into(),
            JS_PROP_C_W_E,
        )?;
        ob.define_property_value(
            ctx,
            if is_reject {
                JS_ATOM_reason
            } else {
                JS_ATOM_value
            },
            cc.arg(0).clone(),
            JS_PROP_C_W_E,
        )?;

        ob.into()
    } else {
        cc.arg(0).clone()
    };

    values.define_property_value_u32(ctx, index as u32, obj, JS_PROP_C_W_E)?;

    match remainingElementsCount_add(ctx, resolve_element_env, -1)? {
        1 => {
            resolve.call(
                ctx,
                &JSValue::UNDEFINED,
                std::slice::from_ref(&if resolve_type == PROMISE_MAGIC_any {
                    let error = js_aggregate_error_constructor(ctx, values).err()?;
                    error
                } else {
                    values
                }),
            )?;
        }
        0 => {}
        _ => {
            unreachable!()
        }
    }

    Ok(JSValue::UNDEFINED)
}
