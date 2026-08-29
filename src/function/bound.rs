// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Atom, IntOrFloat, JS_CLASS_BOUND_FUNCTION, JS_PROP_CONFIGURABLE,
    array::alloc_fast_array_slots,
    context::ScopedContext,
    error::JSResult,
    object::{BoundFunction, Object},
    string::{JS_ConcatString3, StringRef},
    value::JSValue,
};

use super::JS_CALL_FLAG_CONSTRUCTOR;

#[derive(Debug)]
pub struct BoundFunctionPayload {
    pub(crate) func_obj: Object,
    pub(crate) this_val: JSValue,
    pub(crate) bind_args: Vec<JSValue>,
}

impl GcTrace for BoundFunctionPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.func_obj.trace(tr);
        self.this_val.trace(tr);
        self.bind_args.iter().for_each(|x| x.trace(tr));
    }
}

impl BoundFunction {
    pub(crate) fn class_call(
        ctx: &mut ScopedContext,
        func_obj: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let o = unsafe { func_obj.as_object_unchecked() };
        if let Some(bf) = o.payload_bound_function() {
            let arg_count = bf.bind_args.len() + args.len();

            // if js_check_stack_overflow(ctx.rt_ref(), std::mem::size_of::<JSValue>() * arg_count) {
            //     return JS_ThrowStackOverflow(ctx);
            // }

            let mut arg_list = alloc_fast_array_slots(ctx, arg_count)?;
            // following will clone() values to arg_list
            arg_list.extend_from_slice(&bf.bind_args);
            arg_list.extend_from_slice(args);

            if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
                let mut new_target = this_obj;
                if func_obj.is_same(new_target) {
                    new_target = JSValue::from(bf.func_obj);
                }
                bf.func_obj.call_ctor_new_target(ctx, new_target, &arg_list)
            } else {
                bf.func_obj.call(ctx, bf.this_val, &arg_list)
            }
        } else {
            unreachable!()
        }
    }

    /// create a bound function
    pub fn create(
        ctx: &mut ScopedContext,
        mut func_obj: Object,
        this_val: JSValue,
        bind_args: Vec<JSValue>,
    ) -> Result<Self, i32> {
        let arg_count = bind_args.len();

        let mut bound = {
            let p = ctx.function_proto();
            ctx.create_instance_with_proto_class(
                Some(p),
                JS_CLASS_BOUND_FUNCTION,
                Box::into_raw(Box::new(BoundFunctionPayload {
                    func_obj,
                    this_val,
                    bind_args,
                })) as *mut c_void,
            )
            .map_err(|_| -1)?
        };

        debug_assert_eq!(bound.class_id(), JS_CLASS_BOUND_FUNCTION);

        bound.set_flag_constructor(func_obj.is_constructor());

        // set length property
        // XXX: the spec could be simpler by only using GetOwnProperty
        let len_val = if func_obj
            .get_own_property(ctx, Atom::CONST_length)?
            .is_none()
        {
            JSValue::ZERO
        } else {
            match func_obj.get_property(ctx, Atom::CONST_length)?.as_number() {
                Some(IntOrFloat::Int64(n)) => JSValue::from(std::cmp::max(0, n - arg_count as i64)),
                Some(IntOrFloat::Float64(mut d)) => {
                    if d.is_nan() {
                        d = 0.0;
                    } else {
                        d = d.trunc();
                        if d <= arg_count as f64 {
                            d = 0.0;
                        } else {
                            d -= arg_count as f64; // also converts -0 to +0 
                        }
                    }
                    JSValue::from(d)
                }
                _ => JSValue::ZERO,
            }
        };

        bound.define_property_value(ctx, Atom::CONST_length, len_val, JS_PROP_CONFIGURABLE)?;

        // set bound function name property
        let name: StringRef = if let Some(s) = func_obj
            .get_property(ctx, Atom::CONST_name)?
            .as_string(true)
        {
            s
        } else {
            Atom::CONST_empty_string.force_string(ctx)?
        };

        let bf_name = JS_ConcatString3(ctx, c"bound ", JSValue::make_string(name), c"")?;
        bound.define_property_value(
            ctx,
            Atom::CONST_name,
            JSValue::make_string(bf_name),
            JS_PROP_CONFIGURABLE,
        )?;

        Ok(Self(bound.gc_ref()))
    }
}
