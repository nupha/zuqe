use crate::{
    atom::{consts::*, JSAtom},
    context::JSContext,
    function::JS_CallFree,
    gc::{JS_MarkFunc, JS_MarkValue},
    runtime::JSRuntime,
    value::{
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_FreeValueRT, JS_IsException,
        JS_IsNull, JS_IsUndefined, HINT_FORCE_ORDINARY, HINT_NUMBER, HINT_STRING, JS_TAG_BIG_INT,
        JS_TAG_BOOL, JS_TAG_EXCEPTION, JS_TAG_INT, JS_TAG_IS_FLOAT64, JS_TAG_NULL, JS_TAG_OBJECT,
        JS_TAG_STRING, JS_TAG_UNDEFINED, JS_UNDEFINED, JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT,
        JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_STRING, JS_VALUE_GET_TAG,
    },
    JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING, JS_CLASS_SYMBOL, JS_MKVAL,
};

use super::object::{JSObject, JS_GetProperty, JS_IsFunction};

#[inline(always)]
pub fn JS_NewBool(val: bool) -> JSValue {
    if val {
        JSValue::TRUE
    } else {
        JSValue::FALSE
    }
}

#[inline(always)]
pub const fn JS_NewCatchOffset(val: i32) -> JSValue {
    JS_MKVAL!(JSTag::CatchOffset, val)
}

pub unsafe fn JS_ToPrimitiveFree(ctx: *mut JSContext, val: JSValue, mut hint: i32) -> JSValue {
    let force_ordinary = (hint & HINT_FORCE_ORDINARY) != 0;
    hint &= !HINT_FORCE_ORDINARY;

    if !force_ordinary {
        let method = JS_GetProperty(ctx, val, JS_ATOM_Symbol_toPrimitive);
        if JS_IsException(method) {
            return goto_exception(ctx, val);
        }

        /* ECMA says *If exoticToPrim is not undefined* but tests in
        test262 use null as a non callable converter */
        if !JS_IsUndefined(method) && !JS_IsNull(method) {
            let mut arg = match hint {
                HINT_STRING => JS_ATOM_string,
                HINT_NUMBER => JS_ATOM_number,
                _ => JS_ATOM_default,
            }
            .to_value(ctx.as_mut().unwrap(), true);

            let ret = JS_CallFree(ctx, method, val, 1, &mut arg);
            JS_FreeValue(ctx, arg);
            if JS_IsException(ret) {
                return goto_exception(ctx, val);
            }

            JS_FreeValue(ctx, val);
            if JS_VALUE_GET_TAG(ret) != JS_TAG_OBJECT {
                return ret;
            }
            JS_FreeValue(ctx, ret);
            return (*ctx).JS_ThrowTypeError(c"toPrimitive");
        }
    }

    if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
        return val;
    }

    if hint != HINT_STRING {
        hint = HINT_NUMBER;
    };

    for i in 0..2 {
        let method_name: JSAtom = if (i ^ hint) == 0 {
            JS_ATOM_toString
        } else {
            JS_ATOM_valueOf
        };
        let method = JS_GetProperty(ctx, val, method_name);
        if JS_IsException(method) {
            return goto_exception(ctx, val);
        }

        if JS_IsFunction(ctx, method) {
            let ret = JS_CallFree(ctx, method, val, 0, std::ptr::null_mut());
            if JS_IsException(ret) {
                return goto_exception(ctx, val);
            }
            if JS_VALUE_GET_TAG(ret) != JS_TAG_OBJECT {
                JS_FreeValue(ctx, val);
                return ret;
            }
            JS_FreeValue(ctx, ret);
        } else {
            JS_FreeValue(ctx, method);
        }
    }

    ctx.as_mut().unwrap().JS_ThrowTypeError(c"toPrimitive");

    return goto_exception(ctx, val);

    // C2RS: 模拟goto跳转
    unsafe fn goto_exception(ctx: *mut JSContext, val: JSValue) -> JSValue {
        JS_FreeValue(ctx, val);
        JSValue::EXCEPTION
    }
}

pub unsafe fn JS_ToPrimitive(ctx: *mut JSContext, val: *const JSValue, hint: i32) -> JSValue {
    JS_ToPrimitiveFree(ctx, (*val).dup(), hint)
}

/* Only works for primitive types, otherwise return JS_NULL. */
pub unsafe fn JS_GetPrototypePrimitive(ctx: *const JSContext, val: JSValueConst) -> JSValueConst {
    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::BigFloat | JSTag::BigDecimal | JSTag::BigInt => {
            unimplemented!()
        }
        JSTag::Int | JSTag::Float64 => (*ctx).class_proto[JS_CLASS_NUMBER as usize],
        JSTag::Bool => (*ctx).class_proto[JS_CLASS_BOOLEAN as usize],
        JSTag::String => (*ctx).class_proto[JS_CLASS_STRING as usize],
        JSTag::Symbol => (*ctx).class_proto[JS_CLASS_SYMBOL as usize],
        JSTag::Object | JSTag::Null | JSTag::Undefined => JSValue::NULL,
        _ => JSValue::NULL,
    }
}

pub unsafe fn JS_ToBoolFree(ctx: *mut JSContext, val: JSValue) -> Result<bool, i32> {
    match JS_VALUE_GET_TAG(val) {
        JS_TAG_NULL | JS_TAG_UNDEFINED => Ok(false),
        JS_TAG_INT | JS_TAG_BOOL => Ok(JS_VALUE_GET_INT(val) != 0),
        JS_TAG_EXCEPTION => Err(-1),
        JS_TAG_STRING => {
            let ret = !(*JS_VALUE_GET_STRING(val)).is_empty();
            JS_FreeValue(ctx, val);
            Ok(ret)
        }
        JS_TAG_BIG_INT => {
            // let p = JS_VALUE_GET_PTR(val) as *mut JSBigFloat;
            // let ret;
            // ret = (*p).num.expn != BF_EXP_ZERO && (*p).num.expn != BF_EXP_NAN;
            // JS_FreeValue(ctx, val);
            // ret as i32
            unimplemented!()
        }
        JS_TAG_OBJECT => {
            let p = JS_VALUE_GET_OBJ(val);
            let ret = !(*p).detailed.is_htmldda();
            JS_FreeValue(ctx, val);
            Ok(ret)
        }
        tag => {
            if JS_TAG_IS_FLOAT64(tag) {
                let d = JS_VALUE_GET_FLOAT64(val);
                Ok(!d.is_nan() && d != 0.0)
            } else {
                JS_FreeValue(ctx, val);
                Ok(true)
            }
        }
    }
}

pub unsafe fn JS_ToBool(ctx: *mut JSContext, val: JSValueConst) -> Result<bool, i32> {
    JS_ToBoolFree(ctx, JS_DupValue(ctx, val))
}

pub unsafe fn js_object_data_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p: *mut JSObject = JS_VALUE_GET_OBJ(val);
    JS_FreeValueRT(rt, (*p).u.object_data);
    (*p).u.object_data = JS_UNDEFINED;
}

pub unsafe fn js_object_data_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let p = JS_VALUE_GET_OBJ(val);
    JS_MarkValue(rt, (*p).u.object_data, mark_func);
}
