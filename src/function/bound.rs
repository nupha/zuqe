use crate::{
    JS_CLASS_BOUND_FUNCTION,
    context::JSContext,
    error::JSResult,
    object::{JSObject, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    value::JSValue,
};

use super::{JS_CALL_FLAG_CONSTRUCTOR, JS_CallConstructor2};

#[derive(Debug)]
pub struct BoundFuncObjPayload<'a> {
    pub func_obj: JSValue<'a>,
    pub this_val: JSValue<'a>,
    pub bind_args: Vec<JSValue<'a>>,
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct BoundFunction<'a>(JSObject<'a>);

object_cast!(BoundFunction, JS_CLASS_BOUND_FUNCTION);

impl<'a> BoundFunction<'a> {
    pub const fn class_finalizer(_rt: &mut JSRuntime, _isa: &ObjectPayload, _id: usize) {}

    pub fn class_mark(val: JSValue) {
        unsafe {
            if let ObjectPayload::BOUND_FUNCTION(bf) = val.as_object_unchecked().payload() {
                val.gc_mark();
                bf.func_obj.gc_mark();
                bf.this_val.gc_mark();

                for a in bf.bind_args.iter() {
                    a.gc_mark();
                }
            }
        }
    }

    pub fn class_call<'b>(
        ctx: &JSContext<'b>,
        func_obj: &JSValue<'b>,
        this_obj: &JSValue<'b>,
        args: &[JSValue<'b>],
        flags: i32,
    ) -> JSResult<'b> {
        if let ObjectPayload::BOUND_FUNCTION(bf) =
            unsafe { func_obj.as_object_unchecked().payload() }
        {
            let arg_count = bf.bind_args.len() + args.len();

            // if js_check_stack_overflow(&ctx.rt(), std::mem::size_of::<JSValue>() * arg_count) {
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
                let mut new_target = this_obj.clone();
                if func_obj.is_same(&new_target) {
                    new_target = bf.func_obj.clone();
                }
                JS_CallConstructor2(ctx, &bf.func_obj, new_target, arg_list.as_slice()).err()
            } else {
                bf.func_obj.call(ctx, &bf.this_val, arg_list.as_slice())
            }
        } else {
            unreachable!()
        }
    }
}
