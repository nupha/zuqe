// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, ops::DerefMut, rc::Rc};

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    ErrorKind, Exception, JS_CLASS_PROMISE_REJECT_FUNCTION,
    atom::Atom,
    context::{ContextImpl, ScopedContext},
    error::JSResult,
    gc::GcHeapExt,
    object::{JS_SpeciesConstructor, Object, ObjectPayload, PromiseResolveFunc},
    utils::unlikely,
    value::JSValue,
};

use super::{
    JobGcTrace2, JobGcTrace3, Promise, PromiseState, js_promise_reaction_job,
    js_promise_resolve_thenable_job,
};

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
            let err_val = ctx
                .create_error_object(
                    ErrorKind::Type,
                    "promise self resolution",
                    ctx.throw_need_backtrace(),
                )
                .map_or(Exception::OutOfMemory.to_value(ctx), JSValue::from);
            promise.reject(ctx, err_val);
            return Ok(JSValue::UNDEFINED);
        } else if let Ok(then) = resolution.get_property(ctx, Atom::CONST_then) {
            if then.is_callable() {
                ctx.queue_oneshot_job(
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
            let err_val = ctx.take_exception().to_value(ctx);
            promise.reject(ctx, err_val);
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
        | ObjectPayload::PROMISE_REJECT_FUNCTION(x) = self.deref_mut().payload_mut()
        {
            x.as_mut()
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
                if h.is_callable() {
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
                if h.is_callable() {
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
                // bind write barrier
                let self_node = self.gc_node_ptr();
                for v in [
                    success.resolving_funcs[0],
                    success.resolving_funcs[1],
                    success.handler,
                    fail.resolving_funcs[0],
                    fail.resolving_funcs[1],
                    fail.handler,
                ] {
                    ctx.gc_heap_mut().bind_value(self_node, &v);
                }

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
                ctx.queue_oneshot_job(
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

    /// Internal `PerformPromiseThen` equivalent: create a wrapper promise and
    /// register reaction handlers **without** looking up `@@species`.  Use this
    /// from host code (e.g. Streams pipeTo) that must not call the
    /// user-visible `Promise.prototype.then`.
    pub fn perform_then(
        &mut self,
        ctx: &mut ScopedContext,
        success: JSValue,
        fail: JSValue,
    ) -> Result<Object, i32> {
        let (wrap_promise, rr) = Self::create_with_ctor(ctx, JSValue::UNDEFINED)?;
        self.reaction(ctx, [success, fail], rr)?;
        Ok(wrap_promise)
    }

    /// Save result `value` and update promise status to 'fulfilled' or 'rejected'.
    /// Call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    ///
    /// In zuqe, `Promise::resolve()` / `Promise::reject()` bypass the
    /// resolving-function closure and settle the promise directly. Multiple
    /// code paths (e.g. abort/error vs. normal completion in Streams) may
    /// race to settle the same promise. To match the spec's observable
    /// behaviour (second settle is a silent no-op), we return early when the
    /// promise is no longer Pending.
    fn settle(&mut self, ctx: &mut ScopedContext, value: JSValue, is_reject: bool) {
        // Per ECMAScript 2027 §27.5.1.3 (CreateResolvingFunctions), the resolve
        // and reject functions produced by `CreateResolvingFunctions` share a
        // `promiseOrEmpty` record whose `[[Value]]` is set to **empty** after the
        // first invocation. Subsequent calls hit the early-return step
        // (`If promiseOrEmpty.[[Value]] is empty, return undefined`) and are
        // no-ops. The corresponding `FulfillPromise` (§27.5.1.4) and
        // `RejectPromise` (§27.5.1.7) both carry the normative assertion
        // `Assert: promise.[[PromiseState]] is pending`, which is guaranteed to
        // hold because the resolving-function closure prevents double-settling.
        if self.payload().state() != PromiseState::Pending {
            #[cfg(debug_assertions)]
            eprintln!(
                "[zuqe] Promise::{} called on already-{} promise (state={:?}) — no-op per ECMA-262 §27.5.1.3",
                if is_reject { "reject" } else { "resolve" },
                if is_reject { "rejected" } else { "fulfilled" },
                self.payload().state(),
            );
            return;
        }

        // Remove from pending_promises — the Promise is no longer Pending.
        let self_node = self.gc_node_ptr();
        let rt = ctx.rt_mut();
        if let Some(idx) = rt
            .gc_pending_promises
            .iter()
            .position(|p| p.gc_node_ptr() == self_node)
        {
            rt.gc_pending_promises.swap_remove(idx);
        }

        ctx.gc_heap_mut().bind_value(self.gc_node_ptr(), &value); // write barrier

        let (state, is_handled) = {
            let s = if is_reject {
                PromiseState::Rejected
            } else {
                PromiseState::Fulfilled
            };
            let p = self.payload_mut();
            p.set_state(s);
            p.set_result(value);

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
            ctx.queue_oneshot_job(
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
