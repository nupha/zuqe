use std::ops::Deref;

use crate::{
    JS_CLASS_PROXY,
    atom::{JSAtom, consts::*},
    context::JSContext,
    object::{JS_SetPrototypeInternal, JSObject, ObjectPayload},
    object_cast,
    runtime::{JSRuntime, js_check_stack_overflow},
    value::JSValue,
};

#[derive(Debug)]
pub struct ProxyObjPayload {
    pub target: JSValue,
    pub handler: JSValue,
    pub is_func: bool,
    pub is_revoked: bool,
}

impl ProxyObjPayload {
    /// get proxied method
    pub fn get_method(&self, ctx: &JSContext, name: JSAtom) -> Result<JSValue, i32> {
        // safer to test recursion in all proxy methods
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        if self.is_revoked {
            ctx.throw_type_error_revoked_proxy();
            return Err(-1);
        }

        let mut method = self.handler.get_property(ctx, name)?;
        if method.is_null() {
            method = JSValue::UNDEFINED;
        }
        Ok(method)
    }

    pub fn is_extensible(&self, ctx: &JSContext) -> Result<bool, i32> {
        let method = self.get_method(ctx, JS_ATOM_isExtensible)?;
        if method.is_undefined() {
            self.target.is_extensible(ctx)
        } else {
            let b = method
                .call(ctx, &self.handler, std::slice::from_ref(&self.target))?
                .to_bool()?;

            if b != self.target.is_extensible(ctx)? {
                ctx.throw_type_error("proxy: inconsistent isExtensible");
                Err(-1)
            } else {
                Ok(b)
            }
        }
    }

    pub fn prevent_extensions(&self, ctx: &JSContext) -> Result<bool, i32> {
        let method = self.get_method(ctx, JS_ATOM_preventExtensions)?;
        if method.is_undefined() {
            return self.target.prevent_extensions(ctx);
        } else {
            let b = method
                .call(ctx, &self.handler, std::slice::from_ref(&self.target))?
                .to_bool()?;

            if b && self.target.is_extensible(ctx)? {
                ctx.throw_type_error("proxy: inconsistent preventExtensions");
                return Err(-1);
            }

            Ok(b)
        }
    }

    pub fn get_prototype_of(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        let method = self.get_method(ctx, JS_ATOM_getPrototypeOf)?;
        if method.is_undefined() {
            return self.target.prototype(ctx);
        }

        let ret = method.call(ctx, &self.handler, std::slice::from_ref(&self.target))?;
        if !matches!(ret, JSValue::NULL | JSValue::Object(_)) {
            ctx.throw_type_error("proxy: inconsistent prototype");
            return Err(-1);
        }

        if self.target.is_extensible(ctx)? {
            Ok(ret)
        } else {
            // check invariant
            if self.target.prototype(ctx)? != ret {
                ctx.throw_type_error("proxy: inconsistent prototype");
                Err(-1)
            } else {
                Ok(ret)
            }
        }
    }

    pub fn set_prototype_of(
        &self,
        ctx: &JSContext,
        proto_val: JSValue,
        throw: bool,
    ) -> Result<bool, i32> {
        let method = self.get_method(ctx, JS_ATOM_setPrototypeOf)?;
        if method.is_undefined() {
            return JS_SetPrototypeInternal(ctx, &self.target, proto_val, throw);
        }

        let ret = method.call(
            ctx,
            &self.handler,
            &[self.target.clone(), proto_val.clone()],
        )?;

        match ret.to_bool() {
            Ok(false) => {
                if throw {
                    ctx.throw_type_error("proxy: bad prototype");
                    Err(-1)
                } else {
                    Ok(false)
                }
            }
            Ok(true) | Err(_) => {
                if !self.target.is_extensible(ctx)? {
                    if proto_val != self.target.prototype(ctx)? {
                        ctx.throw_type_error("proxy: inconsistent prototype");
                        return Err(-1);
                    }
                }
                Ok(true)
            }
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct Proxy(pub(crate) JSObject);

object_cast!(Proxy, JS_CLASS_PROXY);

impl Proxy {
    pub fn hook_finalizer(rt: &mut JSRuntime, _: &ObjectPayload, _: usize) {
        // this is done via rust drop()
        // if let ObjectPayload::Proxy(p) = payload {
        //     // free p.target and p.handler
        // }
    }

    pub fn hook_mark(val: JSValue) {
        if let ObjectPayload::PROXY(p) = val.object().unwrap().0.payload.borrow().deref() {
            p.target.gc_mark();
            p.handler.gc_mark();
        }
    }

    /// get proxy payload reference
    #[inline(always)]
    pub(super) fn payload(&self) -> &ProxyObjPayload {
        if let ObjectPayload::PROXY(s) = self.0.0.payload.borrow().deref() {
            unsafe {
                let ptr = s.as_ref() as *const ProxyObjPayload;
                &*ptr
            }
        } else {
            #[cfg(debug_assertions)]
            unreachable!();

            #[cfg(not(debug_assertions))]
            std::hint::unreachable_unchecked();
        }
    }

    pub fn is_array(&self, ctx: &JSContext) -> Result<bool, i32> {
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        let p = self.payload();
        if p.is_revoked {
            ctx.throw_type_error_revoked_proxy();
            Err(-1)
        } else {
            p.target.is_array(ctx)
        }
    }

    #[inline(always)]
    pub fn is_extensible(&self, ctx: &JSContext) -> Result<bool, i32> {
        self.payload().is_extensible(ctx)
    }

    #[inline(always)]
    pub fn prevent_extensions(&self, ctx: &JSContext) -> Result<bool, i32> {
        self.payload().prevent_extensions(ctx)
    }

    /// returns Object, NULL or Err.
    #[inline(always)]
    pub fn get_prototype_of(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.payload().get_prototype_of(ctx)
    }

    #[inline(always)]
    pub fn set_prototype_of(
        &self,
        ctx: &JSContext,
        proto_val: JSValue,
        throw: bool,
    ) -> Result<bool, i32> {
        self.payload().set_prototype_of(ctx, proto_val, throw)
    }
}

impl JSObject {
    /// if `self` is an instance of Proxy class, cast it to &Proxy.
    #[inline]
    pub fn as_proxy(&self) -> Option<&Proxy> {
        if self.class_id() == JS_CLASS_PROXY {
            Some(unsafe { &*(self as *const JSObject as *const Proxy) })
        } else {
            None
        }
    }

    /// assume `self` is an instance of Proxy class, cast it to &Proxy.
    #[inline(always)]
    pub unsafe fn as_proxy_unchecked(&self) -> &Proxy {
        debug_assert_eq!(self.class_id(), JS_CLASS_PROXY);
        unsafe { &*(self as *const JSObject as *const Proxy) }
    }
}
