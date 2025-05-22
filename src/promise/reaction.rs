use crate::{
    context::JSContext,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{init_list_head, list_add_tail, list_head},
    value::{object::JS_SpeciesConstructor, JSValue, JS_FreeValue, JS_FreeValueRT},
    JS_CLASS_PROMISE,
};

use super::{job::promise_reaction_job, JSPromiseData, JSPromiseStateEnum, PromiseValue};

pub struct JSPromiseReactionData {
    // not used in promise_reaction_job
    pub link: list_head,

    pub resolving_funcs: [JSValue; 2],
    pub handler: JSValue,
}

impl JSPromiseReactionData {
    pub(super) fn dispose(self, rt: &mut JSRuntime) {
        unsafe {
            JS_FreeValueRT(rt, self.resolving_funcs[0]);
            JS_FreeValueRT(rt, self.resolving_funcs[1]);
            JS_FreeValueRT(rt, self.handler);
        }
    }
}

impl PromiseValue {
    /// Register chained reaction to `self` promise.
    /// `resolve_reject` - functions to use as reaction handler.
    /// `cap_resolving_funcs` - [resolve, reject] functions of wrapper promise.
    /// Note: `resolve_reject` and `cap_resolving_funcs` are not consumed.
    pub unsafe fn reaction(
        self,
        ctx: &mut JSContext,
        resolve_reject: &[JSValue; 2],
        cap_resolving_funcs: &[JSValue; 2],
    ) -> Result<(), i32> {
        let mut rd_array: [*mut JSPromiseReactionData; 2] = [std::ptr::null_mut(); 2];

        for i in 0..2 {
            let rd = if let Some(mut p) = js_malloc_typed::<JSPromiseReactionData>(ctx, 1, false) {
                let o = JSPromiseReactionData {
                    link: list_head::default(),
                    resolving_funcs: [cap_resolving_funcs[0].dup(), cap_resolving_funcs[1].dup()],
                    handler: {
                        let h = resolve_reject[i];
                        if !h.is_callable(ctx) {
                            JSValue::UNDEFINED
                        } else {
                            h.dup()
                        }
                    },
                };
                p.write(o);

                init_list_head(&mut p.as_mut().link);
                Box::from_raw(p.as_ptr())
            } else {
                if i == 1 {
                    Box::from_raw(rd_array[0]).dispose(ctx.rt_mut());
                }
                return Err(-1);
            };

            rd_array[i] = Box::into_raw(rd);
        }

        let p = self
            .0
            .object_unchecked()
            .opaque::<JSPromiseData>(JS_CLASS_PROMISE)
            .unwrap()
            .as_mut();

        if p.promise_state == JSPromiseStateEnum::JS_PROMISE_PENDING {
            // register pending to call later in micro task job
            list_add_tail(&mut (*rd_array[0]).link, &mut p.promise_reactions[0]);
            list_add_tail(&mut (*rd_array[1]).link, &mut p.promise_reactions[1]);
        } else {
            // call immediately
            if p.promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !p.is_handled {
                if let Some(cb) = ctx.rt().host_promise_rejection_tracker {
                    cb(
                        ctx,
                        self,
                        p.promise_result,
                        true as i32,
                        ctx.rt().host_promise_rejection_tracker_opaque,
                    );
                }
            }

            let i = if p.promise_state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
                0 // resolve
            } else {
                1 // reject
            };
            let rd = rd_array[i as usize];
            let args: [JSValue; 5] = [
                (*rd).resolving_funcs[0],
                (*rd).resolving_funcs[1],
                (*rd).handler,
                if i == 0 {
                    JSValue::FALSE
                } else {
                    JSValue::TRUE
                },
                p.promise_result,
            ];
            ctx.enqueue_job(promise_reaction_job, &args).unwrap();

            Box::from_raw(rd_array[0]).dispose(ctx.rt_mut());
            Box::from_raw(rd_array[1]).dispose(ctx.rt_mut());
        }

        p.is_handled = true;
        Ok(())
    }

    /// create new promise for `then()`, register reaction handlers to `self` promise.
    /// `resolve` and `reject` are callback functions.
    /// `resolve` and `reject` are not consumed.
    pub fn then(self, ctx: &mut JSContext, resolve: JSValue, reject: JSValue) -> Result<Self, i32> {
        let _ = unsafe {
            self.0
                .object_unchecked()
                .opaque_must::<JSPromiseData>(JS_CLASS_PROMISE, ctx)?
        };

        let ctor = unsafe { JS_SpeciesConstructor(ctx, self.0, JSValue::UNDEFINED) }
            .check_exception()?
            .into_rc(ctx.rt());

        let (result_promise, resolving_funcs) = ctx.create_promise(*ctor)?;

        // perform reaction
        let ret = unsafe { self.reaction(ctx, &[resolve, reject], &resolving_funcs) };

        unsafe {
            JS_FreeValue(ctx, resolving_funcs[0]);
            JS_FreeValue(ctx, resolving_funcs[1]);
        }

        if ret.is_err() {
            unsafe {
                JS_FreeValue(ctx, result_promise.into());
            }
            Err(-1)
        } else {
            Ok(result_promise)
        }
    }
}
