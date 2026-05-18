// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, Mutex},
};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_PROMISE, JobState, Receiver, Sender, TryRecvError,
    atom::Atom,
    builtins::error::js_aggregate_error_constructor,
    context::{ContextImpl, ScopedContext},
    error::JSResult,
    function::{CallContext, Callback, NativeFunction, js_function_set_properties},
    make_bounded_channel,
    object::{JS_PROP_C_W_E, Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

use super::{JobGcTrace2, PromiseReaction, PromiseResolvePayload};

pub type JSHostPromiseRejectionTracker = fn(
    ctx: &mut ContextImpl,
    promise: Promise,
    reason: JSValue,
    is_handled: ::core::ffi::c_int,
    opaque: *mut ::core::ffi::c_void,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

#[derive(Debug)]
pub struct PromisePayload {
    state: PromiseState,
    /// list of pending reactions: #0 for fulfills, #1 for rejects.
    reactions: [Vec<PromiseReaction>; 2],
    /// promise result
    result: JSValue,

    /// Note: only useful to debug
    #[deprecated]
    is_handled: bool,
}

impl GcTrace for PromisePayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.reactions[0].iter().for_each(|x| x.trace(gcx));
        self.reactions[1].iter().for_each(|x| x.trace(gcx));
        self.result.trace(gcx);
    }
}

impl PromisePayload {
    #[inline(always)]
    pub const fn state(&self) -> PromiseState {
        self.state
    }

    #[inline(always)]
    pub(crate) const fn set_state(&mut self, s: PromiseState) {
        self.state = s
    }

    #[inline(always)]
    pub const fn result(&self) -> JSValue {
        self.result
    }

    #[inline(always)]
    pub(super) const fn set_result(&mut self, val: JSValue) {
        self.result = val;
    }

    /// add pending reaction callback functions
    #[inline]
    pub(super) fn add_reaction(&mut self, success: PromiseReaction, fail: PromiseReaction) {
        self.reactions[0].push(success);
        self.reactions[1].push(fail);
    }

    /// take pending reactions out, reset reactions list to empty.
    #[inline(always)]
    pub(super) fn take_reactions(&mut self) -> [Vec<PromiseReaction>; 2] {
        std::mem::take(&mut self.reactions)
    }

    #[deprecated]
    pub(super) const fn is_handled(&self) -> bool {
        self.is_handled
    }

    #[deprecated]
    pub(super) const fn set_handled(&mut self, v: bool) {
        self.is_handled = v;
    }
}

pub struct PromiseResultSlot<D> {
    data: Arc<Mutex<Option<D>>>,
}

impl<D> Clone for PromiseResultSlot<D> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
        }
    }
}

impl<D: Send + 'static> PromiseResultSlot<D> {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.data.lock().unwrap().is_none()
    }

    /// Submit a value into the slot.
    pub fn commit(&self, data: D) -> Result<(), D> {
        let mut guard = self.data.lock().unwrap();
        if guard.is_none() {
            guard.replace(data);
            Ok(())
        } else {
            Err(data)
        }
    }

    pub(crate) fn try_take(&self) -> Option<D> {
        self.data.lock().unwrap().take()
    }
}

impl<D: Send + 'static> Default for PromiseResultSlot<D> {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<D> Send for PromiseResultSlot<D> {}
unsafe impl<D> Sync for PromiseResultSlot<D> {}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct Promise(GcRef<ObjectImpl>);

object_cast!(Promise, JS_CLASS_PROMISE);

