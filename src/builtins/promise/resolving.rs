use std::{cell::Cell, rc::Rc};

use gc_lite::{GcRef, GcTracable};

use crate::{
    JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    atom::Atom,
    context::{Context, ContextImpl, ContextPtr},
    error::JSResult,
    object::{JS_SpeciesConstructor, Object, ObjectImpl, ObjectPayload},
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

unsafe impl GcTracable for PromiseResolvePayload {
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
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
        mut ctx: Context,
        func_obj: JSValue,
        _this_val: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut prf = PromiseResolveFunc::try_from(func_obj).unwrap();
        let class_id = prf.class_id();

        let s: &mut PromiseResolvePayload = prf.payload_mut();
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

        let mut promise = s.promise;
        let cx: ContextPtr = ctx.into();

        if is_reject || !arg0.is_object() {
            promise.fulfill_or_reject(cx, arg0, is_reject);
            return Ok(JSValue::UNDEFINED);
        } else if unlikely(arg0.is_same(promise.into())) {
            promise.reject(cx, {
                ctx.throw_type_error("promise self resolution");
                ctx.rt_ptr().take_exception()
            });
            return Ok(JSValue::UNDEFINED);
        } else if let Ok(then) = arg0.get_property(&ctx, Atom::CONST_then) {
            if !then.is_function() {
                promise.fulfill_or_reject(cx, arg0, is_reject);
            } else {
                ctx.queue_job_with_gc_trace(
                    move || js_promise_resolve_thenable_job(&cx, promise, arg0, then),
                    move |tracer| {
                        prf.trace(tracer);
                        arg0.trace(tracer);
                        then.trace(tracer);
                    },
                );
            }
        } else {
            promise.reject(cx, ctx.rt_ptr().take_exception());
        }

        Ok(JSValue::UNDEFINED)
    }

    pub fn payload(&self) -> &PromiseResolvePayload {
        if let ObjectPayload::PROMISE_RESOLVE_FUNCTION(x)
        | ObjectPayload::PROMISE_REJECT_FUNCTION(x) = unsafe { self.0.as_ref().payload() }
        {
            x
        } else {
            unreachable!()
        }
    }

    pub fn payload_mut(&mut self) -> &mut PromiseResolvePayload {
        if let ObjectPayload::PROMISE_RESOLVE_FUNCTION(x)
        | ObjectPayload::PROMISE_REJECT_FUNCTION(x) = unsafe { self.0.as_mut().payload_mut() }
        {
            x
        } else {
            unreachable!()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PromiseReaction {
    pub(super) resolving_funcs: [JSValue; 2],
    pub(super) handler: JSValue,
}

unsafe impl GcTracable for PromiseReaction {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.resolving_funcs[0].trace(tracer);
        self.resolving_funcs[1].trace(tracer);
        self.handler.trace(tracer);
    }
}

impl Default for PromiseReaction {
    fn default() -> Self {
        Self {
            resolving_funcs: [const { JSValue::UNDEFINED }; 2],
            handler: JSValue::UNDEFINED,
        }
    }
}

impl Promise {
    /// Add a chained reaction to Promise.
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
        let success = PromiseReaction {
            resolving_funcs: cap_resolving_funcs,
            handler: {
                let h = reaction_funcs[0];
                if !h.is_function() {
                    JSValue::UNDEFINED
                } else {
                    h
                }
            },
        };

        let fail = PromiseReaction {
            resolving_funcs: cap_resolving_funcs,
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
            PromiseState::Pending => {
                self.payload_mut().add_reaction(success, fail);
            }
            _ => {
                if state == PromiseState::Rejected
                    && !is_handled
                    && let Some(cb) = ctx.rt_ptr().host_promise_rejection_tracker
                {
                    cb(
                        ctx,
                        *self,
                        result,
                        true as i32,
                        ctx.rt_ptr().host_promise_rejection_tracker_opaque,
                    );
                }

                let action = if state == PromiseState::Fulfilled {
                    success
                } else {
                    fail
                };

                // add to job list
                {
                    let is_reject = state == PromiseState::Rejected;
                    let cx = unsafe { ContextPtr::from_ref_unchecked(ctx) };

                    ctx.queue_job_with_gc_trace(
                        move || {
                            let raw = cx.gc_ref();
                            promise_reaction_job(
                                unsafe { raw.as_mut() },
                                action.resolving_funcs,
                                action.handler,
                                is_reject,
                                result,
                            )
                        },
                        move |tracer| {
                            action.trace(tracer);
                            result.trace(tracer);
                        },
                    );
                }
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
    ) -> Result<Object, i32> {
        let ctor = JS_SpeciesConstructor(ctx, (*self).into(), JSValue::UNDEFINED)?;
        let (promise2, rr) = Self::create_from_ctor(ctx, ctor)?;
        self.reaction(ctx, [success, fail], rr)?;
        Ok(promise2)
    }

    /// save resolving resultand update promise status to 'fulfilled' or 'rejected'.
    /// set promise state to 'fulfilled' or 'rejected',
    /// call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    pub(super) fn fulfill_or_reject(
        &mut self,
        mut ctx: ContextPtr,
        value: JSValue,
        is_reject: bool,
    ) {
        let (state, is_handled) = {
            let p = self.payload_mut();
            debug_assert_eq!(p.state(), PromiseState::Pending);

            let s = if is_reject {
                PromiseState::Rejected
            } else {
                PromiseState::Fulfilled
            };
            p.set_state(s);
            p.set_result(value);

            (s, p.is_handled())
        };

        if state == PromiseState::Rejected
            && !is_handled
            && let Some(f) = ctx.rt_ptr().host_promise_rejection_tracker
        {
            f(
                &ctx,
                *self,
                value,
                0,
                ctx.rt_ptr().host_promise_rejection_tracker_opaque,
            );
        }

        // call chained reactions registered by `then()`, `catch()` or `finally()`
        let mut reactions = self.payload_mut().take_reactions();
        let ctx2 = ctx;

        for act in reactions[is_reject as usize].drain(..) {
            // queue reaction in job list to resolve in next tick
            ctx.queue_job_with_gc_trace(
                move || {
                    let raw = ctx2.gc_ref();
                    promise_reaction_job(
                        unsafe { raw.as_mut() },
                        act.resolving_funcs,
                        act.handler,
                        is_reject,
                        value,
                    )
                },
                move |tracer| {
                    value.trace(tracer);
                    act.trace(tracer);
                },
            );
        }
    }

    /// Reject promise with `value`.
    /// A shortcut to `fulfill_or_reject()` with is_reject=true.
    #[inline(always)]
    pub(super) fn reject(&mut self, ctx: ContextPtr, value: JSValue) {
        self.fulfill_or_reject(ctx, value, true);
    }
}
