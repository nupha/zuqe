// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_OBJECT,
    atom::Atom,
    builtins::array::ArrayPayload,
    context::ContextImpl,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_VARREF,
        JS_PROP_WRITABLE, JSProperty, Object, ObjectPayload,
    },
    stack_frame::StackFrame,
    value::JSValue,
};

use super::JS_MAX_LOCAL_VARS;

/// Make Arguments class instance with values.
pub(crate) fn js_build_arguments(ctx: &mut ContextImpl, values: &[JSValue]) -> JSValue {
    let mut obj = if let Ok(o) = ctx.create_object_with_proto(
        ctx.class_proto(JS_CLASS_OBJECT),
        ObjectPayload::ARGUMENTS(ArrayPayload::default()),
    ) {
        o
    } else {
        return JSValue::EXCEPTION;
    };

    // add the length property (cannot fail)
    obj.add_own_property(
        ctx,
        Atom::CONST_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        JSProperty::Normal(JSValue::Int(values.len() as i32)),
    )
    .unwrap();

    if !values.is_empty() && obj.is_fast_array() {
        // initialize fast array values
        let xref = obj.gc_node().partition_id();
        let arr_data = obj.as_Array_mut().unwrap().payload_mut();
        if arr_data.reserve(values.len()).is_err() {
            ctx.throw_out_of_memory();
            return JSValue::EXCEPTION;
        }

        arr_data.extend(&mut ctx.rt_ptr(), values.iter().copied(), xref);
    }

    #[cfg(feature = "builtin_symbol")]
    obj.define_property_value(
        ctx,
        Atom::CONST_Symbol_iterator,
        ctx.array_proto_values,
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )
    .unwrap();

    // LEGACY: add 'callee' property
    let throw_type_error = ctx.throw_type_error;
    obj.define_property(
        ctx,
        Atom::CONST_callee,
        JSValue::UNDEFINED,
        throw_type_error,
        throw_type_error,
        JS_PROP_HAS_GET | JS_PROP_HAS_SET,
    )
    .unwrap();

    obj.into()
}

/// legacy arguments object: add references to the function arguments
pub(crate) fn js_build_mapped_arguments(
    ctx: &mut ContextImpl,
    args: &[JSValue],
    sf: &mut StackFrame,
    arg_count: i32,
) -> Result<Object, i32> {
    let mut obj = ctx
        .create_object_with_proto(
            ctx.class_proto(JS_CLASS_OBJECT),
            ObjectPayload::MAPPED_ARGUMENTS,
        )
        .map_err(|_| -1)?;

    // add the `length` property
    obj.add_own_property(
        ctx,
        Atom::CONST_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        JSProperty::Normal(JSValue::Int(args.len() as i32)),
    )?;

    for i in 0..arg_count {
        let content = JSProperty::VarRef(sf.get_var_ref(ctx, i as _, true)?);
        obj.add_own_property(
            ctx,
            Atom::new_small_int(i as u32),
            JS_PROP_C_W_E | JS_PROP_VARREF,
            content,
        )?;
    }

    // the arguments not mapped to the arguments of the function can be normal properties
    for (i, a) in args.iter().enumerate().skip(arg_count as usize) {
        obj.define_property_value_u32(ctx, i as _, *a, JS_PROP_C_W_E)?;
    }

    #[cfg(feature = "builtin_symbol")]
    obj.define_property_value(
        ctx,
        Atom::CONST_Symbol_iterator,
        ctx.array_proto_values,
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )?;

    // callee returns this function in non strict mode
    obj.define_property_value(
        ctx,
        Atom::CONST_callee,
        ctx.rt_ptr().current_stack_frame().unwrap().func_obj.into(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )?;

    Ok(obj)
}

pub(crate) fn build_arg_list(
    ctx: &mut ContextImpl,
    array_arg: JSValue,
) -> Result<Box<[JSValue]>, i32> {
    let Some(array_arg) = array_arg.as_object().copied() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let length = array_arg.get_length_prop(ctx)? as u32;
    if length > JS_MAX_LOCAL_VARS as u32 {
        ctx.throw_internal_error("too many arguments");
        return Err(-1);
    }

    let mut lst = Vec::<JSValue>::new();
    lst.try_reserve_exact(length as usize).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    if array_arg.is_fast_array()
        && let Some(arr_data) = array_arg.payload().array()
    {
        // fast array
        if arr_data.len() == length as usize {
            lst.extend(arr_data.values().iter().copied());
        }
    } else {
        // slow path
        for i in 0..length {
            lst.push(array_arg.get_property_u32(ctx, i)?);
        }
    }

    Ok(lst.into_boxed_slice())
}
