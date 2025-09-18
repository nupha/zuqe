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
    /// - `reaction_funcs` - function pair to act as reaction handler,
    ///    usually customized performing function.
    /// - `cap_resolving_funcs` - function pair of wrapper promise,
    ///    which hook up the wrapper Promise and `reaction_funcs` activation.
    pub fn reaction(
        &self,
        ctx: &JSContext,
        reaction_funcs: &[JSValue; 2],
        cap_resolving_funcs: &[JSValue; 2],
    ) -> Result<(), i32> {
        let a0 = Box::new(JSPromiseReactionData {
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
        let a1 = Box::new(JSPromiseReactionData {
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

        self.with_mut(|p| {
            if p.promise_state == JSPromiseStateEnum::JS_PROMISE_PENDING {
                // queue action to be performed later in job tick (a.k.a. microtask)
                p.promise_reactions[0].push(a0);
                p.promise_reactions[1].push(a1);
            } else {
                // perform action immediately
                if p.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !p.is_handled {
                    if let Some(cb) = ctx.rt().host_promise_rejection_tracker {
                        cb(
                            ctx,
                            self.clone(),
                            p.promise_result.clone(),
                            true as i32,
                            ctx.rt().host_promise_rejection_tracker_opaque,
                        );
                    }
                }

                let act = if p.promise_state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
                    a0
                } else {
                    a1
                };
                let args: [JSValue; 5] = [
                    act.resolving_funcs[0].clone(),
                    act.resolving_funcs[1].clone(),
                    act.handler,
                    JSValue::Bool(p.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED), // is reject?
                    p.promise_result.clone(),
                ];
                ctx.queue_job(promise_reaction_job, &args).unwrap();
            }

            p.is_handled = true;
        });

        Ok(())
    }

    /// create new promise for `then()`, register reaction handlers to `self` promise.
    /// `resolve` and `reject` are callback functions.
    /// `resolve` and `reject` are not consumed.
    pub fn then(&self, ctx: &JSContext, fullfil: JSValue, reject: JSValue) -> Result<Self, i32> {
        let ctor = JS_SpeciesConstructor(ctx, &self.clone().into(), JSValue::UNDEFINED)?;
        let (promise2, resolving_hooks) = ctx.create_promise(&ctor)?;

        // perform reaction
        self.reaction(ctx, &[fullfil, reject], &resolving_hooks)?;
        Ok(promise2)
    }
}
