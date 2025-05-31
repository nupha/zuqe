use crate::{
    context::JSContext,
    error::JS_ThrowStackOverflow,
    gc::{GCMarkFunc, JS_MarkFunc, JS_MarkValue},
    runtime::JSRuntime,
    value::{object::JSObjectIsa, JSValue, JSValueConst, JS_FreeValueRT},
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

impl BoundFunctionValue {
    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        if let JSObjectIsa::BOUND_FUNCTION(p) = val.object_unchecked().isa_mut() {
            let bf = *p;
            debug_assert!(!bf.is_null());

            JS_FreeValueRT(rt, (*bf).func_obj);
            JS_FreeValueRT(rt, (*bf).this_val);

            for a in (*bf).bind_args.iter() {
                JS_FreeValueRT(rt, *a);
            }

            *p = std::ptr::null_mut();
            drop(Box::from_raw(bf));
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        if let JSObjectIsa::BOUND_FUNCTION(p) = val.object_unchecked().isa_mut() {
            let bf = *p;
            debug_assert!(!bf.is_null());

            (*rt).gc_mark_value(val, mark_func);
            (*rt).gc_mark_value((*bf).func_obj, mark_func);
            (*rt).gc_mark_value((*bf).this_val, mark_func);

            for a in (*bf).bind_args.iter() {
                (*rt).gc_mark_value(*a, mark_func);
            }
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
        if let JSObjectIsa::BOUND_FUNCTION(p) = func_obj.object_unchecked().isa() {
            let bf = (*p).as_ref().unwrap();
            let arg_count = bf.bind_args.len() + argc as usize;

            // let sbuf_size = std::mem::size_of::<JSValue>() * arg_count;
            // if js_check_stack_overflow(ctx.rt(), std::mem::size_of::<JSValue>() * arg_count) {
            //     return JS_ThrowStackOverflow(ctx);
            // }

            let mut arg_list = Vec::<JSValue>::new();
            if arg_list.try_reserve_exact(arg_count).is_err() {
                return JS_ThrowStackOverflow(ctx);
            }
            arg_list.extend_from_slice(&bf.bind_args);
            arg_list.extend_from_slice(std::slice::from_raw_parts(argv, argc as usize));

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
        } else {
            unreachable!()
        }
    }
}
