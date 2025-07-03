use std::{cell::Cell, rc::Rc};

use crate::{
    JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst, object::JSObjectIsa},
};

use super::{PromiseValue, js_promise_resolve_thenable_job};

#[derive(Debug, Clone)]
pub struct JSPromiseFunctionData {
    pub promise: PromiseValue,
    pub resolved: Rc<Cell<bool>>,
}

/// Represent a promise resolve or reject function.
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct PromiseResolvingFuncValue(JSValue);

impl PromiseResolvingFuncValue {
    pub fn hook_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_val: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let p = unsafe { func_obj.object_unchecked() };
        let mut isa = p.0.isa.borrow_mut();
        let class_id = isa.class_id();

        if let Some(s) = isa.promise_func_data_mut() {
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

            #[cfg(DUMP_PROMISE)]
            {
                printf(
                    c"js_promise_resolving_function_call: is_reject=%d resolution=",
                    is_reject as i32,
                );
                JS_DumpValue(ctx, resolution);
                printf(c"\n");
            }

            if is_reject || !resolution.is_object() {
                s.promise
                    .clone()
                    .fulfill_or_reject(ctx, resolution, is_reject);
                return Ok(JSValue::UNDEFINED);
            }

            if resolution.is_same(&s.promise.0) {
                ctx.JS_ThrowTypeError("promise self resolution");
                let error = ctx.take_exception();
                s.promise.clone().fulfill_or_reject(ctx, error, true);
                return Ok(JSValue::UNDEFINED);
            }

            let then = resolution.get_property(ctx, JS_ATOM_then);

            if then.is_exception() {
                let error = ctx.take_exception();
                s.promise.clone().fulfill_or_reject(ctx, error, true);
            } else if !then.is_callable() {
                s.promise
                    .clone()
                    .fulfill_or_reject(ctx, resolution, is_reject);
            } else {
                ctx.queue_job(
                    js_promise_resolve_thenable_job,
                    &[s.promise.clone().0, resolution, then], // arg values are not consumed
                )
                .unwrap();
            }

            Ok(JSValue::UNDEFINED)
        } else {
            Ok(JSValue::UNDEFINED)
        }
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

    pub fn hook_mark(val: JSValueConst) {
        let isa = unsafe { val.object_unchecked().0.isa.borrow() };
        if let Some(s) = isa.promise_func_data() {
            s.promise.0.gc_mark();
        }
    }
}

impl JSValue {
    pub unsafe fn as_promise_resolving_func_unchecked(self) -> PromiseResolvingFuncValue {
        debug_assert!(self.is_object());
        debug_assert!(matches!(
            self.object_unchecked().class_id(),
            JS_CLASS_PROMISE_RESOLVE_FUNCTION | JS_CLASS_PROMISE_REJECT_FUNCTION
        ));
        std::mem::transmute(self)
    }
}
