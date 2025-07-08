use std::ops::Deref;

use crate::{
    atom::{JSAtom, consts::*},
    context::JSContext,
    error::{JS_ThrowStackOverflow, JS_ThrowTypeErrorRevokedProxy},
    function::JS_Call,
    object::{JS_PreventExtensions, JS_SetPrototypeInternal, JSObjectIsa},
    runtime::{JSRuntime, js_check_stack_overflow},
    value::{JSValue, JSValueConst},
};

#[derive(Debug)]
pub struct ObjProxy {
    pub target: JSValue,
    pub handler: JSValue,
    pub is_func: bool,
    pub is_revoked: bool,
}

pub unsafe fn get_proxy_method<'a>(
    ctx: &'a JSContext,
    obj: &'a JSValueConst,
    name: JSAtom,
) -> Result<(JSValue, &'a ObjProxy), i32> {
    if let JSObjectIsa::PROXY(s) = obj.object_unchecked().0.isa.borrow().deref() {
        unimplemented!()

        // // safer to test recursion in all proxy methods
        // if js_check_stack_overflow(ctx.rt, 0) {
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

pub fn js_proxy_isArray(ctx: &JSContext, val: &JSValue) -> Result<bool, i32> {
    if let JSValue::Object(p) = val {
        if let JSObjectIsa::PROXY(s) = p.0.isa.borrow().deref() {
            if js_check_stack_overflow(ctx.rt(), 0) {
                JS_ThrowStackOverflow(ctx);
                return Err(-1);
            }
            if s.is_revoked {
                JS_ThrowTypeErrorRevokedProxy(ctx);
                return Err(-1);
            }
            s.target.is_array(ctx)
        } else {
            Ok(false)
        }
    } else {
        Ok(false)
    }
}

pub fn js_proxy_isExtensible(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    let (method, s) = unsafe { get_proxy_method(ctx, obj, JS_ATOM_isExtensible)? };
    if method.is_undefined() {
        return s.target.is_extensible(ctx);
    }

    let b = JS_Call(ctx, &method, &s.handler, std::slice::from_ref(&s.target))
        .check_exception()?
        .coerce_to_bool(ctx)?;

    if b != s.target.is_extensible(ctx)? {
        ctx.JS_ThrowTypeError("proxy: inconsistent isExtensible");
        return Err(-1);
    }

    Ok(b)
}

pub fn js_proxy_preventExtensions(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    let (method, s) = unsafe { get_proxy_method(ctx, obj, JS_ATOM_preventExtensions)? };
    if method.is_undefined() {
        return JS_PreventExtensions(ctx, &s.target);
    }

    let ret =
        JS_Call(ctx, &method, &s.handler, std::slice::from_ref(&s.target)).check_exception()?;

    let res = ret.coerce_to_bool(ctx)?;
    if res && s.target.is_extensible(ctx)? {
        ctx.JS_ThrowTypeError("proxy: inconsistent preventExtensions");
        return Err(-1);
    }

    Ok(res)
}

pub unsafe fn js_proxy_getPrototypeOf(ctx: &JSContext, obj: &JSValueConst) -> JSValue {
    let (method, s) = if let Ok((m, s)) = get_proxy_method(ctx, obj, JS_ATOM_getPrototypeOf) {
        (m, s)
    } else {
        return JSValue::EXCEPTION;
    };

    if method.is_undefined() {
        return s.target.prototype(ctx).unwrap_or(JSValue::EXCEPTION);
    }

    let ret = JS_Call(ctx, &method, &s.handler, std::slice::from_ref(&s.target));
    if ret.is_exception() {
        return ret;
    }

    if !matches!(ret, JSValue::NULL | JSValue::Object(_)) {
        ctx.JS_ThrowTypeError("proxy: inconsistent prototype");
        return JSValue::EXCEPTION;
    }

    match s.target.is_extensible(ctx) {
        Ok(false) => {
            // check invariant
            let proto1 = if let Ok(p) = s.target.prototype(ctx) {
                p
            } else {
                return JSValue::EXCEPTION;
            };

            if proto1 != ret {
                ctx.JS_ThrowTypeError("proxy: inconsistent prototype");
                JSValue::EXCEPTION
            } else {
                ret
            }
        }
        Ok(true) => ret,
        Err(_) => JSValue::EXCEPTION,
    }
}

pub unsafe fn js_proxy_setPrototypeOf(
    ctx: &JSContext,
    obj: &JSValue,
    proto_val: JSValue,
    throw_flag: bool,
) -> Result<bool, i32> {
    let (method, s) = get_proxy_method(ctx, obj, JS_ATOM_setPrototypeOf)?;
    if method.is_undefined() {
        return JS_SetPrototypeInternal(ctx, &s.target, proto_val, throw_flag);
    }

    let ret = JS_Call(
        ctx,
        &method,
        &s.handler,
        &[s.target.clone(), proto_val.clone()],
    )
    .check_exception()?;

    match ret.coerce_to_bool(ctx) {
        Ok(false) => {
            if throw_flag {
                ctx.JS_ThrowTypeError("proxy: bad prototype");
                Err(-1)
            } else {
                Ok(false)
            }
        }
        Ok(true) | Err(_) => {
            if !s.target.is_extensible(ctx)? {
                let proto1 = s.target.prototype(ctx)?;
                if proto_val != proto1 {
                    ctx.JS_ThrowTypeError("proxy: inconsistent prototype");
                    return Err(-1);
                }
            }
            Ok(true)
        }
    }
}

pub fn js_proxy_finalizer(rt: &mut JSRuntime, is: &JSObjectIsa, id: usize) {
    unimplemented!();
}

pub fn js_proxy_mark(val: JSValue) {
    unimplemented!();
}
