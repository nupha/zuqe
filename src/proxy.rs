use crate::{
    array::JS_IsArray,
    atom::{consts::*, JSAtom},
    context::JSContext,
    error::{JS_ThrowStackOverflow, JS_ThrowTypeErrorRevokedProxy},
    function::JS_CallFree,
    gc::GCMarkFunc,
    runtime::{js_check_stack_overflow, JSRuntime},
    value::{
        object::{
            JSObjectIsa, JS_GetPrototype, JS_IsExtensible, JS_PreventExtensions,
            JS_SetPrototypeInternal,
        },
        JSValue, JSValueConst, JS_FreeValue, JS_IsException, JS_IsUndefined, JS_ToBoolFree,
        JS_TAG_NULL, JS_TAG_OBJECT, JS_UNDEFINED, JS_VALUE_GET_OBJ,
    },
};

pub struct JSProxyData {
    pub target: JSValue,
    pub handler: JSValue,
    pub is_func: bool,
    pub is_revoked: bool,
}

pub unsafe fn get_proxy_method(
    ctx: *mut JSContext,
    pmethod: *mut JSValue,
    obj: JSValueConst,
    name: JSAtom,
) -> *mut JSProxyData {
    if let JSObjectIsa::PROXY(s) = obj.object_unchecked().isa() {
        let s = *s;
        unimplemented!()

        // // safer to test recursion in all proxy methods
        // if js_check_stack_overflow((*ctx).rt, 0) {
        //     JS_ThrowStackOverflow(ctx);
        //     return std::ptr::null_mut();
        // }

        // // 's' should never be NULL
        // if (*s).is_revoked {
        //     JS_ThrowTypeErrorRevokedProxy(ctx);
        //     return std::ptr::null_mut();
        // }
        // let method = JS_GetProperty(ctx, (*s).handler, name);
        // if JS_IsException(method) {
        //     return std::ptr::null_mut();
        // }
        // if JS_IsNull(method) {
        //     method = JS_UNDEFINED;
        // }
        // *pmethod = method;
        // s
    } else {
        unreachable!()
    }
}

pub unsafe fn js_proxy_isArray(ctx: *mut JSContext, obj: JSValueConst) -> Result<bool, i32> {
    if let JSObjectIsa::PROXY(s) = obj.object_unchecked().isa() {
        let s = *s;
        if js_check_stack_overflow((*ctx).rt(), 0) {
            JS_ThrowStackOverflow(ctx);
            return Err(-1);
        }
        if s.as_ref().unwrap().is_revoked {
            JS_ThrowTypeErrorRevokedProxy(ctx);
            return Err(-1);
        }
        JS_IsArray(ctx, s.as_ref().unwrap().target)
    } else {
        Ok(false)
    }
}

pub unsafe fn js_proxy_isExtensible(ctx: *mut JSContext, obj: JSValueConst) -> Result<bool, i32> {
    let mut method = JSValue::default();
    let s = get_proxy_method(ctx, &mut method, obj, JS_ATOM_isExtensible);
    if s.is_null() {
        return Err(-1);
    }

    if JS_IsUndefined(method) {
        return JS_IsExtensible(ctx, (*s).target);
    }
    let ret = JS_CallFree(
        ctx.as_mut().unwrap(),
        method,
        (*s).handler,
        1,
        &mut (*s).target,
    );
    if JS_IsException(ret) {
        return Err(-1);
    }

    let b = JS_ToBoolFree(ctx, ret)?;
    if b != JS_IsExtensible(ctx, (*s).target)? {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"proxy: inconsistent isExtensible");
        return Err(-1);
    }

    Ok(b)
}

pub unsafe fn js_proxy_preventExtensions(
    ctx: *mut JSContext,
    obj: JSValueConst,
) -> Result<bool, i32> {
    let mut method: JSValue = JS_UNDEFINED;
    let s = get_proxy_method(ctx, &mut method, obj, JS_ATOM_preventExtensions);
    if s.is_null() {
        return Err(-1);
    }
    if JS_IsUndefined(method) {
        return JS_PreventExtensions(ctx, (*s).target);
    }

    let ret = JS_CallFree(
        ctx.as_mut().unwrap(),
        method,
        (*s).handler,
        1,
        &mut (*s).target,
    );
    if JS_IsException(ret) {
        return Err(-1);
    }

    let res = JS_ToBoolFree(ctx, ret)?;
    if res {
        if JS_IsExtensible(ctx, (*s).target)? {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"proxy: inconsistent preventExtensions");
            return Err(-1);
        }
    }

    Ok(res)
}

