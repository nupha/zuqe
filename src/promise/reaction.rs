use std::ops::DerefMut;

use crate::{
    atom::consts::JS_ATOM_then,
    context::JSContext,
    error::JSResult,
    function::{CallArgs, JS_Call, JS_Invoke},
    mem::js_malloc_typed,
    runtime::JSRuntime,
    types::{list_add_tail, list_head},
    value::{
        object::{JS_GetOpaque, JS_GetOpaque2, JS_SpeciesConstructor},
        JSValue, JS_FreeValue, JS_FreeValueRT,
    },
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
    pub(super) fn free(self: Box<Self>, rt: *mut JSRuntime) {
        unsafe {
            JS_FreeValueRT(rt, self.resolving_funcs[0]);
            JS_FreeValueRT(rt, self.resolving_funcs[1]);
            JS_FreeValueRT(rt, self.handler);
        }
    }
}

impl PromiseValue {
    /// Register then() reaction handlers to `self` promise.
    /// `cap_resolving_funcs` - thrownawayCapability.
    /// Note: `resolve_reject` and `cap_resolving_funcs` are not consumed.
    pub unsafe fn reaction_then(
        self,
        ctx: &mut JSContext,
        resolve_reject: &[JSValue; 2],
        cap_resolving_funcs: &[JSValue; 2],
    ) -> Result<(), i32> {
        let s = JS_GetOpaque(self.0, JS_CLASS_PROMISE).cast::<JSPromiseData>();
        let mut rd_array: [*mut JSPromiseReactionData; 2] = [std::ptr::null_mut(); 2];

        for i in 0..2 {
            let mut rd = if let Some(p) = js_malloc_typed::<JSPromiseReactionData>(ctx, 1, true) {
                Box::from_raw(p.as_ptr())
            } else {
                if i == 1 {
                    Box::from_raw(rd_array[0]).free(ctx.rt_mut());
                }
                return Err(-1);
            };

            rd.resolving_funcs[0] = cap_resolving_funcs[0].dup();
            rd.resolving_funcs[1] = cap_resolving_funcs[1].dup();

            let mut handler = resolve_reject[i];
            if !handler.is_function(ctx) {
                handler = JSValue::UNDEFINED;
            }

            rd.handler = handler.dup();
            rd_array[i] = Box::into_raw(rd);
        }

        if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_PENDING {
            // register pending to call later in micro task job
            list_add_tail(&mut (*rd_array[0]).link, &mut (*s).promise_reactions[0]);
            list_add_tail(&mut (*rd_array[1]).link, &mut (*s).promise_reactions[1]);
        } else {
            // call immediately
            if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_REJECTED && !(*s).is_handled {
                if let Some(cb) = ctx.rt().host_promise_rejection_tracker {
                    cb(
                        ctx,
                        self,
                        (*s).promise_result,
                        true as i32,
                        ctx.rt().host_promise_rejection_tracker_opaque,
                    );
                }
            }

            let i = if (*s).promise_state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
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
                (*s).promise_result,
            ];
            ctx.enqueue_job(promise_reaction_job, &args).unwrap();

            Box::from_raw(rd_array[0]).free(ctx.rt_mut());
            Box::from_raw(rd_array[1]).free(ctx.rt_mut());
        }

        (*s).is_handled = true;
        Ok(())
    }

    /// create new promise for `then()`, register reaction handlers to `self` promise.
    /// `resolve` and `reject` are callback functions.
    /// `resolve` and `reject` are not consumed.
    pub unsafe fn then(
        self,
        ctx: &mut JSContext,
        resolve: JSValue,
        reject: JSValue,
    ) -> Result<Self, i32> {
        let s = JS_GetOpaque2(ctx, self.0, JS_CLASS_PROMISE).cast::<JSPromiseData>();
        if s.is_null() {
            return Err(-1);
        }

        let ctor = JS_SpeciesConstructor(ctx, self.0, JSValue::UNDEFINED)
            .check_exception()?
            .into_rc(ctx.rt());

        let (result_promise, resolving_funcs) = ctx.create_promise(*ctor)?;

        // perform reaction
        let ret = self.reaction_then(ctx, &[resolve, reject], &resolving_funcs);
        JS_FreeValue(ctx, resolving_funcs[0]);
        JS_FreeValue(ctx, resolving_funcs[1]);

        if ret.is_err() {
            JS_FreeValue(ctx, result_promise.into());
            Err(-1)
        } else {
            Ok(result_promise)
        }
    }
}
