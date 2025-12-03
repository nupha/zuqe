use std::{cell::Cell, rc::Rc};

use gc_lite::{Collect, GcRef};

use crate::{
    JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    atom::Atom,
    context::{ContextImpl, ContextPtr},
    error::JSResult,
    object::{JS_SpeciesConstructor, ObjectImpl, ObjectRef},
    object_cast,
    utils::unlikely,
    value::JSValue,
};

use super::{Promise, PromiseState, js_promise_resolve_thenable_job, promise_reaction_job};

/// State of promise resolving function.
#[derive(Debug)]
pub struct PromiseResolvePayload {
    /// the target Promise
    pub(super) promise: Promise,

    /// promise is resolved?
    /// Note: this flag is shared between resolve and reject functions.
    /// so Rc<T> is used here.
    pub(super) resolved: Rc<Cell<bool>>,
}

unsafe impl Collect for PromiseResolvePayload {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.promise.trace(tracer);
    }
}

/// Represent Promise resolve, reject function object
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct PromiseResolveFunc(GcRef<ObjectImpl>);

object_cast!(
    PromiseResolveFunc,
    JS_CLASS_PROMISE_RESOLVE_FUNCTION | JS_CLASS_PROMISE_REJECT_FUNCTION
);

impl PromiseResolveFunc {
    pub fn class_call(
        mut ctx: ContextPtr,
        mut func_obj: JSValue,
        _this_val: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let resolv = func_obj.as_object_PromiseResolveFunc_mut().unwrap();
        let isa = resolv.payload_mut();
        let class_id = isa.class_id();

        if let Some(s) = isa.promise_resolving_function_mut() {
            if s.resolved.get() {
                return Ok(JSValue::UNDEFINED);
            }

            s.resolved.set(true);

            let is_reject = class_id == JS_CLASS_PROMISE_REJECT_FUNCTION;
            let arg0 = if !args.is_empty() {
                args[0]
            } else {
                JSValue::UNDEFINED
            };

            if is_reject || !arg0.is_object() {
                s.promise.fulfill_or_reject(ctx, arg0, is_reject);
                return Ok(JSValue::UNDEFINED);
            } else if unlikely(arg0.is_same(s.promise.into())) {
                ctx.throw_type_error("promise self resolution");
                s.promise.reject(ctx, ctx.rt().take_exception());
                return Ok(JSValue::UNDEFINED);
            } else if let Ok(then) = arg0.get_property(&ctx, Atom::CONST_then) {
                if !then.is_function() {
                    s.promise.fulfill_or_reject(ctx, arg0, is_reject);
                } else {
                    let promise = s.promise;
                    let promise2 = promise;
                    let ctx2 = ctx;

                    ctx.queue_job_with_trace(
                        move || js_promise_resolve_thenable_job(&ctx2, promise, arg0, then),
                        move |tracer| {
                            promise2.trace(tracer);
                            arg0.trace(tracer);
                            then.trace(tracer);
                        },
                    );
                }
            } else {
                s.promise.reject(ctx, ctx.rt().take_exception());
            }
        }

        Ok(JSValue::UNDEFINED)
    }
}

#[derive(Debug, Clone)]
pub(super) struct JSPromiseReactionData {
    pub(super) resolving_funcs: [JSValue; 2],
    pub(super) handler: JSValue,
}

unsafe impl Collect for JSPromiseReactionData {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.resolving_funcs[0].trace(tracer);
        self.resolving_funcs[1].trace(tracer);
        self.handler.trace(tracer);
    }
}

impl Default for JSPromiseReactionData {
    fn default() -> Self {
        Self {
            resolving_funcs: [const { JSValue::UNDEFINED }; 2],
            handler: JSValue::UNDEFINED,
        }
    }
}

