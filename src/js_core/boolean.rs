use crate::{
    atom::consts::{JS_ATOM_false, JS_ATOM_true},
    class::js_create_from_ctor,
    context::JSContext,
    function::JSCFunctionListEntry,
    value::{
        object::JS_SetObjectData, JSValue, JSValueConst, JS_IsException, JS_IsUndefined,
        JS_NewBool, JS_ToBool, JS_TAG_BOOL, JS_TAG_OBJECT, JS_VALUE_GET_BOOL, JS_VALUE_GET_OBJ,
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
    let val = JS_NewBool(JS_ToBool(ctx, *argv.offset(0)).unwrap_or(true));
    if !JS_IsUndefined(new_target) {
        let obj = js_create_from_ctor(ctx, new_target, JS_CLASS_BOOLEAN);
        if !JS_IsException(obj) {
            JS_SetObjectData(ctx, obj, val);
        }
        obj
    } else {
        val
    }
}

pub unsafe fn js_thisBooleanValue(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
    if JS_VALUE_GET_TAG(this_val) == JS_TAG_BOOL {
        return this_val.dup();
    }

    if JS_VALUE_GET_TAG(this_val) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(this_val);
        if (*p).class_id() == JS_CLASS_BOOLEAN {
            if JS_VALUE_GET_TAG((*p).u.object_data) == JS_TAG_BOOL {
                return (*p).u.object_data;
            }
        }
    }

    (*ctx).JS_ThrowTypeError(c"not a boolean")
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
