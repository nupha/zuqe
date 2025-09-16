use std::{cell::Cell, rc::Rc};

use crate::{
    JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    object::ObjectPayload,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

use super::{Promise, js_promise_resolve_thenable_job};

/// State of promise resolving function.
#[derive(Debug, Clone)]
pub struct ObjPromiseResolvingState {
    /// the target Promise
    pub promise: Promise,
    /// is resolved?
    pub resolved: Rc<Cell<bool>>,
}

/// Promise resolving function
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct PromiseResolvingFunc(JSValue);

impl PromiseResolvingFunc {
    pub fn hook_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_val: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let p = unsafe { func_obj.as_object_unchecked() };
        let payload = p.0.payload.borrow();
        let class_id = payload.class_id();
        matches!(
            class_id,
            JS_CLASS_PROMISE_RESOLVE_FUNCTION | JS_CLASS_PROMISE_REJECT_FUNCTION
        );

        if let Some(s) = payload.promise_func_data() {
            if s.resolved.get() {
                return Ok(JSValue::UNDEFINED);
            }

            s.resolved.set(true);

            let is_reject = class_id - JS_CLASS_PROMISE_RESOLVE_FUNCTION != 0;
            let resolution = if args.len() > 0 {
                args[0].clone()
            } else {
                JSValue::UNDEFINED
            };

            if is_reject || !resolution.is_object() {
                s.promise.fulfill_or_reject(ctx, resolution, is_reject);
                return Ok(JSValue::UNDEFINED);
            }

            if resolution.is_same(&s.promise.clone().into()) {
                ctx.throw_type_error("promise self resolution");
                let error = ctx.take_exception();
                s.promise.fulfill_or_reject(ctx, error, true);
                return Ok(JSValue::UNDEFINED);
            }

            if let Ok(then) = resolution.get_property(ctx, JS_ATOM_then) {
                if !then.is_callable() {
                    s.promise.fulfill_or_reject(ctx, resolution, is_reject);
                } else {
                    ctx.queue_job(
                        js_promise_resolve_thenable_job,
                        &[s.promise.clone().into(), resolution, then], // arg values are not consumed
                    )
                    .unwrap();
                }
            } else {
                s.promise.fulfill_or_reject(ctx, ctx.take_exception(), true);
            }

            Ok(JSValue::UNDEFINED)
        } else {
            Ok(JSValue::UNDEFINED)
        }
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {}

    pub fn hook_mark(val: JSValueConst) {
        let isa = unsafe { val.as_object_unchecked().0.payload.borrow() };
        if let Some(s) = isa.promise_func_data() {
            s.promise.0.gc_mark();
        }
    }
}
