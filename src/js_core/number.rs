use crate::{
    class::js_create_from_ctor,
    context::JSContext,
    error::{JSResult, JS_ThrowRangeError},
    function::{CallArgs, JSCFunctionListEntry},
    value::{
        is_safe_integer, js_dtoa, object::JS_SetObjectData, JSValue, JSValueConst, JS_DupValue,
        JS_FreeValue, JS_IsException, JS_IsNumber, JS_IsUndefined, JS_ToFloat64, JS_ToFloat64Free,
        JS_ToInt32Sat, JS_ToNumeric, JS_ToStringFree, JS_DTOA_FIXED_FORMAT, JS_DTOA_FORCE_EXP,
        JS_DTOA_FRAC_FORMAT, JS_DTOA_VAR_FORMAT, JS_TAG_BIG_INT, JS_TAG_OBJECT, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG,
    },
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CFUNC_NATIVE_DEF, JS_CLASS_NUMBER, JS_PROP_DOUBLE_DEF,
};

use super::global::{js_global_isNaN_isFinite, MAGIC_NUM_IS_FINITE, MAGIC_NUM_IS_NAN};

pub unsafe fn js_thisNumberValue(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
    if JS_IsNumber(this_val) {
        return JS_DupValue(ctx, this_val);
    }

    if JS_VALUE_GET_TAG(this_val) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(this_val);
        if (*p).class_id() == JS_CLASS_NUMBER {
            if JS_IsNumber((*p).u.object_data) {
                return JS_DupValue(ctx, (*p).u.object_data);
            }
        }
    }

    (*ctx).JS_ThrowTypeError(c"not a number")
}

pub unsafe fn js_number_valueOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    js_thisNumberValue(ctx, this_val)
}

pub unsafe fn js_get_radix(ctx: *mut JSContext, val: JSValueConst) -> i32 {
    let mut radix: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut radix, val) != 0 {
        return -1;
    }
    if !(2..=36).contains(&radix) {
        JS_ThrowRangeError(ctx, c"radix must be between 2 and 36");
        return -1;
    }
    radix
}

pub unsafe fn js_number_toString(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let val = js_thisNumberValue(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }

    let base = if magic != 0 || JS_IsUndefined(*argv.offset(0)) {
        10
    } else {
        let base = js_get_radix(ctx, unsafe { *argv.offset(0) });
        if base < 0 {
            JS_FreeValue(ctx, val);
            return JSValue::EXCEPTION;
        }
        base
    };

    let mut d: f64 = 0.0;
    if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
        JSValue::EXCEPTION
    } else {
        js_dtoa(ctx, d, base, 0, JS_DTOA_VAR_FORMAT)
    }
}

pub unsafe fn js_number_toFixed(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val = js_thisNumberValue(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }

    let mut d: f64 = 0.0;
    if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
        return JSValue::EXCEPTION;
    }

    let mut f: i32 = 0;
    if JS_ToInt32Sat(ctx, &mut f, unsafe { *argv.offset(0) }) != 0 {
        return JSValue::EXCEPTION;
    }
    if f < 0 || f > 100 {
        return JS_ThrowRangeError(ctx, c"invalid number of digits");
    }

    if d.abs() >= 1e21 {
        JS_ToStringFree(ctx, JSValue::from_f64_as_is(d))
    } else {
        js_dtoa(ctx, d, 10, f, JS_DTOA_FRAC_FORMAT)
    }
}

pub unsafe fn js_number_toExponential(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val = js_thisNumberValue(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }

    let mut f: i32 = 0;
    let mut d: f64 = 0.0;

    if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
        return JSValue::EXCEPTION;
    }
    if JS_ToInt32Sat(ctx, &mut f, *argv.offset(0)) != 0 {
        return JSValue::EXCEPTION;
    }
    if !d.is_finite() {
        return JS_ToStringFree(ctx, JSValue::from_f64_as_is(d));
    }

    let flags = if JS_IsUndefined(unsafe { *argv.offset(0) }) {
        f = 0;
        0
    } else {
        if !(0..=100).contains(&f) {
            return JS_ThrowRangeError(ctx, c"invalid number of digits");
        }
        f += 1;
        JS_DTOA_FIXED_FORMAT
    };

    js_dtoa(ctx, d, 10, f, flags | JS_DTOA_FORCE_EXP)
}

