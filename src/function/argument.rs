use crate::{
    JS_CLASS_OBJECT,
    array::JSObjectArray,
    atom::{JSAtom, consts::*},
    context::JSContext,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_VARREF,
        JS_PROP_WRITABLE, JSObjectIsa, JSProperty, js_get_length32,
    },
    runtime::JS_MAX_LOCAL_VARS,
    stack_frame::JSStackFrame,
    value::JSValue,
};

pub unsafe fn js_build_arguments(ctx: &JSContext, args: &[JSValue]) -> JSValue {
    let val = if let Ok(o) = ctx.create_instance_from_proto(
        &ctx.0.borrow().class_proto[JS_CLASS_OBJECT as usize],
        JSObjectIsa::ARGUMENTS(JSObjectArray::default()),
    ) {
        o
    } else {
        return JSValue::EXCEPTION;
    };

    let p = val.object_unchecked();

    // add the length property (cannot fail)
    p.add_shape_property_then(
        ctx,
        JS_ATOM_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        |pr| {
            *pr = JSProperty::Normal(JSValue::Int(args.len() as i32));
        },
    )
    .unwrap();

    if !args.is_empty() {
        // initialize fast array values
        if p.with_fast_array_values_mut(|lst| -> Result<(), i32> {
            lst.try_reserve_exact(args.len()).map_err(|_| -1)?;
            lst.extend(args.iter().map(|v| v.clone()));
            Ok(())
        })
        .unwrap()
        .is_err()
        {
            ctx.JS_ThrowOutOfMemory();
            return JSValue::EXCEPTION;
        };
    }

    let _ = val.define_property_value(
        ctx,
        JS_ATOM_Symbol_iterator,
        ctx.0.borrow().array_proto_values.clone(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );

    // add callee property to throw a TypeError in strict mode
    let throw_type_error = &ctx.0.borrow().throw_type_error;
    let _ = val.define_property(
        ctx,
        JS_ATOM_callee,
        &JSValue::UNDEFINED,
        throw_type_error,
        throw_type_error,
        JS_PROP_HAS_GET | JS_PROP_HAS_SET,
    );

    val
}

/// legacy arguments object: add references to the function arguments
pub unsafe fn js_build_mapped_arguments(
    ctx: &JSContext,
    args: &[JSValue],
    sf: &mut JSStackFrame,
    arg_count: i32,
) -> JSValue {
    let val = if let Ok(o) = ctx.create_instance_from_proto(
        &ctx.0.borrow().class_proto[JS_CLASS_OBJECT as usize],
        JSObjectIsa::MAPPED_ARGUMENTS,
    ) {
        o
    } else {
        return JSValue::EXCEPTION;
    };

    let p = val.object_unchecked();

    // add the length property (cannot fail)
    p.add_shape_property_then(
        ctx,
        JS_ATOM_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        |pr| {
            *pr = JSProperty::Normal(JSValue::Int(args.len() as i32));
        },
    )
    .unwrap();

    for i in 0..arg_count {
        match sf.get_var_ref(ctx, i as _, true) {
            Ok(vr) => {
                if p.add_shape_property_then(
                    ctx,
                    JSAtom::new_u32(i as u32),
                    JS_PROP_C_W_E | JS_PROP_VARREF,
                    |pr| {
                        *pr = JSProperty::VarRef(vr);
                    },
                )
                .is_err()
                {
                    return JSValue::EXCEPTION;
                }
            }
            Err(_) => {
                return JSValue::EXCEPTION;
            }
        }
    }

    // the arguments not mapped to the arguments of the function can be normal properties
    for i in arg_count as usize..args.len() {
        if val
            .define_property_value_u32(ctx, i as _, args[i].clone(), JS_PROP_C_W_E)
            .is_err()
        {
            return JSValue::EXCEPTION;
        }
    }

    let _ = val.define_property_value(
        ctx,
        JS_ATOM_Symbol_iterator,
        ctx.0.borrow().array_proto_values.clone(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );

    // callee returns this function in non strict mode
    let _ = val.define_property_value(
        ctx,
        JS_ATOM_callee,
        (*ctx.rt().current_stack_frame.cast::<JSStackFrame>())
            .cur_func
            .clone(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );

    val
}

pub fn js_build_rest(ctx: &JSContext, first: usize, args: &[JSValue]) -> JSValue {
    if let Ok(val) = ctx.create_array() {
        for (i, v) in args[first..].iter().enumerate() {
            if val
                .define_property_value_u32(ctx, (i - first) as _, v.clone(), JS_PROP_C_W_E)
                .is_err()
            {
                return JSValue::EXCEPTION;
            }
        }
        val
    } else {
        JSValue::EXCEPTION
    }
}

// XXX: should use ValueArray
pub fn build_arg_list(ctx: &JSContext, array_arg: &JSValue) -> Result<Box<[JSValue]>, i32> {
    if !array_arg.is_object() {
        ctx.JS_ThrowTypeError("not an object");
        return Err(-1);
    }

    let len = js_get_length32(ctx, array_arg)?;
    if len > JS_MAX_LOCAL_VARS as u32 {
        ctx.JS_ThrowInternalError("too many arguments");
        return Err(-1);
    }

    let mut tab = Vec::<JSValue>::new();
    tab.try_reserve_exact(len as usize).map_err(|_| {
        ctx.JS_ThrowOutOfMemory();
        -1
    })?;

    if unsafe { array_arg.object_unchecked() }
        .with_fast_array_values(|a| {
            if a.len() == len as usize {
                // fast array: clone values
                tab.extend(a.iter().map(|v| v.clone()));
            }
        })
        .is_none()
    {
        // slow array
        for i in 0..len {
            tab.push(array_arg.get_property_u32(ctx, i).check_exception()?);
        }
    }

    Ok(tab.into_boxed_slice())
}
