// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, Mutex, Weak},
};

use gc_lite::{GcRef, GcTrace};

use crate::{
    JS_CLASS_PROMISE, JobState, TryRecvError, WakeReceiver,
    atom::Atom,
    builtins::error::js_aggregate_error_constructor,
    context::{ContextImpl, ScopedContext},
    error::JSResult,
    function::{CallContext, Callback, NativeFunction, js_function_set_properties},
    object::{JS_PROP_C_W_E, Object, ObjectImpl, PayloadPtr, Promise},
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

#[derive(Debug, GcTrace)]
pub struct PromisePayload {
    #[gc(skip)]
    state: PromiseState,
    /// list of pending reactions: #0 for fulfills, #1 for rejects.
    reactions: [Vec<PromiseReaction>; 2],
    /// promise result
    result: JSValue,

    /// Note: only useful to debug
    #[deprecated]
    is_handled: bool,
}

impl Default for PromisePayload {
    fn default() -> Self {
        Self {
            state: PromiseState::Pending,
            reactions: [Vec::new(), Vec::new()],
            result: JSValue::UNDEFINED,
            is_handled: false,
        }
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

    /// Whether any reaction (then/catch/await) is attached. A promise with no
    /// reactions is observed by nobody; `create_with_receiver_hold_when_awaited`
    /// uses this to decide whether to keep the event loop alive.
    #[inline(always)]
    pub(crate) const fn has_reactions(&self) -> bool {
        !self.reactions[0].is_empty() || !self.reactions[1].is_empty()
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

/// Shared backing storage of a [`PromiseResultSlot`]. The wake hook lives
/// here (not on the slot handle) so every clone of the slot — including the
/// ones held by producer threads/tasks — wakes the parked event loop on
/// `commit`.
pub(crate) struct SlotInner<D: Send> {
    pub(crate) value: Mutex<Option<D>>,
    /// Park state of the runtime that bound this slot to a polling job; set
    /// by `attach_wake` when the job is created. `Weak` so a slot outliving
    /// its runtime doesn't keep the parker thread alive.
    wake: Mutex<Option<Weak<crate::runtime::ParkState>>>,
}

pub struct PromiseResultSlot<D: Send> {
    data: Arc<SlotInner<D>>,
}

impl<D: Send> Clone for PromiseResultSlot<D> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
        }
    }
}

impl<D: Send> Drop for PromiseResultSlot<D> {
    fn drop(&mut self) {
        // A committer disappeared. If it never committed, the polling job can
        // only discover `strong_count == 1` on its *next* poll — wake the
        // parked loop so that poll happens now instead of the promise (and
        // the event loop) hanging. Already-committed drops are skipped:
        // commit() woke the loop. Infrastructure-level guarantee: producers
        // cannot forget to wake, the slot does it for them.
        let committed = self.data.value.lock().unwrap().is_some();
        if !committed
            && let Some(park) = self
                .data
                .wake
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|w| w.upgrade())
        {
            park.wake();
        }
    }
}

impl<D: Send + 'static> PromiseResultSlot<D> {
    pub fn new() -> Self {
        Self {
            data: Arc::new(SlotInner {
                value: Mutex::new(None),
                wake: Mutex::new(None),
            }),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.data.value.lock().unwrap().is_none()
    }

    /// Submit a value into the slot.
    pub fn commit(&self, data: D) -> Result<(), D> {
        {
            let mut guard = self.data.value.lock().unwrap();
            if guard.is_some() {
                return Err(data);
            }
            guard.replace(data);
        }
        // Wake the parked event loop (if bound) so the polling job observes
        // the value promptly instead of waiting out the park quantum.
        if let Some(park) = self
            .data
            .wake
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade())
        {
            park.wake();
        }
        Ok(())
    }

    /// Take the committed value, if any. Lets a caller that drives the slot
    /// itself (rather than through `create_with_result_slot`) observe the
    /// result.
    pub fn try_take(&self) -> Option<D> {
        self.data.value.lock().unwrap().take()
    }

    /// Clone the strong reference to the backing storage.
    ///
    /// A job/fetcher that polls the slot should hold the returned strong
    /// `Arc` for its whole lifetime: a value committed right before the
    /// committers drop their references stays reachable, while
    /// `Arc::strong_count() == 1` (only the poller holds a strong reference)
    /// detects that all committers are gone and settles deterministically
    /// instead of polling forever.
    pub(crate) fn arc(&self) -> Arc<SlotInner<D>> {
        self.data.clone()
    }

    /// Bind this slot to the runtime's park state: a `commit` from any clone
    /// then wakes the parked event loop. Called when a polling job is created
    /// for the slot (`Promise::create_with_result_slot`, async iterators).
    pub(crate) fn attach_wake(&self, park: &Arc<crate::runtime::ParkState>) {
        *self.data.wake.lock().unwrap() = Some(Arc::downgrade(park));
    }
}