pub unsafe fn js_proxy_getPrototypeOf(ctx: *mut JSContext, obj: JSValueConst) -> JSValue {
    let mut method: JSValue = JS_UNDEFINED;
    let s = get_proxy_method(ctx, &mut method, obj, JS_ATOM_getPrototypeOf);
    if s.is_null() {
        return JSValue::EXCEPTION;
    }
    if method.is_undefined() {
        return JS_GetPrototype(ctx, (*s).target);
    }

    let ret = JS_CallFree(
        ctx.as_mut().unwrap(),
        method,
        (*s).handler,
        1,
        &mut (*s).target,
    );
    if ret.is_exception() {
        return ret;
    }

    if !matches!(ret.tag(), JS_TAG_NULL | JS_TAG_OBJECT) {
        {
            // C2RS: goto fail replaced with a block to handle the failure case
            JS_FreeValue(ctx, ret);
            return ctx
                .as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"proxy: inconsistent prototype");
        }
    }

    match JS_IsExtensible(ctx, (*s).target) {
        Ok(false) => {
            // check invariant
            let proto1 = JS_GetPrototype(ctx, (*s).target);
            if JS_IsException(proto1) {
                JS_FreeValue(ctx, ret);
                return JSValue::EXCEPTION;
            }
            if JS_VALUE_GET_OBJ(proto1) != JS_VALUE_GET_OBJ(ret) {
                JS_FreeValue(ctx, proto1);
                JS_FreeValue(ctx, ret);

                ctx.as_mut()
                    .unwrap()
                    .JS_ThrowTypeError(c"proxy: inconsistent prototype")
            } else {
                JS_FreeValue(ctx, proto1);
                ret
            }
        }
        Ok(true) => ret,
        Err(_) => JSValue::EXCEPTION,
    }
}

pub unsafe fn js_proxy_setPrototypeOf(
    ctx: *mut JSContext,
    obj: JSValueConst,
    proto_val: JSValueConst,
    throw_flag: bool,
) -> Result<bool, i32> {
    let mut method: JSValue = JS_UNDEFINED;
    let s = get_proxy_method(ctx, &mut method, obj, JS_ATOM_setPrototypeOf);
    if s.is_null() {
        return Err(-1);
    }
    if JS_IsUndefined(method) {
        return JS_SetPrototypeInternal(ctx, (*s).target, proto_val, throw_flag);
    }

    let mut args = [(*s).target, proto_val];
    let ret = JS_CallFree(
        ctx.as_mut().unwrap(),
        method,
        (*s).handler,
        2,
        args.as_mut_ptr(),
    );
    if JS_IsException(ret) {
        return Err(-1);
    }

    match JS_ToBoolFree(ctx, ret) {
        Ok(false) => {
            if throw_flag {
                ctx.as_mut()
                    .unwrap()
                    .JS_ThrowTypeError(c"proxy: bad prototype");
                Err(-1)
            } else {
                Ok(false)
            }
        }
        Ok(true) | Err(_) => {
            if !JS_IsExtensible(ctx, (*s).target)? {
                let proto1 = JS_GetPrototype(ctx, (*s).target);
                if JS_IsException(proto1) {
                    return Err(-1);
                }
                if JS_VALUE_GET_OBJ(proto_val) != JS_VALUE_GET_OBJ(proto1) {
                    JS_FreeValue(ctx, proto1);
                    ctx.as_mut()
                        .unwrap()
                        .JS_ThrowTypeError(c"proxy: inconsistent prototype");
                    return Err(-1);
                }
                JS_FreeValue(ctx, proto1);
            }
            Ok(true)
        }
    }
}

pub unsafe fn js_proxy_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_proxy_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
    unimplemented!();
}
