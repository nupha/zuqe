use crate::{context::JSContext, object::JS_SpeciesConstructor, value::JSValue};

use super::{JSPromiseStateEnum, Promise, job::promise_reaction_job};

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
    pub fn reaction(
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
                if !h.is_callable() {
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
                if !h.is_callable() {
                    JSValue::UNDEFINED
                } else {
                    h
                }
            },
        });

        self.with_mut(|s| {
            if s.promise_state == JSPromiseStateEnum::JS_PROMISE_PENDING {
                // add to promise's waiting reactions list
                s.promise_reactions[0].push(success);
                s.promise_reactions[1].push(fail);
            } else {
                // queue as job
                if s.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !s.is_handled {
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

                let act = if s.promise_state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
                    success
                } else {
                    fail
                };
                let is_reject = s.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED;
                let result = s.promise_result.clone();
                let cx = ctx.clone();
                ctx.queue_job(move || {
                    promise_reaction_job(&cx, act.resolving_funcs, act.handler, is_reject, result)
                });
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

        // perform reaction
        self.reaction(ctx, &[success, fail], &resolving_hooks)?;
        Ok(promise2)
    }
}
