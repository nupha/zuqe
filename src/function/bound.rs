// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_BOUND_FUNCTION,
    context::Context,
    error::JSResult,
    object::{Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

use super::JS_CALL_FLAG_CONSTRUCTOR;

#[derive(Debug)]
pub struct BoundFuncObjPayload {
    pub func_obj: JSValue,
    pub this_val: JSValue,
    pub bind_args: Vec<JSValue>,
}

impl GcTrace for BoundFuncObjPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.func_obj.trace(tr);
        self.this_val.trace(tr);
        self.bind_args.iter().for_each(|x| x.trace(tr));
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct BoundFunction(GcRef<ObjectImpl>);

object_cast!(BoundFunction, JS_CLASS_BOUND_FUNCTION);

impl BoundFunction {
    pub fn class_call(
        ctx: Context,
        func_obj: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        if let ObjectPayload::BOUND_FUNCTION(bf) =
            unsafe { func_obj.as_object_unchecked().payload() }
        {
            let arg_count = bf.bind_args.len() + args.len();

            // if js_check_stack_overflow(ctx.rt_ref(), std::mem::size_of::<JSValue>() * arg_count) {
            //     return JS_ThrowStackOverflow(ctx);
            // }

            let mut arg_list = Vec::<JSValue>::new();
            arg_list.try_reserve_exact(arg_count).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            // following will clone() values to arg_list
            arg_list.extend_from_slice(&bf.bind_args);
            arg_list.extend_from_slice(args);

            if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
                let mut new_target = this_obj;
                if func_obj.is_same(new_target) {
                    new_target = bf.func_obj;
                }
                bf.func_obj
                    .call_ctor_new_target(&ctx, new_target, &arg_list)
            } else {
                bf.func_obj.call(&ctx, bf.this_val, &arg_list)
            }
        } else {
            unreachable!()
        }
    }
}