impl Promise {
    /// create Promise object with given executor function.
    /// `new_target` - the 'new' operator target.
    /// `executor` must be a callable object.
    fn create_with_executor(
        ctx: &mut ScopedContext,
        executor: Object,
        ctor: JSValue,
    ) -> Result<Self, i32> {
        executor.ensure_function(ctx)?;

        // create Promise instance
        let isa = Box::new(PromisePayload {
            state: PromiseState::Pending,
            reactions: [Vec::new(), Vec::new()],
            result: JSValue::UNDEFINED,
            is_handled: false,
        });
        let promise = Self(
            ctx.create_instance_with_ctor(ctor, ObjectPayload::PROMISE(isa))
                .map_err(|_| -1)?
                .0,
        );

        // call executor function with (resolve, reject) arguments
        let rr = promise.create_resolving_functions(ctx)?;
        if executor.call(ctx, JSValue::UNDEFINED, &rr).is_err() {
            let exc = ctx.rt_mut().take_exception();
            let err_val = exc.to_error_value(ctx);
            rr[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
        }

        Ok(promise)
    }

    /// Create Promise (with constructor) and associated \[resolve, reject\] resolving functions pair.
    ///
    /// returns Object (could be Promise or derived class instance) and \[resolve, reject\] functions.
    /// `ctor` is the constructor function value to instantiate Promise object:
    ///  - when provided, will call this function to create customized Promise object,
    ///  - when it is `undefined`, creates default Promise object.
    ///
    /// Resolving functions hold reference to Promise.
    pub(super) fn create_with_ctor(
        ctx: &mut ScopedContext,
        ctor: JSValue,
    ) -> Result<(Object, [JSValue; 2]), i32> {
        let executor = NativeFunction::create(
            ctx,
            Callback::Func(Promise::executor_handler),
            Atom::CONST_empty_string,
            2,
            0,
            ctx.function_proto(),
            // resolving functions, initially undefined.
            // these two fields will be replace by real Function object
            // in callback handler, which takes two arguments: resolve, reject.
            Some([JSValue::UNDEFINED, JSValue::UNDEFINED].into()),
        )?;

        let promise: Object = if ctor.is_undefined() {
            Self::create_with_executor(ctx, executor.into(), JSValue::UNDEFINED)?.into()
        } else {
            ctor.call_ctor(ctx, std::slice::from_ref(&executor.into()))
                .and_then(Object::try_from)?
        };

        // collect resolving functions
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        let data = executor.payload();

        for (i, f) in resolving_funcs.iter_mut().enumerate() {
            let v = data.data(i).unwrap();

            #[cfg(debug_assertions)]
            v.ensure_function(ctx)?;

            *f = v;
        }

        Ok((promise, resolving_funcs))
    }

    /// Create default Promise and its associated \[resolve, reject\] resolving functions pair.
    ///
    /// Resolving functions hold reference to Promise.
    pub fn create(ctx: &mut ScopedContext) -> Result<(Self, [JSValue; 2]), i32> {
        Self::create_with_ctor(ctx, JSValue::UNDEFINED).map(|(p, rr)| {
            debug_assert_eq!(p.class_id(), JS_CLASS_PROMISE);
            (Self(p.0), rr)
        })
    }

    /// Create Promise with background running job.
    ///
    /// The `job` function could return:
    ///  - Some(value) - resolve promise with value.
    ///  - None - job need to be run again.
    ///  - Err(err) - reject promise with err.
    pub fn create_with_job(
        ctx: &mut ScopedContext,
        job: impl Fn(&mut ScopedContext) -> Result<Option<JSValue>, i32> + 'static,
    ) -> Result<Self, i32> {
        let isa = Box::new(PromisePayload {
            state: PromiseState::Pending,
            reactions: [Vec::new(), Vec::new()],
            result: JSValue::UNDEFINED,
            is_handled: false,
        });
        let promise = Self(
            ctx.create_instance_with_ctor(JSValue::UNDEFINED, ObjectPayload::PROMISE(isa))
                .map_err(|_| -1)?
                .0,
        );

        let rr = promise.create_resolving_functions(ctx)?;

        ctx.queue_job(
            move |cx| -> Result<JobState, i32> {
                match job(cx) {
                    Ok(Some(v)) => {
                        rr[0].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&v))?;
                        Ok(JobState::Done)
                    }
                    Ok(None) => Ok(JobState::Again),
                    Err(_) => {
                        let exc = cx.rt_mut().take_exception();
                        let err_val = exc.to_error_value(cx);
                        rr[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
                        Ok(JobState::Done)
                    }
                }
            },
            JobGcTrace2 { a: rr[0], b: rr[1] },
        );

        Ok(promise)
    }

    /// Create Promise driven by a `Receiver`-backed channel.
    ///
    /// The returned Promise is resolved or rejected according to values
    /// received from `rx`. When a value of type `D` is received, `convert`
    /// is called once inside a JS context to produce a `JSResult`, whose
    /// outcome is used to resolve or reject the Promise. If the channel is
    /// disconnected before a value is received, the Promise is rejected
    /// with an internal error.
    pub fn create_with_receiver<D: Send + 'static>(
        ctx: &mut ScopedContext,
        rx: Receiver<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32> {
        let convert_slot = Rc::new(RefCell::new(Some(convert)));

        let promise = Promise::create_with_job(ctx, move |cx| -> Result<Option<JSValue>, i32> {
            match rx.try_recv() {
                Ok(data) => {
                    let mut slot = convert_slot.borrow_mut();
                    if let Some(conv) = slot.take() {
                        let v = conv(cx, data)?;
                        Ok(Some(v))
                    } else {
                        cx.throw_internal_error("result already handled");
                        Err(-1)
                    }
                }
                Err(TryRecvError::Empty) => Ok(None),
                Err(TryRecvError::Disconnected) => {
                    cx.throw_internal_error("channel disconnected");
                    Err(-1)
                }
            }
        })?;

        Ok(promise)
    }

