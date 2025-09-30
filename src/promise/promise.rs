use std::{
    cell::{Cell, RefCell},
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

use super::PromiseResolvingPayload;

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
pub struct PromisePayload {
    promise_state: JSPromiseStateEnum,

    /// list of pending reactions: #0 for fulfill, #1 for reject.
    pub promise_reactions: [Vec<Box<JSPromiseReactionData>>; 2],

    pub promise_result: JSValue,

    /// Note: only useful to debug
    pub is_handled: bool,
}

impl PromisePayload {
    #[inline(always)]
    pub fn state(&self) -> JSPromiseStateEnum {
        self.promise_state
    }

    #[inline(always)]
    pub fn set_state(&mut self, s: JSPromiseStateEnum) {
        self.promise_state = s;
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct Promise(JSObject);

object_cast!(Promise, JS_CLASS_PROMISE);

impl Promise {
    /// create new Promise object with given executor function.
    /// `new_target` - the 'new' operator target.
    /// `executor` must be a callable object.
    pub(super) fn from_executor(
        ctx: &JSContext,
        executor: JSObject,
        new_target: &JSValue,
    ) -> Result<Self, i32> {
        executor.ensure_function(ctx)?;

        // create js object of promise class
        let isa = RefCell::new(Box::new(PromisePayload {
            promise_state: JSPromiseStateEnum::JS_PROMISE_PENDING,
            promise_reactions: [Vec::new(), Vec::new()],
            promise_result: JSValue::UNDEFINED,
            is_handled: false,
        }));
        let promise = Self(
            ctx.create_object_apply_ctor(new_target, ObjectPayload::PROMISE(isa))
                .map_err(|_| -1)?,
        );

        let rr = promise.create_resolving_functions(ctx)?;
        if executor.call(ctx, &JSValue::UNDEFINED, &rr).is_err() {
            let exc = ctx.take_exception();
            rr[1].call(ctx, &JSValue::UNDEFINED, &[exc])?;
        }

        Ok(promise)
    }

    /// create new Promise object and its resolving function pairs.
    /// returns Promise object and [resolve, reject] functions.
    /// `ctor` is a customized constructor function value to instantiate Promise object:
    ///  - when provided, will call this function to create customized Promise object,
    ///  - when it is `undefined`, creates default Promise object.
    pub fn new(ctx: &JSContext, ctor: &JSValue) -> Result<(Promise, [JSValue; 2]), i32> {
        let executor = ctx.create_native_function_with_data(
            Promise::class_executor,
            2,
            0,
            [JSValue::UNDEFINED, JSValue::UNDEFINED].into(), // resolving functions, initially undefined
        )?;

        let promise: Self = if ctor.is_undefined() {
            Self::from_executor(ctx, executor.clone(), &JSValue::UNDEFINED)?
        } else {
            JS_CallConstructor(ctx, ctor, std::slice::from_ref(&executor.clone().into()))
                .err()?
                .try_into()?
        };

        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        if let ObjectPayload::C_FUNCTION_DATA(s) = executor.0.payload.borrow().deref() {
            for i in 0..2 {
                let item = s.get(i).unwrap();
                item.ensure_function(ctx)?;
                resolving_funcs[i] = item;
            }

            Ok((promise.into(), resolving_funcs))
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
            let function_proto = ctx.0.borrow().function_proto.clone();
            let s = Box::new(PromiseResolvingPayload {
                promise: self.clone(),
                resolved: resolved.clone(), // shared between two functions
            });
            let func = ctx
                .create_object_from_proto(
                    function_proto,
                    if i == 0 {
                        ObjectPayload::PROMISE_RESOLVE_FUNCTION(s)
                    } else {
                        ObjectPayload::PROMISE_REJECT_FUNCTION(s)
                    },
                )
                .map_err(|_| -1)?;
            js_function_set_properties(ctx, &func, JS_ATOM_empty_string, 1);
            resolving_funcs[i as usize] = func.into();
        }

        Ok(resolving_funcs)
    }

    /// run closure with Promise payload.
    pub(super) fn with<R>(&self, f: impl FnOnce(&PromisePayload) -> R) -> R {
        if let ObjectPayload::PROMISE(p) = self.0.0.payload.borrow().deref() {
            f(p.borrow().deref())
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// run closure with Promise payload.
    pub(super) fn with_mut<R>(&self, f: impl FnOnce(&mut PromisePayload) -> R) -> R {
        if let ObjectPayload::PROMISE(p) = self.0.0.payload.borrow().deref() {
            f(p.borrow_mut().deref_mut())
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// get Promise state.
    #[inline]
    pub fn state(&self) -> JSPromiseStateEnum {
        self.with(|s| s.promise_state)
    }

    /// get Promise result.
    /// returned value is duplicated, caller must free it.
    #[inline]
    pub fn result(&self) -> JSValue {
        self.with(|s| s.promise_result.clone())
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
    pub fn class_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
        Promise::from_executor(
            ctx,
            cc.arg(0).ensure_function(ctx)?.as_object().unwrap().clone(),
            cc.this(),
        )
        .map(|p| p.into())
    }

    /// callback to execute a promise:
    /// setting resolve and reject functions as attaching data of 'executor'.
    /// arg#0: resolve function, arg#1: reject function
    fn class_executor(ctx: &JSContext, mut cc: CallContext) -> JSResult {
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

    pub fn class_finalizer(_: &mut JSRuntime, isa: &ObjectPayload, _: usize) {
        // if let ObjectPayload::PROMISE(mut s) = isa {
        //     s.promise_reactions[0].clear();
        //     s.promise_reactions[1].clear();
        // }
    }

    pub fn class_mark(val: JSValue) {
        if let Some(promise) = val.as_object_Promise() {
            promise.with(|s| {
                for i in 0..2 {
                    for rd in s.promise_reactions[i].iter() {
                        rd.resolving_funcs[0].gc_mark();
                        rd.resolving_funcs[1].gc_mark();
                        rd.handler.gc_mark();
                    }
                }
                s.promise_result.gc_mark();
            });
        }
    }

    /// see ECMAScript spec `Promise.resolve()` and `Promise.reject()` for details.
    fn from_resolve_or_reject_value(
        ctx: &JSContext,
        value: &JSValue,
        is_reject: bool,
    ) -> Result<Self, i32> {
        let (promise, resolving_funcs) = ctx.create_promise(&JSValue::UNDEFINED)?;
        resolving_funcs[is_reject as usize].call(
            ctx,
            &JSValue::UNDEFINED,
            std::slice::from_ref(value),
        )?;
        Ok(promise)
    }

    /// Create an Promise with resolving a value.
    /// see ECMAScript spec `Promise.resolve()` for details.
    #[inline(always)]
    pub fn from_resolve_value(ctx: &JSContext, value: &JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, false)
    }

    /// Create an Promise with rejecting a value.
    /// see ECMAScript spec `Promise.reject()` for details.
    #[inline(always)]
    pub fn from_reject_value(ctx: &JSContext, value: &JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, true)
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