impl Promise {
    /// Add a chained reaction to `self` Promise.
    ///
    /// Arguments:
    /// - `reaction_funcs` - [success, fail] reaction callback functions, user's logic.
    /// - `cap_resolving_funcs` - resolving functions of wrapper promise,
    ///   which activates the `reaction_funcs` execution.
    pub(crate) fn reaction(
        &mut self,
        ctx: &mut ContextImpl,
        reaction_funcs: [JSValue; 2],
        cap_resolving_funcs: [JSValue; 2],
    ) -> Result<(), i32> {
        let success = JSPromiseReactionData {
            resolving_funcs: [cap_resolving_funcs[0], cap_resolving_funcs[1]],
            handler: {
                let h = reaction_funcs[0];
                if !h.is_function() {
                    JSValue::UNDEFINED
                } else {
                    h
                }
            },
        };

        let fail = JSPromiseReactionData {
            resolving_funcs: [cap_resolving_funcs[0], cap_resolving_funcs[1]],
            handler: {
                let h = reaction_funcs[1];
                if !h.is_function() {
                    JSValue::UNDEFINED
                } else {
                    h
                }
            },
        };

        let (state, result, is_handled) = {
            let s = self.payload();
            (s.state(), s.result(), s.is_handled())
        };

        match state {
            PromiseState::JS_PROMISE_PENDING => {
                self.payload_mut().add_reaction(success, fail);
            }
            state => {
                // queue as job
                if state == PromiseState::JS_PROMISE_REJECTED
                    && !is_handled
                    && let Some(cb) = ctx.rt().host_promise_rejection_tracker
                {
                    cb(
                        ctx,
                        *self,
                        result,
                        true as i32,
                        ctx.rt().host_promise_rejection_tracker_opaque,
                    );
                }

                let act = if state == PromiseState::JS_PROMISE_FULFILLED {
                    success
                } else {
                    fail
                };

                let is_reject = state == PromiseState::JS_PROMISE_REJECTED;
                let mut cx = unsafe { ContextPtr::from_ref_unchecked(ctx) };
                let act2 = act.clone();

                ctx.queue_job_with_trace(
                    move || {
                        promise_reaction_job(
                            &mut cx,
                            act.resolving_funcs,
                            act.handler,
                            is_reject,
                            result,
                        )
                    },
                    move |tracer| {
                        act2.trace(tracer);
                        result.trace(tracer);
                    },
                );
            }
        }

        self.payload_mut().set_handled(true);
        Ok(())
    }

    /// create new promise for `then()`, register reaction handlers to `self` promise.
    /// `resolve` and `reject` are callback functions.
    /// `resolve` and `reject` are not consumed.
    pub fn then(
        &mut self,
        ctx: &mut ContextImpl,
        success: JSValue,
        fail: JSValue,
    ) -> Result<ObjectRef, i32> {
        let ctor = JS_SpeciesConstructor(ctx, (*self).into(), JSValue::UNDEFINED)?;
        let (promise2, rr) = Self::create_from_ctor(ctx, ctor)?;

        self.reaction(ctx, [success, fail], rr)?;
        Ok(promise2)
    }

    /// save resolving resultand update promise status to 'fulfilled' or 'rejected'.
    /// call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    /// Note: `value` is not consumed (duplicated inside).
    pub(super) fn fulfill_or_reject(
        &mut self,
        mut ctx: ContextPtr,
        value: JSValue,
        is_reject: bool,
    ) {
        let (promise_state, is_handled) = {
            let p = self.payload_mut();
            debug_assert_eq!(p.state(), PromiseState::JS_PROMISE_PENDING);

            let state = if is_reject {
                PromiseState::JS_PROMISE_REJECTED
            } else {
                PromiseState::JS_PROMISE_FULFILLED
            };
            p.set_state(state);
            p.set_result(value);

            (state, p.is_handled())
        };

        if promise_state == PromiseState::JS_PROMISE_REJECTED
            && !is_handled
            && let Some(f) = ctx.rt().host_promise_rejection_tracker
        {
            f(
                &ctx,
                *self,
                value,
                0,
                ctx.rt().host_promise_rejection_tracker_opaque,
            );
        }

        // call chained reactions registered by `then()`, `catch()` or `finally()`
        let mut reactions = self.payload_mut().take_reactions();
        let mut ctx2 = ctx;

        for act in reactions[is_reject as usize].drain(..) {
            let val = value;
            let act2 = act.clone();

            ctx.queue_job_with_trace(
                move || {
                    promise_reaction_job(
                        &mut ctx2,
                        act.resolving_funcs,
                        act.handler,
                        is_reject,
                        val,
                    )
                },
                move |tracer| {
                    val.trace(tracer);
                    act2.trace(tracer);
                },
            );
        }
    }

    /// Reject promise with `value`.
    /// A shortcut to `fulfill_or_reject()` with is_reject=true.
    #[inline(always)]
    pub fn reject(&mut self, ctx: ContextPtr, value: JSValue) {
        self.fulfill_or_reject(ctx, value, true);
    }
}
