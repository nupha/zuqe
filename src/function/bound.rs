use std::ops::{Deref, DerefMut};

use crate::{
    context::JSContext,
    error::JSResult,
    gc::GCMarkFunc,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst, object::JSObjectIsa},
};

use super::{JS_CALL_FLAG_CONSTRUCTOR, JS_Call, JS_CallConstructor2};

#[derive(Debug)]
pub struct JSBoundFunction {
    pub func_obj: JSValue,
    pub this_val: JSValue,
    pub bind_args: Vec<JSValue>,
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct BoundFunctionValue(JSValue);

impl BoundFunctionValue {
    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        if let JSObjectIsa::BOUND_FUNCTION(bf) =
            val.object_unchecked().0.isa.borrow_mut().deref_mut()
        {
            (*rt).gc_mark_value(&val, mark_func);
            (*rt).gc_mark_value(&bf.func_obj, mark_func);
            (*rt).gc_mark_value(&bf.this_val, mark_func);

            for a in bf.bind_args.iter() {
                (*rt).gc_mark_value(a, mark_func);
            }
        }
    }

    pub fn hook_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let isa = unsafe { func_obj.object_unchecked().0.isa.borrow() };

        if let JSObjectIsa::BOUND_FUNCTION(bf) = isa.deref() {
            let arg_count = bf.bind_args.len() + args.len() as usize;

            // if js_check_stack_overflow(&ctx.rt(), std::mem::size_of::<JSValue>() * arg_count) {
            //     return JS_ThrowStackOverflow(ctx);
            // }

            let mut arg_list = Vec::<JSValue>::new();
            arg_list.try_reserve_exact(arg_count).map_err(|_| {
                ctx.JS_ThrowOutOfMemory();
                -1
            })?;

            // following will clone() values to arg_list
            arg_list.extend_from_slice(&bf.bind_args);
            arg_list.extend_from_slice(args);

            if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
                let mut new_target = this_obj.clone();
                if func_obj.is_same(&new_target) {
                    new_target = bf.func_obj.clone();
                }
                JS_CallConstructor2(ctx, &bf.func_obj, new_target, arg_list.as_slice())
            } else {
                JS_Call(ctx, &bf.func_obj, &bf.this_val, arg_list.as_slice())
            }
            .check_exception()
        } else {
            unreachable!()
        }
    }
}
