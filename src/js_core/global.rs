use crate::{
    JS_CFUNC_NATIVE_DEF, JS_PROP_DOUBLE_DEF, JS_PROP_UNDEFINED_DEF,
    context::JSContext,
    error::JSResult,
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{CallContext, NativeFieldDef},
    value::{JS_ToFloat64, JSValue, js_parseFloat, js_parseInt},
};

pub fn js_global_eval(ctx: &JSContext, cc: CallContext) -> JSResult {
    // to avoid ctx borrow conflict, fetch global object in advance
    let global = JSValue::Object(ctx.global_obj());
    cc.arg(0)
        .eval_object(ctx, global, JS_EVAL_TYPE_INDIRECT, -1)
        .check_exception()
}

pub const MAGIC_NUM_IS_NAN: i32 = 1;
pub const MAGIC_NUM_IS_FINITE: i32 = 2;

pub fn js_global_isNaN_isFinite(ctx: &JSContext, args: CallContext) -> JSResult {
    let mut d: f64 = 0.0;

    if unsafe { JS_ToFloat64(ctx, &mut d, args.arg(0)) } != 0 {
        Err(-1)
    } else {
        let b = match args.magic() {
            MAGIC_NUM_IS_NAN => f64::is_nan(d),
            MAGIC_NUM_IS_FINITE => f64::is_finite(d),
            _ => unreachable!(),
        };
        Ok(JSValue::Bool(b))
    }
}

// global objects
pub const js_global_props: [NativeFieldDef; 7] = [
    JS_CFUNC_NATIVE_DEF!("parseInt", 2, js_parseInt, 0),
    JS_CFUNC_NATIVE_DEF!("parseFloat", 1, js_parseFloat, 0),
    JS_CFUNC_NATIVE_DEF!("isNaN", 1, js_global_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    JS_CFUNC_NATIVE_DEF!(
        "isFinite",
        1,
        js_global_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _
    ),
    // JS_CFUNC_MAGIC_DEF("decodeURI", 1, js_global_decodeURI, 0),
    // JS_CFUNC_MAGIC_DEF("decodeURIComponent", 1, js_global_decodeURI, 1),
    // JS_CFUNC_MAGIC_DEF("encodeURI", 1, js_global_encodeURI, 0),
    // JS_CFUNC_MAGIC_DEF("encodeURIComponent", 1, js_global_encodeURI, 1),
    // JS_CFUNC_DEF("escape", 1, js_global_escape),
    // JS_CFUNC_DEF("unescape", 1, js_global_unescape),
    JS_PROP_DOUBLE_DEF!("Infinity", f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!("NaN", f64::NAN, 0),
    JS_PROP_UNDEFINED_DEF!("undefined", 0),
];
