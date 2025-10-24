use std::{
    cell::{Cell, RefCell},
    ops::DerefMut,
    rc::Rc,
};

use crate::{
    JS_CLASS_PROMISE,
    atom::{Atom, consts::*},
    callable::NativeFunctionWithData,
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_CallConstructor, js_function_set_properties},
    js_core::error::js_aggregate_error_constructor,
    object::{JS_PROP_C_W_E, JSObject, ObjectPayload},
    object_cast,
    promise::JSPromiseReactionData,
    runtime::JSRuntime,
    value::JSValue,
};

use super::PromiseResolvePayload;

pub type JSHostPromiseRejectionTracker = fn(
    ctx: &JSContext,
    promise: Promise,
    reason: JSValue,
    is_handled: ::core::ffi::c_int,
    opaque: *mut ::core::ffi::c_void,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PromiseState {
    JS_PROMISE_PENDING,
    JS_PROMISE_FULFILLED,
    JS_PROMISE_REJECTED,
}

#[derive(Debug)]
pub struct PromisePayload {
    promise_state: Cell<PromiseState>,

    /// list of pending reactions:
    /// #0 for fulfill, #1 for reject.
    pub promise_reactions: RefCell<[Vec<Box<JSPromiseReactionData>>; 2]>,

    pub promise_result: RefCell<JSValue>,

    /// Note: only useful to debug
    pub is_handled: Cell<bool>,
}

impl PromisePayload {
    #[inline(always)]
    pub const fn state(&self) -> PromiseState {
        self.promise_state.get()
    }

    #[inline(always)]
    pub fn set_state(&self, s: PromiseState) {
        self.promise_state.set(s);
    }

    #[inline(always)]
    pub fn result(&self) -> JSValue {
        self.promise_result.borrow().clone()
    }

    pub fn set_result(&self, val: JSValue) {
        *self.promise_result.borrow_mut() = val;
    }

    /// add pending reaction callback functions
    pub fn add_reaction(
        &self,
        success: Box<JSPromiseReactionData>,
        fail: Box<JSPromiseReactionData>,
    ) {
        let mut b = self.promise_reactions.borrow_mut();
        b[0].push(success);
        b[1].push(fail);
    }

    /// take pending reactions out, reset reactions list to empty.
    pub fn take_reactions(&self) -> [Vec<Box<JSPromiseReactionData>>; 2] {
        std::mem::take(self.promise_reactions.borrow_mut().deref_mut())
    }

    pub const fn is_handled(&self) -> bool {
        self.is_handled.get()
    }
    pub fn set_handled(&self, v: bool) {
        self.is_handled.set(v);
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
    fn from_executor(ctx: &JSContext, executor: JSObject, ctor: &JSValue) -> Result<Self, i32> {
        executor.ensure_function(ctx)?;

        // create js object of promise class
        let isa = Box::new(PromisePayload {
            promise_state: Cell::new(PromiseState::JS_PROMISE_PENDING),
            promise_reactions: RefCell::new([Vec::new(), Vec::new()]),
            promise_result: RefCell::new(JSValue::UNDEFINED),
            is_handled: Cell::new(false),
        });
        let promise = Self(
            ctx.create_object_from_ctor(ctor, ObjectPayload::PROMISE(isa))
                .map_err(|_| -1)?,
        );

        let rr = promise.create_resolving_functions(ctx)?;
        if executor.call(ctx, &JSValue::UNDEFINED, &rr).is_err() {
            let exc = ctx.take_exception();
            rr[1].call(ctx, &JSValue::UNDEFINED, &[exc])?;
        }

        Ok(promise)
    }

    /// create new Promise and associated resolving function pairs.
    /// returns Object (could be Promise or derived class instance) and \[resolve, reject\] functions.
    /// `ctor` is the constructor function value to instantiate Promise object:
    ///  - when provided, will call this function to create customized Promise object,
    ///  - when it is `undefined`, creates default Promise object.
    pub(super) fn create_from_ctor(
        ctx: &JSContext,
        ctor: &JSValue,
    ) -> Result<(JSObject, [JSValue; 2]), i32> {
        let executor = NativeFunctionWithData::create(
            ctx,
            Promise::executor_handler,
            2,
            0,
            // resolving functions, initially undefined.
            // these two fields will be replace by real Function object
            // in callback handler, which takes two arguments: resolve, reject.
            [JSValue::UNDEFINED, JSValue::UNDEFINED].into(),
        )?;

        let promise: JSObject = if ctor.is_undefined() {
            Self::from_executor(ctx, executor.clone().into(), &JSValue::UNDEFINED)?.into()
        } else {
            JS_CallConstructor(ctx, ctor, std::slice::from_ref(&executor.clone().into()))
                .err()?
                .try_into()?
        };

        // collect resolving functions
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        if let ObjectPayload::C_FUNCTION_DATA(s) = executor.payload() {
            for i in 0..2 {
                let item = s.data(i).unwrap();
                item.ensure_function(ctx)?;
                resolving_funcs[i] = item;
            }

            Ok((promise, resolving_funcs))
        } else {
            unreachable!()
        }
    }

    /// create default Promise and associated resolving functions.
    /// returns a Promise object and \[resolve, reject\] functions pair.
    #[inline]
    pub fn create(ctx: &JSContext) -> Result<(Self, [JSValue; 2]), i32> {
        Self::create_from_ctor(ctx, &JSValue::UNDEFINED).map(|(p, rr)| {
            debug_assert_eq!(p.class_id(), JS_CLASS_PROMISE);
            (Self(p), rr)
        })
    }

    /// create \[resolve, reject\] function pairs for `self` promise object.
    pub(super) fn create_resolving_functions(&self, ctx: &JSContext) -> Result<[JSValue; 2], i32> {
        // [resolve, reject]
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        let resolved = Rc::new(Cell::new(false));
        let func_proto = ctx.function_proto();

        for i in 0..2 {
            let s = Box::new(PromiseResolvePayload {
                promise: self.clone(),
                resolved: resolved.clone(), // shared between two functions
            });
            let func = ctx
                .create_object_with_proto(
                    Some(func_proto.clone()),
                    if i == 0 {
                        ObjectPayload::PROMISE_RESOLVE_FUNCTION(s)
                    } else {
                        ObjectPayload::PROMISE_REJECT_FUNCTION(s)
                    },
                )
                .map_err(|_| -1)?;
            js_function_set_properties(ctx, &func, Atom::new_const(JS_ATOM_empty_string), 1);
            resolving_funcs[i as usize] = func.into();
        }

        Ok(resolving_funcs)
    }

    /// get Promise payload
    #[inline(always)]
    pub fn payload(&self) -> &PromisePayload {
        if let ObjectPayload::PROMISE(p) = self.0.payload() {
            p
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
    #[inline(always)]
    pub fn state(&self) -> PromiseState {
        self.payload().state()
    }

    /// get Promise result.
    /// returned value is duplicated, caller must free it.
    #[inline(always)]
    pub fn result(&self) -> JSValue {
        self.payload().result()
    }
}

//
// Hooks for Promise
//
impl Promise {
    /// callback to execute a promise:
    /// setting resolve and reject functions as attaching data of 'executor'.
    /// arg#0: resolve function, arg#1: reject function
    fn executor_handler(ctx: &JSContext, mut cc: CallContext) -> JSResult {
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

    /// the native callback for Promise constructor function
    pub fn class_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
        Promise::from_executor(
            ctx,
            cc.arg(0).ensure_function(ctx)?.as_object().unwrap().clone(),
            cc.this(),
        )
        .map(|p| p.into())
    }

    pub const fn class_finalizer(_: &mut JSRuntime, _isa: &ObjectPayload, _: usize) {
        // if let ObjectPayload::PROMISE(mut s) = isa {
        //     s.promise_reactions[0].clear();
        //     s.promise_reactions[1].clear();
        // }
    }

    pub fn class_mark(val: JSValue) {
        if let Some(promise) = val.as_object_Promise() {
            let s = promise.payload();

            let re = s.promise_reactions.borrow();
            for i in 0..2 {
                for rd in re[i].iter() {
                    rd.resolving_funcs[0].gc_mark();
                    rd.resolving_funcs[1].gc_mark();
                    rd.handler.gc_mark();
                }
            }

            s.result().gc_mark();
        }
    }

    /// see ECMAScript spec `Promise.resolve()` and `Promise.reject()` for details.
    fn from_resolve_or_reject_value(
        ctx: &JSContext,
        value: &JSValue,
        is_reject: bool,
    ) -> Result<Self, i32> {
        let (promise, resolving_funcs) = Self::create(ctx)?;
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
            &JS_ATOM_status,
            ctx.create_string_from_str(if is_reject { "rejected" } else { "fulfilled" })?
                .into(),
            JS_PROP_C_W_E,
        )?;
        ob.define_property_value(
            ctx,
            if is_reject {
                &JS_ATOM_reason
            } else {
                &JS_ATOM_value
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
                    js_aggregate_error_constructor(ctx, values).err()?
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
