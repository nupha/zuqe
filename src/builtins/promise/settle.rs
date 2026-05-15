// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, rc::Rc};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    atom::Atom,
    builtins::promise::{JobGcTrace2, JobGcTrace3},
    context::{ContextImpl, ScopedContext},
    error::JSResult,
    object::{JS_SpeciesConstructor, Object, ObjectImpl, ObjectPayload},
    object_cast,
    utils::unlikely,
    value::JSValue,
};

use super::{Promise, PromiseState, js_promise_reaction_job, js_promise_resolve_thenable_job};

/// Promise resolving function data
#[derive(Debug)]
pub struct PromiseResolvePayload {
    /// the target Promise
    pub(super) promise: Promise,

    /// promise is resolved?
    /// Note: this flag is shared between resolve and reject functions.
    /// so Rc<T> is used here.
    pub(super) resolved: Rc<Cell<bool>>,
}

impl GcTrace for PromiseResolvePayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.promise.trace(gcx);
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
        ctx: &mut ScopedContext,
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
        let resolution = if !args.is_empty() {
            args[0]
        } else {
            JSValue::UNDEFINED
        };

        let mut promise = s.promise;

        if is_reject || !resolution.is_object() {
            promise.settle(ctx, resolution, is_reject);
            return Ok(JSValue::UNDEFINED);
        } else if unlikely(resolution.is_same(promise.into())) {
            let exc = {
                ctx.throw_type_error("promise self resolution");
                ctx.rt_mut().take_exception()
            };
            let err_val = exc.to_error_value(ctx);
            promise.reject(ctx, err_val);
            return Ok(JSValue::UNDEFINED);
        } else if let Ok(then) = resolution.get_property(ctx, Atom::CONST_then) {
            if then.is_function() {
                ctx.queue_job(
                    move |cx| js_promise_resolve_thenable_job(cx, promise, resolution, then),
                    JobGcTrace3 {
                        a: prf,
                        b: resolution,
                        c: then,
                    },
                );
            } else {
                promise.settle(ctx, resolution, is_reject);
            }
        } else {
            let exc = ctx.rt_mut().take_exception();
            let err_val = exc.to_error_value(ctx);
            promise.reject(ctx, err_val);
        }

        Ok(JSValue::UNDEFINED)
    }

    pub fn payload(&self) -> &PromiseResolvePayload {
        if let ObjectPayload::PROMISE_RESOLVE_FUNCTION(x)
        | ObjectPayload::PROMISE_REJECT_FUNCTION(x) = self.0.payload()
        {
            x
        } else {
            unreachable!()
        }
    }

    pub fn payload_mut(&mut self) -> &mut PromiseResolvePayload {
        if let ObjectPayload::PROMISE_RESOLVE_FUNCTION(x)
        | ObjectPayload::PROMISE_REJECT_FUNCTION(x) = self.0.payload_mut()
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

impl GcTrace for PromiseReaction {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.resolving_funcs[0].trace(tr);
        self.resolving_funcs[1].trace(tr);
        self.handler.trace(tr);
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
                if h.is_function() {
                    h
                } else {
                    JSValue::UNDEFINED
                }
            },
        };
        let fail = PromiseReaction {
            resolving_funcs: cap_resolving_funcs,
            handler: {
                let h = reaction_funcs[1];
                if h.is_function() {
                    h
                } else {
                    JSValue::UNDEFINED
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

                let act = if state == PromiseState::Fulfilled {
                    success
                } else {
                    fail
                };

                // add a job to poll promise reaction in runtime tick
                let is_reject = state == PromiseState::Rejected;
                ctx.queue_job(
                    move |cx| js_promise_reaction_job(cx, act, result, is_reject),
                    JobGcTrace2 { a: act, b: result },
                );
            }
        }

        self.payload_mut().set_handled(true);
        Ok(())
    }

    /// Create wrapper promise for `then()`, register reaction handlers to `self` promise.
    /// `resolve` and `reject` are callback functions.
    pub fn then(
        &mut self,
        ctx: &mut ScopedContext,
        success: JSValue,
        fail: JSValue,
    ) -> Result<Object, i32> {
        let ctor = JS_SpeciesConstructor(ctx, (*self).into(), JSValue::UNDEFINED)?;
        let (wrap_promise, rr) = Self::create_with_ctor(ctx, ctor)?;

        self.reaction(ctx, [success, fail], rr)?;
        Ok(wrap_promise)
    }

    /// Save result `value` and update promise status to 'fulfilled' or 'rejected'.
    /// Call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    fn settle(&mut self, ctx: &mut ScopedContext, value: JSValue, is_reject: bool) {
        let (state, is_handled) = {
            let p = self.payload_mut();
            debug_assert_eq!(p.state(), PromiseState::Pending);

            let s = if is_reject {
                PromiseState::Rejected
            } else {
                PromiseState::Fulfilled
            };
            p.set_state(s);
            p.set_result(value); // value will be traced by Promise itself

            (s, p.is_handled())
        };

        if state == PromiseState::Rejected
            && !is_handled
            && let Some(f) = ctx.rt().host_promise_rejection_tracker
        {
            let opaque = ctx.rt().host_promise_rejection_tracker_opaque;
            f(ctx, *self, value, 0, opaque);
        }

        // call chained reactions
        let mut reactions = self.payload_mut().take_reactions();
        for act in reactions[is_reject as usize].drain(..) {
            ctx.queue_job(
                move |cx| js_promise_reaction_job(cx, act, value, is_reject),
                JobGcTrace2 { a: act, b: value },
            );
        }
    }

    /// Update promise state to fulfilled with result `value`.
    /// Call chained reactions.
    #[inline(always)]
    pub fn resolve(&mut self, ctx: &mut ScopedContext, value: JSValue) {
        self.settle(ctx, value, false);
    }

    /// Update promise state to rejected with result `value`.
    /// Call chained reactions.
    #[inline(always)]
    pub fn reject(&mut self, ctx: &mut ScopedContext, value: JSValue) {
        self.settle(ctx, value, true);
    }
}
