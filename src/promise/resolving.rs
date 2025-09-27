use std::{cell::Cell, rc::Rc};

use crate::{
    JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    object::{JS_SpeciesConstructor, JSObject, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

use super::{JSPromiseStateEnum, Promise, js_promise_resolve_thenable_job, promise_reaction_job};

/// State of promise resolving function.
#[derive(Debug, Clone)]
pub struct PromiseResolvingPayload {
    /// the target Promise
    pub promise: Promise,
    /// is resolved?
    pub resolved: Rc<Cell<bool>>,
}

/// Represent Promise resolve, reject function object
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct PromiseResolveFunc(JSObject);

object_cast!(
    PromiseResolveFunc,
    JS_CLASS_PROMISE_RESOLVE_FUNCTION | JS_CLASS_PROMISE_REJECT_FUNCTION
);

impl PromiseResolveFunc {
    pub fn class_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_val: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let resolv = func_obj.as_object_PromiseResolveFunc().unwrap();
        let payload = resolv.0.retrive_payload(ctx, None)?;

        if let Some(s) = payload.promise_func_data() {
            if s.resolved.get() {
                return Ok(JSValue::UNDEFINED);
            }

            s.resolved.set(true);

            let is_reject = payload.class_id() == JS_CLASS_PROMISE_REJECT_FUNCTION;
            let resolution = if args.len() > 0 {
                args[0].clone()
            } else {
                JSValue::UNDEFINED
            };

            if is_reject || !resolution.is_object() {
                s.promise.fulfill_or_reject(ctx, resolution, is_reject);
                return Ok(JSValue::UNDEFINED);
            } else if resolution.is_same(&s.promise.clone().into()) {
                ctx.throw_type_error("promise self resolution");
                s.promise.reject(ctx, ctx.take_exception());
                return Ok(JSValue::UNDEFINED);
            }

            if let Ok(then) = resolution.get_property(ctx, JS_ATOM_then) {
                if !then.is_function() {
                    s.promise.fulfill_or_reject(ctx, resolution, is_reject);
                } else {
                    let promise = s.promise.clone();
                    let cx = ctx.clone();
                    ctx.queue_job(move || {
                        js_promise_resolve_thenable_job(&cx, promise, resolution, then)
                    });
                }
            } else {
                s.promise.reject(ctx, ctx.take_exception());
            }
        }

        Ok(JSValue::UNDEFINED)
    }

    pub fn class_finalizer(rt: &mut JSRuntime, isa: ObjectPayload, id: usize) {}

    pub fn class_mark(val: JSValueConst) {
        let isa = unsafe { val.as_object_unchecked().0.payload.borrow() };
        if let Some(s) = isa.promise_func_data() {
            s.promise.gc_mark();
        }
    }
}

#[derive(Debug, Clone)]
pub struct JSPromiseReactionData {
    pub resolving_funcs: [JSValue; 2],
    pub handler: JSValue,
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
    ///    which activates the `reaction_funcs` execution.
    pub(crate) fn reaction(
        &self,
        ctx: &JSContext,
        reaction_funcs: &[JSValue; 2],
        cap_resolving_funcs: &[JSValue; 2],
    ) -> Result<(), i32> {
        // TODO: should use Box::try_new() when stable
        let success = Box::new(JSPromiseReactionData {
            resolving_funcs: [
                cap_resolving_funcs[0].clone(),
                cap_resolving_funcs[1].clone(),
            ],
            handler: {
                let h = reaction_funcs[0].clone();
                if !h.is_function() {
                    JSValue::UNDEFINED
                } else {
                    h
                }
            },
        });

        // TODO: should use Box::try_new() when stable
        let fail = Box::new(JSPromiseReactionData {
            resolving_funcs: [
                cap_resolving_funcs[0].clone(),
                cap_resolving_funcs[1].clone(),
            ],
            handler: {
                let h = reaction_funcs[1].clone();
                if !h.is_function() {
                    JSValue::UNDEFINED
                } else {
                    h
                }
            },
        });

        self.with_mut(|s| {
            match s.state() {
                JSPromiseStateEnum::JS_PROMISE_PENDING => {
                    // add to promise's waiting reactions list
                    s.promise_reactions[0].push(success);
                    s.promise_reactions[1].push(fail);
                }
                state => {
                    // queue as job
                    if state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !s.is_handled {
                        if let Some(cb) = ctx.rt().host_promise_rejection_tracker {
                            cb(
                                ctx,
                                self.clone(),
                                s.promise_result.clone(),
                                true as i32,
                                ctx.rt().host_promise_rejection_tracker_opaque,
                            );
                        }
                    }

                    let act = if state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
                        success
                    } else {
                        fail
                    };
                    let is_reject = state == JSPromiseStateEnum::JS_PROMISE_REJECTED;
                    let result = s.promise_result.clone();
                    let cx = ctx.clone();
                    ctx.queue_job(move || {
                        promise_reaction_job(
                            &cx,
                            act.resolving_funcs,
                            act.handler,
                            is_reject,
                            result,
                        )
                    });
                }
            }

            s.is_handled = true;
        });

        Ok(())
    }

    /// create new promise for `then()`, register reaction handlers to `self` promise.
    /// `resolve` and `reject` are callback functions.
    /// `resolve` and `reject` are not consumed.
    pub fn then(&self, ctx: &JSContext, success: JSValue, fail: JSValue) -> Result<Self, i32> {
        let ctor = JS_SpeciesConstructor(ctx, &self.clone().into(), JSValue::UNDEFINED)?;
        let (promise2, resolving_hooks) = ctx.create_promise(&ctor)?;
        self.reaction(ctx, &[success, fail], &resolving_hooks)?;
        Ok(promise2)
    }

    /// save resolving resultand update promise status to 'fulfilled' or 'rejected'.
    /// call chained reactions which was registered by `then()`, `catch()` or `finally()`.
    /// Note: `value` is not consumed (duplicated inside).
    pub(super) fn fulfill_or_reject(&self, ctx: &JSContext, value: JSValue, is_reject: bool) {
        let (promise_state, is_handled) = self.with_mut(|p| {
            debug_assert_eq!(p.state(), JSPromiseStateEnum::JS_PROMISE_PENDING);

            let state = if is_reject {
                JSPromiseStateEnum::JS_PROMISE_REJECTED
            } else {
                JSPromiseStateEnum::JS_PROMISE_FULFILLED
            };
            p.set_state(state);
            p.promise_result = value.clone();

            (state, p.is_handled)
        });

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
        let mut reactions = self
            .with_mut(|s| std::mem::replace(&mut s.promise_reactions, [Vec::new(), Vec::new()]));

        for act in reactions[is_reject as usize].drain(..) {
            let cx = ctx.clone();
            let val = value.clone();
            ctx.queue_job(move || {
                promise_reaction_job(&cx, act.resolving_funcs, act.handler, is_reject, val)
            });
        }
    }

    /// Reject promise with `value`.
    /// A shortcut to `fulfill_or_reject()` with is_reject=true.
    #[inline(always)]
    pub fn reject(&self, ctx: &JSContext, value: JSValue) {
        self.fulfill_or_reject(ctx, value, true);
    }
}
