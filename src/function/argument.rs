// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_OBJECT,
    array::{ArrayPayload, alloc_fast_array_slots},
    atom::Atom,
    context::ScopedContext,
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_WRITABLE, JSProperty,
        ObjectPayload,
    },
    value::JSValue,
};

use super::JS_MAX_LOCAL_VARS;

/// Make Arguments class instance with values.
pub(crate) fn js_build_arguments(ctx: &mut ScopedContext, values: &[JSValue]) -> JSValue {
    let object_proto = ctx.class_proto(JS_CLASS_OBJECT);
    let mut obj = if let Ok(o) = ctx.create_instance_with_proto_payload(
        object_proto,
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
        let arr_data = obj.as_Array_mut().unwrap().payload_mut();
        if arr_data.reserve(values.len()).is_err() {
            ctx.throw_out_of_memory();
            return JSValue::EXCEPTION;
        }

        arr_data.extend(values.iter().copied());
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

pub(crate) fn build_arg_list(
    ctx: &mut ScopedContext,
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

    let mut lst = alloc_fast_array_slots(ctx, length as usize)?;

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