impl<D: Send + 'static> Default for PromiseResultSlot<D> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "async_support")]
mod async_slot {
    use super::*;

    pub struct PromiseSlotCommitFut<D: Send + Unpin + 'static> {
        slot: PromiseResultSlot<D>,
        data: Option<D>,
    }

    impl<D: Send + Unpin + 'static> Future for PromiseSlotCommitFut<D> {
        type Output = ();

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            let this = self.get_mut();
            let data = this.data.take().unwrap();

            match this.slot.commit(data) {
                Ok(()) => std::task::Poll::Ready(()),
                Err(d) => {
                    this.data.replace(d);
                    // Re-schedule for re-polling on the next executor tick.
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            }
        }
    }

    impl<D: Send + Unpin + 'static> PromiseResultSlot<D> {
        pub fn commit_async(&self, data: D) -> PromiseSlotCommitFut<D> {
            PromiseSlotCommitFut {
                slot: self.clone(),
                data: Some(data),
            }
        }
    }
}

#[cfg(feature = "async_support")]
pub use async_slot::PromiseSlotCommitFut;

impl Promise {
    /// create Promise object with given executor function.
    /// `new_target` - the 'new' operator target.
    /// `executor` must be a callable object.
    fn create_with_executor(
        ctx: &mut ScopedContext,
        executor: Object,
        ctor: JSValue,
    ) -> Result<Self, i32> {
        executor.ensure_callable(ctx)?;

        // create Promise instance
        let isa = PromisePayload {
            state: PromiseState::Pending,
            reactions: [Vec::new(), Vec::new()],
            result: JSValue::UNDEFINED,
            is_handled: false,
        };
        let promise = Self(
            ctx.ordinary_create_from_constructor_payload(
                ctor,
                PayloadPtr::new(JS_CLASS_PROMISE, isa),
            )?
            .0,
        );

        let rr = promise.create_resolving_functions(ctx)?;

        // call executor function with (resolve, reject) arguments
        if executor.call(ctx, JSValue::UNDEFINED, &rr).is_err() {
            let err_val = ctx.take_exception().to_value(ctx);
            rr[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
        }

        // Register in pending_promises so GC never collects a Pending Promise
        // and loses its reactions/settle callbacks.
        if promise.payload().state() == PromiseState::Pending {
            ctx.rt_mut().gc_pending_promises.push(promise);
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
        let exec_payload = executor.payload();

        for (i, f) in resolving_funcs.iter_mut().enumerate() {
            let v = exec_payload.data(i).unwrap();
            v.ensure_callable(ctx)?; // guard resolving function
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

    /// Create Promise with background polling job.
    ///
    /// The `job` function could return:
    ///  - Some(value) - resolve promise with value.
    ///  - None - job need to be run again.
    ///  - Err(err) - reject promise with err.
    pub fn create_with_job(
        ctx: &mut ScopedContext,
        job: impl Fn(&mut ScopedContext) -> Result<Option<JSValue>, i32> + 'static,
    ) -> Result<Self, i32> {
        Self::create_with_job_state(ctx, move |cx, _promise, rr| match job(cx) {
            Ok(Some(v)) => {
                rr[0].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&v))?;
                debug_assert_eq!(
                    cx.rt().exception(),
                    crate::Exception::No,
                    "[promise create_with_job] resolve() left exception behind"
                );
                Ok(JobState::Done)
            }
            Ok(None) => {
                debug_assert_eq!(
                    cx.rt().exception(),
                    crate::Exception::No,
                    "[promise create_with_job] job returned Ok(None) but left exception behind"
                );
                Ok(JobState::Again)
            }
            Err(_) => {
                let err_val = cx.take_exception().to_value(cx);
                rr[1].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&err_val))?;
                Ok(JobState::Done)
            }
        })
    }

