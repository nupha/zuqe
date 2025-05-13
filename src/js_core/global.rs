use crate::{
    context::JSContext,
    error::JSResult,
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{CallArgs, JSCFunctionListEntry},
    value::{js_parseFloat, js_parseInt, JSValue, JS_ToFloat64},
    JS_CFUNC_NATIVE_DEF, JS_PROP_DOUBLE_DEF, JS_PROP_UNDEFINED_DEF,
};

pub fn js_global_eval(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    args.get_raw(0)
        .eval_object(ctx, ctx.global_obj, JS_EVAL_TYPE_INDIRECT, -1)
        .check_exception()
}

pub const MAGIC_NUM_IS_NAN: i32 = 1;
pub const MAGIC_NUM_IS_FINITE: i32 = 2;

pub fn js_global_isNaN_isFinite(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let mut d: f64 = 0.0;

    if unsafe { JS_ToFloat64(ctx, &mut d, args.get_raw(0)) } != 0 {
        Err(-1)
    } else {
        let b = match args.magic {
            MAGIC_NUM_IS_NAN => f64::is_nan(d),
            MAGIC_NUM_IS_FINITE => f64::is_finite(d),
            _ => unreachable!(),
        };
        Ok(if b { JSValue::TRUE } else { JSValue::FALSE })
    }
}

// global objects
pub const js_global_props: [JSCFunctionListEntry; 7] = [
    JS_CFUNC_NATIVE_DEF!(c"parseInt", 2, js_parseInt, 0),
    JS_CFUNC_NATIVE_DEF!(c"parseFloat", 1, js_parseFloat, 0),
    JS_CFUNC_NATIVE_DEF!(c"isNaN", 1, js_global_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    JS_CFUNC_NATIVE_DEF!(
        c"isFinite",
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
    JS_PROP_DOUBLE_DEF!(c"Infinity", f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!(c"NaN", f64::NAN, 0),
    JS_PROP_UNDEFINED_DEF!(c"undefined", 0),
];
