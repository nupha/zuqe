use crate::{
    context::JSContext,
    error::JS_ThrowStackOverflow,
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::js_free_rt,
    runtime::{js_check_stack_overflow, JSRuntime},
    value::{JSValue, JSValueConst, JS_FreeValueRT, JS_TAG_OBJECT, JS_VALUE_GET_TAG},
    JS_CLASS_BOUND_FUNCTION,
};

use super::{JS_Call, JS_CallConstructor2, JS_CALL_FLAG_CONSTRUCTOR};

pub struct JSBoundFunction {
    pub func_obj: JSValue,
    pub this_val: JSValue,
    pub bind_args: Vec<JSValue>,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct BoundFunctionValue(JSValue);

impl JSValue {
    #[inline(always)]
    pub unsafe fn as_bound_function_unchecked(self) -> BoundFunctionValue {
        debug_assert_eq!(JS_VALUE_GET_TAG(self), JS_TAG_OBJECT);
        debug_assert_eq!((*self.object()).class_id(), JS_CLASS_BOUND_FUNCTION);
        std::mem::transmute(self)
    }
}

impl BoundFunctionValue {
    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        let p = val.object();
        let bf = p.u.bound_function;

        JS_FreeValueRT(rt, (*bf).func_obj);
        JS_FreeValueRT(rt, (*bf).this_val);

        for a in (*bf).bind_args.iter() {
            JS_FreeValueRT(rt, *a);
        }

        drop(Box::from_raw(bf));
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        let p = val.object();
        let bf: *mut JSBoundFunction = p.u.bound_function;

        JS_MarkValue(rt, (*bf).func_obj, mark_func);
        JS_MarkValue(rt, (*bf).this_val, mark_func);
        for a in (*bf).bind_args.iter() {
            JS_MarkValue(rt, *a, mark_func);
        }
    }

    pub unsafe fn hook_call(
        ctx: &mut JSContext,
        func_obj: JSValueConst,
        this_obj: JSValueConst,
        argc: i32,
        argv: *const JSValueConst,
        flags: i32,
    ) -> JSValue {
        #[cfg(debug_assertions)]
        let _ = func_obj.as_bound_function_unchecked(); // ensure it's bound function

        let p = func_obj.object();
        let bf = p.u.bound_function.as_mut().unwrap();
        let arg_count = bf.bind_args.len() + argc as usize;

        let alloca_size = std::mem::size_of::<JSValue>() * arg_count;
        if js_check_stack_overflow(ctx.rt(), alloca_size) {
            return JS_ThrowStackOverflow(ctx);
        }

        // FIXME: use SmallVec for args. don't use alloca.
        // XXX

        crate::mem::alloca::with_alloca_zeroed(alloca_size, |sbuf| {
            let arg_list = std::slice::from_raw_parts_mut(
                sbuf.as_mut_ptr().cast::<JSValue>(),
                arg_count as usize,
            );

            for (i, a) in bf.bind_args.iter().enumerate() {
                arg_list[i] = *a;
            }
            let base = bf.bind_args.len();
            for i in 0..argc as usize {
                arg_list[base + i] = *argv.offset(i as isize);
            }

            if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
                let mut new_target = this_obj;
                if func_obj.is_same(new_target) {
                    new_target = bf.func_obj;
                }

                JS_CallConstructor2(
                    ctx,
                    bf.func_obj,
                    new_target,
                    arg_count as _,
                    arg_list.as_mut_ptr(),
                )
            } else {
                JS_Call(
                    ctx,
                    bf.func_obj,
                    bf.this_val,
                    arg_count as _,
                    arg_list.as_mut_ptr(),
                )
            }
        })
    }
}