    /// Shared plumbing for `create_with_job` and the hold-when-awaited channel
    /// constructors: create the pending Promise, its \[resolve, reject\]
    /// functions, and queue `job` (which receives the promise and the resolving
    /// functions, and returns the `JobState` directly).
    fn create_with_job_state(
        ctx: &mut ScopedContext,
        job: impl Fn(&mut ScopedContext, Promise, &[JSValue; 2]) -> Result<JobState, i32> + 'static,
    ) -> Result<Self, i32> {
        let promise = Self(
            ctx.ordinary_create_from_constructor_payload(
                JSValue::UNDEFINED,
                PayloadPtr::new(JS_CLASS_PROMISE, PromisePayload::default()),
            )?
            .0,
        );

        let rr = promise.create_resolving_functions(ctx)?;
        ctx.queue_job(
            move |cx| job(cx, promise, &rr),
            JobGcTrace2 { a: rr[0], b: rr[1] },
        );

        ctx.rt_mut().gc_pending_promises.push(promise);

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
        rx: WakeReceiver<D>,
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
                        Err(cx.throw_internal_error("result already handled"))
                    }
                }
                Err(TryRecvError::Empty) => Ok(None),
                Err(TryRecvError::Disconnected) => {
                    Err(cx.throw_internal_error("channel disconnected"))
                }
            }
        })?;

        Ok(promise)
    }

    /// Create Promise driven by an internal `sync_channel`.
    ///
    /// Returns a pair of `(Promise, WakeSender<D>)`. The sender can be used,
    /// typically from another thread, to send a single value of type `D`;
    /// every successful send — and dropping the sender — wakes the parked
    /// event loop automatically, so the polling job runs promptly. When the
    /// value is received, `convert` is called inside a JS context
    /// to turn it into a `JSValue` and resolve the Promise. If the channel is
    /// disconnected before a value is sent, the Promise is rejected with an
    /// internal error.
    pub fn create_with_channel<D: Send + 'static>(
        ctx: &mut ScopedContext,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<(Self, crate::WakeSender<D>), i32> {
        let (tx, rx) = ctx.make_channel(1);
        let promise = Promise::create_with_receiver(ctx, rx, convert)?;
        Ok((promise, tx))
    }

    /// Create Promise driven by a `Receiver`-backed channel, keeping the event
    /// loop alive only while the Promise is observed (awaited or `.then`-ed).
    /// An unobserved (fire-and-forget) promise must not block the process from
    /// exiting — used by `Aike.spawn` so an unawaited child does not keep aike
    /// alive, while an awaited child still does.
    pub fn create_with_receiver_hold_when_awaited<D: Send + 'static>(
        ctx: &mut ScopedContext,
        rx: WakeReceiver<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32> {
        let convert_slot = Rc::new(RefCell::new(Some(convert)));

        Self::create_with_job_state(ctx, move |cx, promise, rr| match rx.try_recv() {
            Ok(data) => {
                let mut slot = convert_slot.borrow_mut();
                if let Some(conv) = slot.take() {
                    let v = conv(cx, data)?;
                    rr[0].call(cx, JSValue::UNDEFINED, std::slice::from_ref(&v))?;
                    Ok(JobState::Done)
                } else {
                    Err(cx.throw_internal_error("result already handled"))
                }
            }
            Err(TryRecvError::Empty) => {
                // Hold the loop only while the promise is observed; a
                // fire-and-forget promise must not keep the process alive.
                if promise.payload().has_reactions() {
                    Ok(JobState::Again)
                } else {
                    Ok(JobState::AgainNoHold)
                }
            }
            Err(TryRecvError::Disconnected) => Err(cx.throw_internal_error("channel disconnected")),
        })
    }

    /// Create Promise driven by an internal `sync_channel`, keeping the event
    /// loop alive only while the Promise is observed. See
    /// [`Promise::create_with_receiver_hold_when_awaited`]. The returned
    /// [`crate::WakeSender`] wakes the parked loop on send and on drop.
    pub fn create_with_channel_hold_when_awaited<D: Send + 'static>(
        ctx: &mut ScopedContext,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<(Self, crate::WakeSender<D>), i32> {
        let (tx, rx) = ctx.make_channel(1);
        let promise = Promise::create_with_receiver_hold_when_awaited(ctx, rx, convert)?;
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
            return Err(ctx.throw_internal_error("promise data slot is not empty"));
        }

        // The job keeps a strong `Arc` for its whole lifetime, so a value
        // committed right before the committers drop their references stays
        // reachable. When it is still empty and `strong_count` is 1 (only the
        // job holds a strong reference), all committers are gone without
        // committing, so settle deterministically instead of polling forever.
        let data = data_slot.arc();
        // Commits from any slot clone (typically on IO threads) wake the
        // parked event loop so this polling job runs promptly.
        data_slot.attach_wake(&ctx.rt_mut().park);
        let convert_slot = Rc::new(RefCell::new(Some(convert)));

        Promise::create_with_job(ctx, move |cx| -> Result<Option<JSValue>, i32> {
            let Some(data) = data.value.lock().unwrap().take() else {
                if Arc::strong_count(&data) == 1 {
                    // Here: slot empty and `strong_count == 1` (only the job holds
                    // a strong ref), i.e. all committers dropped without committing.
                    // Debug panics immediately; release rejects instead of hanging.
                    debug_assert!(false, "[promise result slot] dropped without commit");
                    return Err(
                        cx.throw_internal_error("promise result slot dropped without commit")
                    );
                }
                return Ok(None);
            };

            if let Some(conv) = convert_slot.borrow_mut().take() {
                let v = conv(cx, data)?;
                Ok(Some(v))
            } else {
                #[cfg(debug_assertions)]
                unreachable!("slot value already handled");
                #[cfg(not(debug_assertions))]
                {
                    Err(cx.throw_internal_error("slot value already handled"))
                }
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
            let p = PromiseResolvePayload {
                promise: *self,
                resolved: resolved_flag.clone(), // flag is shared between two functions
            };
            let func = ctx.create_instance_with_proto(
                Some(func_proto),
                PayloadPtr::new(
                    if i == 0 {
                        crate::JS_CLASS_PROMISE_RESOLVE_FUNCTION
                    } else {
                        crate::JS_CLASS_PROMISE_REJECT_FUNCTION
                    },
                    p,
                ),
            )?;

            js_function_set_properties(ctx, func, Atom::CONST_empty_string, 1)?;
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
            unsafe { self.0.as_ref() }.payload_promise().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.0.as_ref().payload_promise().unwrap_unchecked()
        }
    }

    /// get Promise payload
    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut PromisePayload {
        #[cfg(debug_assertions)]
        {
            unsafe { self.0.as_mut() }.payload_promise_mut().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.0.as_mut().payload_promise_mut().unwrap_unchecked()
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
                return Err(ctx.throw_type_error("resolving function already set"));
            } else {
                let _ = cc.replace_data(i, cc.arg(i));
            }
        }
        Ok(JSValue::UNDEFINED)
    }

    /// see ECMAScript spec `Promise.resolve()` and `Promise.reject()` for details.
    fn from_resolve_or_reject_value(
        ctx: &mut ScopedContext,
        value: JSValue,
        is_reject: bool,
    ) -> Result<Self, i32> {
        let (promise, resolving_funcs) = Self::create(ctx)?;
        resolving_funcs[is_reject as usize].call(
            ctx,
            JSValue::UNDEFINED,
            std::slice::from_ref(&value),
        )?;
        Ok(promise)
    }

    /// Create an Promise with resolving a value.
    /// see ECMAScript spec `Promise.resolve()` for details.
    #[inline(always)]
    pub fn from_resolve_value(ctx: &mut ScopedContext, value: JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, false)
    }

    /// Create an Promise with rejecting a value.
    /// see ECMAScript spec `Promise.reject()` for details.
    #[inline(always)]
    pub fn from_reject_value(ctx: &mut ScopedContext, value: JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, true)
    }
}

