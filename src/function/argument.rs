use std::ops::Deref;

use crate::{
    JS_CLASS_OBJECT,
    array::ArrayObjPayload,
    atom::{Atom, consts::*},
    branch_hints::unlikely,
    context::JSContext,
    error::JSResult,
    object::JSObject,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_VARREF,
        JS_PROP_WRITABLE, JSProperty, ObjectPayload, js_get_length32,
    },
    runtime::JS_MAX_LOCAL_VARS,
    stack_frame::StackFrame,
    value::JSValue,
};

impl JSContext {
    pub(crate) fn build_rest(&self, first: usize, args: &[JSValue]) -> JSResult {
        let arr = self.create_array()?;
        if !args.is_empty() {
            for (i, v) in args.iter().enumerate().skip(first) {
                arr.define_property_value_u32(self, (i - first) as u32, v.clone(), JS_PROP_C_W_E)?;
            }
        }
        Ok(arr.into())
    }
}

pub fn js_build_arguments(ctx: &JSContext, args: &[JSValue]) -> JSValue {
    let obj: JSObject = if let Ok(o) = ctx.create_object_from_proto(
        ctx.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
        ObjectPayload::ARGUMENTS(ArrayObjPayload::default()),
    ) {
        o
    } else {
        return JSValue::EXCEPTION;
    };

    // add the length property (cannot fail)
    obj.add_own_property(
        ctx,
        JS_ATOM_length.deref().clone(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        JSProperty::Normal(JSValue::Int(args.len() as i32)),
    )
    .unwrap();

    if !args.is_empty() {
        // initialize fast array values
        if obj
            .with_fast_array_values_mut(|lst| -> Result<(), i32> {
                lst.try_reserve_exact(args.len()).map_err(|_| -1)?;
                lst.extend(args.iter().cloned());
                Ok(())
            })
            .unwrap()
            .is_err()
        {
            ctx.throw_out_of_memory();
            return JSValue::EXCEPTION;
        };
    }

    obj.define_property_value(
        ctx,
        JS_ATOM_Symbol_iterator.deref(),
        ctx.0.borrow().array_proto_values.clone(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )
    .unwrap();

    // LEGACY: add 'callee' property
    let throw_type_error = ctx.0.borrow().throw_type_error.clone();
    obj.define_property(
        ctx,
        JS_ATOM_callee.deref(),
        JSValue::UNDEFINED,
        throw_type_error.clone(),
        throw_type_error,
        JS_PROP_HAS_GET | JS_PROP_HAS_SET,
    )
    .unwrap();

    obj.into()
}

/// legacy arguments object: add references to the function arguments
pub fn js_build_mapped_arguments(
    ctx: &JSContext,
    args: &[JSValue],
    sf: &mut StackFrame,
    arg_count: i32,
) -> Result<JSObject, i32> {
    let obj = ctx
        .create_object_from_proto(
            ctx.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
            ObjectPayload::MAPPED_ARGUMENTS,
        )
        .map_err(|_| -1)?;

    // add the `length` property
    obj.add_own_property(
        ctx,
        JS_ATOM_length.deref().clone(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        JSProperty::Normal(JSValue::Int(args.len() as i32)),
    )?;

    for i in 0..arg_count {
        let vref = sf.get_var_ref(ctx, i as _, true)?;
        obj.add_own_property(
            ctx,
            Atom::new_small_int(i as u32),
            JS_PROP_C_W_E | JS_PROP_VARREF,
            JSProperty::VarRef(vref),
        )?;
    }

    // the arguments not mapped to the arguments of the function can be normal properties
    for (i, a) in args.iter().enumerate().skip(arg_count as usize) {
        obj.define_property_value_u32(ctx, i as _, a.clone(), JS_PROP_C_W_E)?;
    }

    obj.define_property_value(
        ctx,
        &JS_ATOM_Symbol_iterator,
        ctx.0.borrow().array_proto_values.clone(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )?;

    // callee returns this function in non strict mode
    obj.define_property_value(
        ctx,
        &JS_ATOM_callee,
        unsafe {
            (*ctx.rt().cur_stack_frame.cast::<StackFrame>())
                .cur_func
                .clone()
        },
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )?;

    Ok(obj)
}

#[deprecated(note = "use JSContext::build_rest() instead")]
pub fn js_build_rest(ctx: &JSContext, first: usize, args: &[JSValue]) -> JSValue {
    ctx.build_rest(first, args).unwrap_or(JSValue::EXCEPTION)
}

// XXX: should use ValueArray
pub fn build_arg_list(ctx: &JSContext, array_arg: &JSValue) -> Result<Box<[JSValue]>, i32> {
    if unlikely(!array_arg.is_object()) {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    } else {
        let len = js_get_length32(ctx, array_arg)?;
        if len > JS_MAX_LOCAL_VARS as u32 {
            ctx.throw_internal_error("too many arguments");
            return Err(-1);
        }

        let mut tab = Vec::<JSValue>::new();
        tab.try_reserve_exact(len as usize).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        if unsafe { array_arg.as_object_unchecked() }
            .with_fast_array_values(|a| {
                if a.len() == len as usize {
                    // fast array: clone values
                    tab.extend(a.iter().cloned());
                }
            })
            .is_none()
        {
            // slow array
            for i in 0..len {
                tab.push(array_arg.get_property_u32(ctx, i)?);
            }
        }

        Ok(tab.into_boxed_slice())
    }
}