    /// Create Promise driven by an internal `sync_channel`.
    ///
    /// Returns a pair of `(Promise, SyncSender<D>)`. The sender can be used,
    /// typically from another thread, to send a single value of type `D`.
    /// When the value is received, `convert` is called inside a JS context
    /// to turn it into `JSValue` and resolve the Promise. If the channel is
    /// disconnected before a value is sent, the Promise is rejected with an
    /// internal error.
    pub fn create_with_channel<D: Send + 'static>(
        ctx: &mut ScopedContext,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<(Self, Sender<D>), i32> {
        let (tx, rx) = make_bounded_channel::<D>(1);
        let promise = Promise::create_with_receiver(ctx, rx, convert)?;
        Ok((promise, tx))
    }

    /// Create Promise driven by a [`PromiseResultSlot`].
    ///
    /// Returns a pair of `(Promise, PromiseResultSlot<D>)`. The slot can be used,
    /// typically from another thread, to submit a single value of type `D`
    /// via [`PromiseResultSlot::commit`]. When the value is taken, `convert` is called
    /// inside a JS context to turn it into a `JSValue` and resolve the Promise.
    pub fn create_with_result_slot<D: Send + 'static>(
        ctx: &mut ScopedContext,
        data_slot: PromiseResultSlot<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32> {
        if !data_slot.is_empty() {
            ctx.throw_internal_error("promise data slot is not empty");
            return Err(-1);
        }

        let convert_slot = Rc::new(RefCell::new(Some(convert)));

        Promise::create_with_job(ctx, move |cx| -> Result<Option<JSValue>, i32> {
            match data_slot.try_take() {
                Some(data) => {
                    if let Some(conv) = convert_slot.borrow_mut().take() {
                        let v = conv(cx, data)?;
                        Ok(Some(v))
                    } else {
                        #[cfg(debug_assertions)]
                        unreachable!("slot value already handled");
                        #[cfg(not(debug_assertions))]
                        {
                            cx.throw_internal_error("slot value already handled");
                            Err(-1)
                        }
                    }
                }
                None => Ok(None),
            }
        })
    }

    /// Create \[resolve, reject\] resolving functions pair for `self` promise object.
    ///
    /// Resolving functions hold reference to Promise.
    pub(super) fn create_resolving_functions(
        &self,
        ctx: &mut ScopedContext,
    ) -> Result<[JSValue; 2], i32> {
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED]; // [resolve, reject]
        let resolved_flag = Rc::new(Cell::new(false));
        let func_proto = ctx.function_proto();

        for i in 0..2 {
            let p = Box::new(PromiseResolvePayload {
                promise: *self,
                resolved: resolved_flag.clone(), // flag is shared between two functions
            });
            let func = ctx
                .create_instance_with_proto_payload(
                    Some(func_proto),
                    if i == 0 {
                        ObjectPayload::PROMISE_RESOLVE_FUNCTION(p)
                    } else {
                        ObjectPayload::PROMISE_REJECT_FUNCTION(p)
                    },
                )
                .map_err(|_| -1)?;
            js_function_set_properties(ctx, func, Atom::CONST_empty_string, 1);
            resolving_funcs[i as usize] = func.into();
        }

