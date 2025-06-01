use std::mem::ManuallyDrop;

use crate::{
    array::JSObjectArray,
    atom::{consts::*, JSAtom},
    context::JSContext,
    function::JSVarRef,
    gc::RefCounted,
    runtime::JS_MAX_LOCAL_VARS,
    stack_frame::JSStackFrame,
    value::{
        object::{
            js_get_length32, JSObjectIsa, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_HAS_GET,
            JS_PROP_HAS_SET, JS_PROP_VARREF, JS_PROP_WRITABLE,
        },
        JSValue, JSValueConst, JS_FreeValue, JS_TAG_OBJECT, JS_VALUE_GET_OBJ,
    },
    JS_CLASS_OBJECT,
};

use super::free_var_ref;

pub unsafe fn js_build_arguments(ctx: &mut JSContext, args: &[JSValue]) -> JSValue {
    let val = if let Ok(o) = ctx.create_instance_from_proto(
        ctx.class_proto[JS_CLASS_OBJECT as usize],
        JSObjectIsa::ARGUMENTS(JSObjectArray::default()),
    ) {
        o
    } else {
        return JSValue::EXCEPTION;
    };

    let p = val.object_unchecked();

    // add the length field (cannot fail)
    let pr = p
        .add_property(ctx, JS_ATOM_length, JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
        .unwrap();
    (*pr).u.value = JSValue::from_i32(args.len() as i32);

    // initialize fast array values
    let tab = p.fast_array_values_mut_unchecked();
    if !args.is_empty() {
        if tab.try_reserve_exact(args.len()).is_err() {
            JS_FreeValue(ctx, val);
            ctx.JS_ThrowOutOfMemory();
            return JSValue::EXCEPTION;
        }
        tab.extend(args.iter().map(|v| v.dup())); // value is duplicated
    }

    let _ = val.define_property_value(
        ctx,
        JS_ATOM_Symbol_iterator,
        ctx.array_proto_values.dup(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );

    // add callee property to throw a TypeError in strict mode
    let _ = val.define_property(
        ctx,
        JS_ATOM_callee,
        JSValue::UNDEFINED,
        ctx.throw_type_error,
        ctx.throw_type_error,
        JS_PROP_HAS_GET | JS_PROP_HAS_SET,
    );

    val
}

/// legacy arguments object: add references to the function arguments
pub unsafe fn js_build_mapped_arguments(
    ctx: &mut JSContext,
    args: &[JSValue],
    sf: &mut JSStackFrame,
    arg_count: i32,
) -> JSValue {
    let val = if let Ok(o) = ctx.create_instance_from_proto(
        ctx.class_proto[JS_CLASS_OBJECT as usize],
        JSObjectIsa::MAPPED_ARGUMENTS,
    ) {
        o
    } else {
        return JSValue::EXCEPTION;
    };

    let p = val.object_unchecked();

    // add the length field (cannot fail)
    let pr = p
        .add_property(ctx, JS_ATOM_length, JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
        .unwrap();
    (*pr).u.value = JSValue::from(args.len() as i32);

    for i in 0..arg_count {
        match sf.get_var_ref(ctx, i as _, true) {
            Ok(vr) => {
                if let Ok(pr) = p.add_property(
                    ctx,
                    JSAtom::from_u32(i as u32),
                    JS_PROP_C_W_E | JS_PROP_VARREF,
                ) {
                    (*pr).u.var_ref = ManuallyDrop::new(vr);
                } else {
                    debug_assert!(vr.cast::<JSVarRef>().as_ref().ref_count() > 1);
                    JSVarRef::free((*ctx).rt_mut(), vr.cast::<JSVarRef>());

                    JS_FreeValue(ctx, val);
                    return JSValue::EXCEPTION;
                }
            }
            Err(_) => {
                JS_FreeValue(ctx, val);
                return JSValue::EXCEPTION;
            }
        }
    }

    // the arguments not mapped to the arguments of the function can be normal properties
    for i in arg_count as usize..args.len() {
        if val
            .define_property_value_u32(ctx, i as _, args[i].dup(), JS_PROP_C_W_E)
            .is_err()
        {
            JS_FreeValue(ctx, val);
            return JSValue::EXCEPTION;
        }
    }

    let _ = val.define_property_value(
        ctx,
        JS_ATOM_Symbol_iterator,
        ctx.array_proto_values.dup(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );

    // callee returns this function in non strict mode
    let _ = val.define_property_value(
        ctx,
        JS_ATOM_callee,
        (*ctx.rt().current_stack_frame.cast::<JSStackFrame>())
            .cur_func
            .dup(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );

    val
}

pub fn js_build_rest(ctx: &mut JSContext, first: usize, args: &[JSValue]) -> JSValue {
    if let Ok(val) = ctx.create_array() {
        for (i, v) in args[first..].iter().enumerate() {
            if val
                .define_property_value_u32(ctx, (i - first) as _, v.dup(), JS_PROP_C_W_E)
                .is_err()
            {
                JS_FreeValue(ctx, val);
                return JSValue::EXCEPTION;
            }
        }
        val
    } else {
        JSValue::EXCEPTION
    }
}

// XXX: should use ValueArray
pub unsafe fn build_arg_list(
    ctx: *mut JSContext,
    array_arg: JSValueConst,
) -> Result<Box<[JSValue]>, i32> {
    if array_arg.tag() != JS_TAG_OBJECT {
        (*ctx).JS_ThrowTypeError(c"not an object");
        return Err(-1);
    }

    let len = if let Ok(n) = js_get_length32(ctx, array_arg) {
        n
    } else {
        return Err(-1);
    };
    if len > JS_MAX_LOCAL_VARS as u32 {
        (*ctx).JS_ThrowInternalError(c"too many arguments");
        return Err(-1);
    }

    let mut tab = Vec::<JSValue>::new();
    if tab.try_reserve_exact(len as usize).is_err() {
        (*ctx).JS_ThrowOutOfMemory();
        return Err(-1);
    }

    let p = JS_VALUE_GET_OBJ(array_arg);
    if let Some(src) = (*p).fast_array_values().and_then(|a| {
        if a.len() == len as usize {
            Some(a)
        } else {
            None
        }
    }) {
        // values from fast array
        tab.extend(src.iter().map(|v| v.dup()));
    } else {
        // slow array
        for i in 0..len {
            let val = array_arg.get_property_u32(ctx.as_mut().unwrap(), i);
            if val.is_exception() {
                for v in tab.drain(..) {
                    JS_FreeValue(ctx, v);
                }
                return Err(-1);
            }
            tab.push(val);
        }
    }

    Ok(tab.into_boxed_slice())
}
