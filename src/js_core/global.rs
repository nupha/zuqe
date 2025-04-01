use crate::{
    context::JSContext,
    eval::{JS_EvalObject, JS_EVAL_TYPE_INDIRECT},
    function::JSCFunctionListEntry,
    value::{JSValue, JSValueConst, JS_NewBool, JS_ToFloat64},
    JS_PROP_DOUBLE_DEF, JS_PROP_UNDEFINED_DEF,
};

pub unsafe fn js_global_eval(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    JS_EvalObject(ctx, (*ctx).global_obj, *argv, JS_EVAL_TYPE_INDIRECT, -1)
}

pub unsafe fn js_global_isNaN(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let mut d: f64 = 0.0;

    // XXX: does this work for bigfloat?
    if JS_ToFloat64(ctx, &mut d, *argv.offset(0)) != 0 {
        return JSValue::EXCEPTION;
    }
    return JS_NewBool(f64::is_nan(d));
}

pub unsafe fn js_global_isFinite(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let mut d: f64 = 0.0;
    if JS_ToFloat64(ctx, &mut d, *argv.offset(0)) != 0 {
        return JSValue::EXCEPTION;
    }
    return JS_NewBool(f64::is_finite(d));
}

// global objects
pub const js_global_props: [JSCFunctionListEntry; 3] = [
    // JS_CFUNC_DEF("parseInt", 2, js_parseInt),
    // JS_CFUNC_DEF("parseFloat", 1, js_parseFloat),
    // JS_CFUNC_DEF("isNaN", 1, js_global_isNaN),
    // JS_CFUNC_DEF("isFinite", 1, js_global_isFinite),
    // JS_CFUNC_MAGIC_DEF("decodeURI", 1, js_global_decodeURI, 0),
    // JS_CFUNC_MAGIC_DEF("decodeURIComponent", 1, js_global_decodeURI, 1),
    // JS_CFUNC_MAGIC_DEF("encodeURI", 1, js_global_encodeURI, 0),
    // JS_CFUNC_MAGIC_DEF("encodeURIComponent", 1, js_global_encodeURI, 1),
    // JS_CFUNC_DEF("escape", 1, js_global_escape),
    // JS_CFUNC_DEF("unescape", 1, js_global_unescape),
    JS_PROP_DOUBLE_DEF!(c"Infinity", f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!(c"NaN", f64::NAN, 0),
    JS_PROP_UNDEFINED_DEF!(c"undefined", 0),
];