pub const PROMISE_MAGIC_all: i32 = 0;
pub const PROMISE_MAGIC_allSettled: i32 = 1;
pub const PROMISE_MAGIC_any: i32 = 2;

// Keyed variants (Promise.allKeyed / Promise.allSettledKeyed). The reject bit
// (4) marks the reject twin of an allSettledKeyed resolve element; resolve_type
// (bits 0-1) distinguishes `all` (0) from `allSettled` (1). `allKeyed` reuses
// PROMISE_MAGIC_all (0) since it has no reject twin.
pub const PROMISE_MAGIC_allKeyed: i32 = PROMISE_MAGIC_all;
pub const PROMISE_MAGIC_allSettledKeyed: i32 = PROMISE_MAGIC_allSettled | 4;

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
    resolve_element_env.set_property_u32(ctx, 0, JSValue::from(remainingElementsCount))?;

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

    let _ = cc.replace_data(0, JSValue::TRUE);

    let obj: JSValue = if resolve_type == PROMISE_MAGIC_allSettled {
        let mut ob = ctx.create_base_object()?;

        let status_val =
            ctx.create_string_from_str(if is_reject { "rejected" } else { "fulfilled" })?;

        ob.define_property_value(
            ctx,
            Atom::CONST_status,
            JSValue::make_string(status_val),
            JS_PROP_C_W_E,
        )?;
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

    // Protect obj from GC before writing into values. define_property_value_u32
    // may trigger destruct_fast_array which allocates and could fire arena GC.
    // obj is a LOCAL of the current child scope (NFuncPayload::call uses
    // with_new_scope), but protect explicitly to be safe.
    ctx.gc_protect_transient(&obj);
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
    let exec_func = cc.arg(0).ensure_callable(ctx)?;
    Promise::create_with_executor(ctx, exec_func, cc.this()).map(JSValue::from)
}
