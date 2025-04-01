use crate::{
    array::JS_NewArray,
    atom::{consts::*, JSAtom},
    context::JSContext,
    mem::{js_free, js_malloc, js_mallocz},
    runtime::JS_MAX_LOCAL_VARS,
    stack_frame::JSStackFrame,
    value::{
        object::{
            add_property, js_get_length32, JS_DefineProperty, JS_DefinePropertyValue,
            JS_DefinePropertyValueUint32, JS_GetPropertyUint32, JS_NewObjectProtoClass,
            JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_VARREF,
            JS_PROP_WRITABLE,
        },
        JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException, JS_TAG_OBJECT,
        JS_UNDEFINED, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_OBJECT,
};

pub unsafe fn js_build_arguments(
    ctx: *mut JSContext,
    argc: i32,
    argv: *const JSValueConst,
) -> JSValue {
    let val = JS_NewObjectProtoClass(
        ctx,
        (*ctx).class_proto[JS_CLASS_OBJECT as usize],
        JS_CLASS_ARGUMENTS,
    );
    if JS_IsException(val) {
        return val;
    }

    let p = JS_VALUE_GET_OBJ(val);

    // add the length field (cannot fail)
    let pr = add_property(
        ctx,
        p,
        JS_ATOM_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );
    (*pr).u.value = JSValue::from_i32(argc);

    // initialize the fast array part
    let tab = if argc > 0 {
        let arr = js_malloc(ctx, std::mem::size_of::<JSValue>() * argc as usize).cast::<JSValue>();
        if arr.is_null() {
            JS_FreeValue(ctx, val);
            return JSValue::EXCEPTION;
        }
        for i in 0..argc {
            *arr.offset(i as isize) = JS_DupValue(ctx, *argv.offset(i as isize));
        }
        arr
    } else {
        std::ptr::null_mut()
    };

    (*p).u.array.u.values = tab;
    (*p).u.array.count = argc as _;

    JS_DefinePropertyValue(
        ctx,
        val,
        JS_ATOM_Symbol_iterator,
        JS_DupValue(ctx, (*ctx).array_proto_values),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );
    // add callee property to throw a TypeError in strict mode
    JS_DefineProperty(
        ctx,
        val,
        JS_ATOM_callee,
        JS_UNDEFINED,
        (*ctx).throw_type_error,
        (*ctx).throw_type_error,
        JS_PROP_HAS_GET | JS_PROP_HAS_SET,
    );

    val
}

// legacy arguments object: add references to the function arguments
pub unsafe fn js_build_mapped_arguments(
    ctx: *mut JSContext,
    argc: i32,
    argv: *const JSValueConst,
    sf: *mut JSStackFrame,
    arg_count: i32,
) -> JSValue {
    let val = JS_NewObjectProtoClass(
        ctx,
        (*ctx).class_proto[JS_CLASS_OBJECT as usize],
        JS_CLASS_MAPPED_ARGUMENTS,
    );
    if JS_IsException(val) {
        return val;
    }

    let p = JS_VALUE_GET_OBJ(val);

    // add the length field (cannot fail)
    let pr = add_property(
        ctx,
        p,
        JS_ATOM_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );
    (*pr).u.value = JSValue::from_i32(argc);

    for i in 0..arg_count {
        match (*sf).get_var_ref(ctx.as_mut().unwrap(), i as _, true) {
            Ok(var_ref) => {
                let pr = add_property(
                    ctx,
                    p,
                    JSAtom::from_u32(i as u32),
                    JS_PROP_C_W_E | JS_PROP_VARREF,
                );
                if pr.is_null() {
                    if let Some(vr) = Box::from_raw(var_ref.as_ptr()).free((*ctx).rt_mut()) {
                        Box::leak(vr);
                    }

                    JS_FreeValue(ctx, val);
                    return JSValue::EXCEPTION;
                }

                (*pr).u.var_ref = var_ref.as_ptr();
            }
            Err(_) => {
                JS_FreeValue(ctx, val);
                return JSValue::EXCEPTION;
            }
        }
    }

    // the arguments not mapped to the arguments of the function can be normal properties
    for i in arg_count..argc {
        if JS_DefinePropertyValueUint32(
            ctx,
            val,
            i as _,
            JS_DupValue(ctx, *argv.offset(i as isize)),
            JS_PROP_C_W_E,
        ) < 0
        {
            // C2RS: goto fail replaced with explicit cleanup and return
            JS_FreeValue(ctx, val);
            return JSValue::EXCEPTION;
        }
    }

    JS_DefinePropertyValue(
        ctx,
        val,
        JS_ATOM_Symbol_iterator,
        JS_DupValue(ctx, (*ctx).array_proto_values),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );
    // callee returns this function in non strict mode
    JS_DefinePropertyValue(
        ctx,
        val,
        JS_ATOM_callee,
        JS_DupValue(ctx, (*(*ctx).rt().current_stack_frame).cur_func),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    );

    val
}

pub unsafe fn js_build_rest(
    ctx: *mut JSContext,
    first: i32,
    argc: i32,
    argv: *const JSValueConst,
) -> JSValue {
    let val = JS_NewArray(ctx);
    if JS_IsException(val) {
        return val;
    }

    for i in first..argc {
        let ret = JS_DefinePropertyValueUint32(
            ctx,
            val,
            (i - first) as _,
            JS_DupValue(ctx, *argv.offset(i as isize)),
            JS_PROP_C_W_E,
        );
        if ret < 0 {
            JS_FreeValue(ctx, val);
            return JSValue::EXCEPTION;
        }
    }

    val
}

// XXX: should use ValueArray
pub unsafe fn build_arg_list(
    ctx: *mut JSContext,
    plen: *mut u32,
    array_arg: JSValueConst,
) -> *mut JSValue {
    if JS_VALUE_GET_TAG(array_arg) != JS_TAG_OBJECT {
        (*ctx).JS_ThrowTypeError(c"not a object");
        return std::ptr::null_mut();
    }

    let len = if let Ok(n) = js_get_length32(ctx, array_arg) {
        n
    } else {
        return std::ptr::null_mut();
    };
    if len > JS_MAX_LOCAL_VARS as u32 {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowInternalError(c"too many arguments");
        return std::ptr::null_mut();
    }

    // avoid allocating 0 bytes
    let tab = js_mallocz(
        ctx,
        std::mem::size_of::<JSValue>() * std::cmp::max(1, len as usize),
    )
    .cast::<JSValue>();
    if tab.is_null() {
        return std::ptr::null_mut();
    }

    let p = JS_VALUE_GET_OBJ(array_arg);
    if ((*p).class_id() == JS_CLASS_ARRAY || (*p).class_id() == JS_CLASS_ARGUMENTS)
        && (*p).detailed.fast_array()
        && len == (*p).u.array.count
    {
        for i in 0..len {
            *tab.offset(i as isize) = JS_DupValue(ctx, *(*p).u.array.u.values.offset(i as isize));
        }
    } else {
        for i in 0..len {
            let ret = JS_GetPropertyUint32(ctx, array_arg, i);
            if JS_IsException(ret) {
                free_arg_list(ctx, tab, i);
                return std::ptr::null_mut();
            }
            *tab.offset(i as isize) = ret;
        }
    }

    *plen = len;
    tab
}

pub unsafe fn free_arg_list(ctx: *mut JSContext, tab: *mut JSValue, len: u32) {
    for i in 0..len {
        JS_FreeValue(ctx, *tab.add(i as _));
    }
    js_free(ctx, tab.cast());
}