pub unsafe fn js_number_toPrecision(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let mut p: i32 = 0;
    let mut d: f64 = 0.0;

    let val = js_thisNumberValue(ctx, this_val);
    if JS_IsException(val) {
        return val;
    }
    if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
        return JSValue::EXCEPTION;
    }
    if JS_IsUndefined(*argv.offset(0)) {
        // C2RS: goto to_string; replaced with direct control flow
        return JS_ToStringFree(ctx, JSValue::from_f64_as_is(d));
    }
    if JS_ToInt32Sat(ctx, &mut p, *argv.offset(0)) != 0 {
        return JSValue::EXCEPTION;
    }
    if !d.is_finite() {
        return JS_ToStringFree(ctx, JSValue::from_f64_as_is(d));
    }
    if !(1..=100).contains(&p) {
        return JS_ThrowRangeError(ctx, c"invalid number of digits");
    }

    js_dtoa(ctx, d, 10, p, JS_DTOA_FIXED_FORMAT)
}

pub const js_number_proto_funcs: [JSCFunctionListEntry; 6] = [
    JS_CFUNC_DEF!(c"toExponential", 1, js_number_toExponential),
    JS_CFUNC_DEF!(c"toFixed", 1, js_number_toFixed),
    JS_CFUNC_DEF!(c"toPrecision", 1, js_number_toPrecision),
    JS_CFUNC_MAGIC_DEF!(c"toString", 1, js_number_toString, 0),
    JS_CFUNC_MAGIC_DEF!(c"toLocaleString", 0, js_number_toString, 1),
    JS_CFUNC_DEF!(c"valueOf", 0, js_number_valueOf),
];

pub fn js_number_isNaN_isFinite(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    if !JS_IsNumber(args.get_raw(0)) {
        Ok(JSValue::FALSE)
    } else {
        js_global_isNaN_isFinite(ctx, args)
    }
}

// pub unsafe fn js_number_isNaN(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     if !JS_IsNumber(*argv.offset(0)) {
//         JSValue::FALSE
//     } else {
//         js_global_isNaN(ctx, this_val, argc, argv)
//     }
// }
// pub unsafe fn js_number_isFinite(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     if !JS_IsNumber(*argv.offset(0)) {
//         JSValue::FALSE
//     } else {
//         js_global_isFinite(ctx, this_val, argc, argv)
//     }
// }

pub unsafe fn js_number_isInteger(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if let Ok(b) = (*argv.offset(0)).is_integer(ctx.as_mut().unwrap()) {
        b.into()
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_number_isSafeInteger(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if !JS_IsNumber(*argv.offset(0)) {
        return JSValue::FALSE;
    }

    let mut d: f64 = 0.0;
    if JS_ToFloat64(ctx, &mut d, *argv.offset(0)) != 0 {
        return JSValue::EXCEPTION;
    }

    is_safe_integer(d).into()
}

pub const js_number_funcs: [JSCFunctionListEntry; 12] = [
    // global ParseInt and parseFloat should be defined already or delayed
    // JS_ALIAS_BASE_DEF("parseInt", "parseInt", 0 ),
    // JS_ALIAS_BASE_DEF("parseFloat", "parseFloat", 0 ),
    JS_CFUNC_NATIVE_DEF!(c"isNaN", 1, js_number_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    JS_CFUNC_NATIVE_DEF!(
        c"isFinite",
        1,
        js_number_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _
    ),
    JS_CFUNC_DEF!(c"isInteger", 1, js_number_isInteger),
    JS_CFUNC_DEF!(c"isSafeInteger", 1, js_number_isSafeInteger),
    JS_PROP_DOUBLE_DEF!(c"MAX_VALUE", 1.797_693_134_862_315_7e308, 0),
    JS_PROP_DOUBLE_DEF!(c"MIN_VALUE", 5e-324, 0),
    JS_PROP_DOUBLE_DEF!(c"NaN", f64::NAN, 0),
    JS_PROP_DOUBLE_DEF!(c"NEGATIVE_INFINITY", -f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!(c"POSITIVE_INFINITY", f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!(c"EPSILON", 2.220446049250313e-16, 0), /* ES6 */
    JS_PROP_DOUBLE_DEF!(c"MAX_SAFE_INTEGER", 9007199254740991.0, 0), /* ES6 */
    JS_PROP_DOUBLE_DEF!(c"MIN_SAFE_INTEGER", -9007199254740991.0, 0), /* ES6 */
];

pub unsafe fn js_number_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let val: JSValue;
    if argc == 0 {
        val = JSValue::ZERO;
    } else {
        val = JS_ToNumeric(ctx, *argv.offset(0));
        if JS_IsException(val) {
            return val;
        }
        if JS_VALUE_GET_TAG(val) == JS_TAG_BIG_INT {
            (*ctx).JS_ThrowTypeError(c"BigInt is not supported");
            return JSValue::EXCEPTION;
        }
    }

    if !JS_IsUndefined(new_target) {
        let obj = js_create_from_ctor(ctx, new_target, JS_CLASS_NUMBER);
        if !JS_IsException(obj) {
            JS_SetObjectData(ctx, obj, val);
        }
        obj
    } else {
        val
    }
}
