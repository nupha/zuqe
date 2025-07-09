use std::ops::Deref;

use crate::{
    JS_CLASS_BOUND_FUNCTION,
    context::JSContext,
    error::JSResult,
    object::{JSObject, ObjectDetail},
    runtime::JSRuntime,
    value::JSValue,
};

use super::{JS_CALL_FLAG_CONSTRUCTOR, JS_Call, JS_CallConstructor2};

#[derive(Debug)]
pub struct ObjBoundFunc {
    pub func_obj: JSValue,
    pub this_val: JSValue,
    pub bind_args: Vec<JSValue>,
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct BoundFunction(JSObject);

impl TryFrom<JSObject> for BoundFunction {
    type Error = i32;

    fn try_from(value: JSObject) -> Result<Self, Self::Error> {
        if value.class_id() == JS_CLASS_BOUND_FUNCTION {
            Ok(Self(value))
        } else {
            Err(-1)
        }
    }
}

impl TryFrom<JSValue> for BoundFunction {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if let JSValue::Object(o) = value {
            o.try_into()
        } else {
            Err(-1)
        }
    }
}

impl From<BoundFunction> for JSObject {
    fn from(value: BoundFunction) -> Self {
        value.0
    }
}

impl From<BoundFunction> for JSValue {
    fn from(value: BoundFunction) -> Self {
        Self::Object(value.0)
    }
}

impl BoundFunction {
    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {}

    pub fn hook_mark(val: JSValue) {
        unsafe {
            if let ObjectDetail::BOUND_FUNCTION(bf) =
                val.object_unchecked().0.detail.borrow().deref()
            {
                val.gc_mark();
                bf.func_obj.gc_mark();
                bf.this_val.gc_mark();

                for a in bf.bind_args.iter() {
                    a.gc_mark();
                }
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
        let isa = unsafe { func_obj.object_unchecked().0.detail.borrow() };

        if let ObjectDetail::BOUND_FUNCTION(bf) = isa.deref() {
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
