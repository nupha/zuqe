use std::ops::DerefMut;

use crate::{
    context::JSContext,
    object::{JS_SpeciesConstructor, ObjectDetail},
    value::JSValue,
};

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
    /// Register chained reaction to `self` promise.
    /// `resolve_reject` - functions to use as reaction handler.
    /// `cap_resolving_funcs` - [resolve, reject] functions of wrapper promise.
    /// Note: `resolve_reject` and `cap_resolving_funcs` are not consumed.
    pub fn reaction(
        &self,
        ctx: &JSContext,
        resolve_reject: &[JSValue; 2],
        cap_resolving_funcs: &[JSValue; 2],
    ) -> Result<(), i32> {
        let rd1 = Box::new(JSPromiseReactionData {
            resolving_funcs: [
                cap_resolving_funcs[0].clone(),
                cap_resolving_funcs[1].clone(),
            ],
            handler: {
                let h = resolve_reject[0].clone();
                if !h.is_callable() {
                    JSValue::UNDEFINED
                } else {
                    h.clone()
                }
            },
        });
        let rd2 = Box::new(JSPromiseReactionData {
            resolving_funcs: [
                cap_resolving_funcs[0].clone(),
                cap_resolving_funcs[1].clone(),
            ],
            handler: {
                let h = resolve_reject[1].clone();
                if !h.is_callable() {
                    JSValue::UNDEFINED
                } else {
                    h.clone()
                }
            },
        });

        let mut payload = self.0.0.payload.borrow_mut();
        if let ObjectDetail::PROMISE(p) = payload.deref_mut() {
            if p.promise_state == JSPromiseStateEnum::JS_PROMISE_PENDING {
                // register pending to call later in micro task job
                p.promise_reactions[0].push(rd1);
                p.promise_reactions[1].push(rd2);
            } else {
                // call immediately
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

                let rd = if p.promise_state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
                    rd1 // 0 resolve
                } else {
                    rd2 // 1 reject
                };
                let args: [JSValue; 5] = [
                    rd.resolving_funcs[0].clone(),
                    rd.resolving_funcs[1].clone(),
                    rd.handler,
                    JSValue::Bool(p.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED), // is reject?
                    p.promise_result.clone(),
                ];
                ctx.queue_job(promise_reaction_job, &args).unwrap();
            }

            p.is_handled = true;
            Ok(())
        } else {
            unreachable!()
        }
    }

    /// create new promise for `then()`, register reaction handlers to `self` promise.
    /// `resolve` and `reject` are callback functions.
    /// `resolve` and `reject` are not consumed.
    pub fn then(&self, ctx: &JSContext, resolve: JSValue, reject: JSValue) -> Result<Self, i32> {
        // check guard
        // let _ = unsafe {
        //     self.0
        //         .object_unchecked()
        //         .opaque_must::<PromiseObjPayload>(JS_CLASS_PROMISE, ctx)?
        // };

        let ctor = JS_SpeciesConstructor(ctx, &self.clone().into(), JSValue::UNDEFINED)?;
        let (result_promise, resolving_funcs) = ctx.create_promise(&ctor)?;

        // perform reaction
        self.reaction(ctx, &[resolve, reject], &resolving_funcs)?;

        Ok(result_promise)
    }
}