        Ok(resolving_funcs)
    }

    #[inline(always)]
    pub const fn gc_ref(&self) -> GcRef<ObjectImpl> {
        self.0
    }

    /// get Promise payload
    #[inline(always)]
    pub fn payload(&self) -> &PromisePayload {
        #[cfg(debug_assertions)]
        {
            self.0.payload().promise().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.0.payload().promise().unwrap_unchecked()
        }
    }

    /// get Promise payload
    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut PromisePayload {
        #[cfg(debug_assertions)]
        {
            self.0.payload_mut().promise_mut().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.0.payload_mut().promise_mut().unwrap_unchecked()
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
// Hooks for Promise class
//
impl Promise {
    /// callback to execute a promise:
    /// setting resolve and reject functions as attaching data of 'executor'.
    /// arg#0: resolve function, arg#1: reject function
    fn executor_handler(ctx: &mut ScopedContext, mut cc: CallContext) -> JSResult {
        for i in 0..2 {
            if !cc.data(i).is_undefined() {
                ctx.throw_type_error("resolving function already set");
                return Err(-1);
            } else {
                let _ = cc.replace_data(i, cc.arg(i));
            }
        }
        Ok(JSValue::UNDEFINED)
    }

    /// see ECMAScript spec `Promise.resolve()` and `Promise.reject()` for details.
    fn from_resolve_or_reject_value(
        ctx: &mut ScopedContext,
        value: &JSValue,
        is_reject: bool,
    ) -> Result<Self, i32> {
        let (promise, resolving_funcs) = Self::create(ctx)?;
        resolving_funcs[is_reject as usize].call(
            ctx,
            JSValue::UNDEFINED,
            std::slice::from_ref(value),
        )?;
        Ok(promise)
    }

    /// Create an Promise with resolving a value.
    /// see ECMAScript spec `Promise.resolve()` for details.
    #[inline(always)]
    pub fn from_resolve_value(ctx: &mut ScopedContext, value: &JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, false)
    }

    /// Create an Promise with rejecting a value.
    /// see ECMAScript spec `Promise.reject()` for details.
    #[inline(always)]
    pub fn from_reject_value(ctx: &mut ScopedContext, value: &JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, true)
    }
}

pub const PROMISE_MAGIC_all: i32 = 0;
pub const PROMISE_MAGIC_allSettled: i32 = 1;
pub const PROMISE_MAGIC_any: i32 = 2;

pub(crate) fn remainingElementsCount_add(
    ctx: &mut ScopedContext,
    mut resolve_element_env: JSValue,
    addend: i32,
) -> Result<i32, i32> {
    let mut remainingElementsCount = resolve_element_env
        .get_property_u32(ctx, 0)?
        .to_number(ctx)?
        .to_i32();

    remainingElementsCount += addend;
    resolve_element_env.set_property_u32(ctx, 0, JSValue::Int(remainingElementsCount))?;

    Ok(if remainingElementsCount == 0 { 1 } else { 0 })
}

pub fn js_promise_all_resolve_element(ctx: &mut ScopedContext, mut cc: CallContext) -> JSResult {
    let resolve_type = cc.magic() & 3;
    let is_reject = cc.magic() & 4 != 0;

    let alreadyCalled = cc.data(0).to_bool()?;
    let mut values = cc.data(2);
    let resolve = cc.data(3);
    let resolve_element_env = cc.data(4);
    let index = cc.data(1).to_number(ctx)?.to_i32();

    if alreadyCalled {
        return Ok(JSValue::UNDEFINED);
    }

    let _ = cc.replace_data(0, JSValue::Bool(true));

    let obj: JSValue = if resolve_type == PROMISE_MAGIC_allSettled {
        let mut ob = ctx.create_base_object()?;

        let status_val =
            ctx.create_string_from_str(if is_reject { "rejected" } else { "fulfilled" })?;

        ob.define_property_value(ctx, Atom::CONST_status, status_val.into(), JS_PROP_C_W_E)?;
        ob.define_property_value(
            ctx,
            if is_reject {
                Atom::CONST_reason
            } else {
                Atom::CONST_value
            },
            cc.arg(0),
            JS_PROP_C_W_E,
        )?;

        ob.into()
    } else {
        cc.arg(0)
    };

    values.define_property_value_u32(ctx, index as u32, obj, JS_PROP_C_W_E)?;

    match remainingElementsCount_add(ctx, resolve_element_env, -1)? {
        1 => {
            let arg = if resolve_type == PROMISE_MAGIC_any {
                js_aggregate_error_constructor(ctx, values).err()?
            } else {
                values
            };
            resolve.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&arg))?;
        }
        0 => {}
        _ => {
            unreachable!()
        }
    }

    Ok(JSValue::UNDEFINED)
}

pub(crate) fn js_promise_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let exec_func = cc
        .arg(0)
        .ensure_function(ctx)?
        .as_object()
        .copied()
        .unwrap();

    Promise::create_with_executor(ctx, exec_func, cc.this()).map(|p| p.into())
}
