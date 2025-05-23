use crate::{
    atom::consts::{JS_ATOM_false, JS_ATOM_true},
    context::JSContext,
    function::JSCFunctionListEntry,
    value::{
        object::{JSObjectIsa, JS_SetObjectData},
        JSValue, JSValueConst, JS_IsException, JS_ToBool, JS_VALUE_GET_BOOL, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG,
    },
    JS_CFUNC_DEF, JS_CLASS_BOOLEAN,
};

pub unsafe fn js_boolean_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val = JS_ToBool(ctx, *argv.offset(0)).unwrap_or(true).into();
    if !new_target.is_undefined() {
        (*ctx)
            .create_instance(new_target, JSObjectIsa::BOOLEAN(val))
            .unwrap_or(JSValue::EXCEPTION)
    } else {
        val
    }
}

pub unsafe fn js_thisBooleanValue(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
    if this_val.is_bool() {
        this_val.dup()
    } else if let Some(d) = this_val
        .object()
        .and_then(|p| p.isa().value())
        .and_then(|x| if x.is_bool() { Some(x) } else { None })
    {
        d.dup()
    } else {
        (*ctx).JS_ThrowTypeError(c"not a boolean");
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_boolean_toString(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val: JSValue = js_thisBooleanValue(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }

    if JS_VALUE_GET_BOOL(val) {
        JS_ATOM_true
    } else {
        JS_ATOM_false
    }
    .to_value(ctx.as_mut().unwrap(), true)
}

pub unsafe fn js_boolean_valueOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    return js_thisBooleanValue(ctx, this_val);
}

pub const js_boolean_proto_funcs: [JSCFunctionListEntry; 2] = [
    JS_CFUNC_DEF!(c"toString", 0, js_boolean_toString),
    JS_CFUNC_DEF!(c"valueOf", 0, js_boolean_valueOf),
];
