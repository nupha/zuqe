use std::ptr::NonNull;

use crate::{
    atom::consts::*,
    context::JSContext,
    gc::GCMarkFunc,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_TAG_OBJECT, JS_VALUE_GET_TAG},
    JS_CLASS_PROMISE_REJECT_FUNCTION, JS_CLASS_PROMISE_RESOLVE_FUNCTION,
};

use super::{js_promise_resolve_thenable_job, PromiseValue};

pub struct JSPromiseFunctionData {
    pub promise: PromiseValue,
    pub presolved: NonNull<JSPromiseFunctionDataResolved>,
}

pub struct JSPromiseFunctionDataResolved {
    pub ref_count: i32,
    pub already_resolved: bool,
}

/// Represent a promise resolve or reject function.
#[derive(Debug, Clone, Copy)]
pub struct PromiseResolvingFuncValue(JSValue);

impl PromiseResolvingFuncValue {
    pub unsafe fn hook_call(
        ctx: &mut JSContext,
        func_obj: JSValueConst,
        this_val: JSValueConst,
        argc: i32,
        argv: *const JSValueConst,
        flags: i32,
    ) -> JSValue {
        let p = func_obj.object_unchecked();
        let pfd = p.isa().promise_func_data_ptr();
        let s = if let Some(s) = pfd.as_mut() {
            if s.presolved.as_ref().already_resolved {
                return JSValue::UNDEFINED;
            }
            s
        } else {
            return JSValue::UNDEFINED;
        };

        s.presolved.as_mut().already_resolved = true;
        let is_reject = p.class_id() - JS_CLASS_PROMISE_RESOLVE_FUNCTION != 0;
        let resolution = if argc > 0 {
            *argv.offset(0)
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
            s.promise.fulfill_or_reject(ctx, resolution, is_reject);
            return JSValue::UNDEFINED;
        } else if resolution.is_same(s.promise.0) {
            ctx.JS_ThrowTypeError(c"promise self resolution");
            let error = ctx.take_exception();
            s.promise.fulfill_or_reject(ctx, error, true);
            JS_FreeValue(ctx, error);
            return JSValue::UNDEFINED;
        }

        let then = resolution.get_property(ctx, JS_ATOM_then);
        if then.is_exception() {
            let error = ctx.take_exception();
            s.promise.fulfill_or_reject(ctx, error, true);
            JS_FreeValue(ctx, error);
            JSValue::UNDEFINED
        } else if !then.is_callable(ctx) {
            JS_FreeValue(ctx, then);
            s.promise.fulfill_or_reject(ctx, resolution, is_reject);
            JSValue::UNDEFINED
        } else {
            ctx.enqueue_job(
                js_promise_resolve_thenable_job,
                &[s.promise.0, resolution, then], // arg values are not consumed
            )
            .unwrap();

            JS_FreeValue(ctx, then);
            JSValue::UNDEFINED
        }
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        let s = val.object_unchecked().isa().promise_func_data_ptr();
        if !s.is_null() {
            js_promise_resolve_function_free_resolved((*s).presolved.as_ptr());
            JS_FreeValueRT(rt, (*s).promise.0);
            drop(Box::from_raw(s));
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        let s = val.object_unchecked().isa().promise_func_data_ptr();
        if !s.is_null() {
            (*rt).gc_mark_value((*s).promise.0, mark_func);
        }
    }
}

impl JSValue {
    pub unsafe fn as_promise_resolving_func_unchecked(self) -> PromiseResolvingFuncValue {
        debug_assert_eq!(JS_VALUE_GET_TAG(self), JS_TAG_OBJECT);
        debug_assert!(matches!(
            self.object_unchecked().class_id(),
            JS_CLASS_PROMISE_RESOLVE_FUNCTION | JS_CLASS_PROMISE_REJECT_FUNCTION
        ));
        std::mem::transmute(self)
    }
}

pub unsafe fn js_promise_resolve_function_free_resolved(sr: *mut JSPromiseFunctionDataResolved) {
    debug_assert!((*sr).ref_count > 0);
    (*sr).ref_count -= 1;
    if (*sr).ref_count == 0 {
        drop(Box::from_raw(sr));
    }
}
