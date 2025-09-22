use std::{cell::Cell, ops::Deref};

use crate::{
    JS_CLASS_PROXY,
    atom::{JS_FreeAtom, JSAtom, consts::*},
    class::JSClassExoticMethods,
    context::JSContext,
    error::JSResult,
    function::{JS_CALL_FLAG_CONSTRUCTOR, JS_CallConstructor2},
    object::{
        JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_GETSET,
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
        JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_THROW, JS_PROP_WRITABLE, JSObject,
        JSPropertyDescriptor, ObjectPayload, PropertyKey, check_define_prop_flags,
    },
    object_cast,
    runtime::{JSRuntime, js_check_stack_overflow},
    value::JSValue,
};

#[derive(Debug)]
pub struct ProxyObjPayload {
    pub target: JSObject,
    pub handler: JSValue,
    pub is_func: bool,
    pub is_revoked: Cell<bool>,
}

impl ProxyObjPayload {
    /// get proxied method. returns function object or undefined
    pub fn get_method(&self, ctx: &JSContext, name: JSAtom) -> Result<JSValue, i32> {
        // safer to test recursion in all proxy methods
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        if self.is_revoked.get() {
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
                .call(ctx, &self.handler, &[self.target.clone().into()])?
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
                .call(ctx, &self.handler, &[self.target.clone().into()])?
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
            return self
                .target
                .prototype(ctx)
                .map(|v| v.map_or(JSValue::NULL, JSValue::Object));
        }

        let ret = method.call(ctx, &self.handler, &[self.target.clone().into()])?;
        if !matches!(ret, JSValue::NULL | JSValue::Object(_)) {
            ctx.throw_type_error("proxy: inconsistent prototype");
            return Err(-1);
        }

        if self.target.is_extensible(ctx)? {
            Ok(ret)
        } else {
            // check invariant
            if self
                .target
                .prototype(ctx)?
                .map_or(JSValue::NULL, JSValue::Object)
                != ret
            {
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
            return self.target.set_prototype_internal(ctx, proto_val, throw);
        }

        let ret = method.call(
            ctx,
            &self.handler,
            &[self.target.clone().into(), proto_val.clone()],
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
                    if proto_val
                        != self
                            .target
                            .prototype(ctx)?
                            .map_or(JSValue::NULL, JSValue::Object)
                    {
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
    pub fn class_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let proxy = Proxy::try_from(func_obj.clone()).unwrap();

        if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
            return js_proxy_call_constructor(ctx, &proxy, this_obj.clone(), args);
        }

        let px = proxy.payload();
        if !px.is_func {
            ctx.throw_type_error("not a function");
            return Err(-1);
        }

        let method = px.get_method(ctx, JS_ATOM_apply)?;
        if method.is_undefined() {
            return JSValue::from(px.target.clone()).call(ctx, this_obj, args);
        }

        let arg_array = ctx.create_array_from_slice(args)?;
        method.call(
            ctx,
            &px.handler,
            &[px.target.clone().into(), this_obj.clone(), arg_array.into()],
        )
    }

    pub fn class_finalizer(rt: &mut JSRuntime, _: ObjectPayload, _: usize) {
        // this is done via rust drop()
        // if let ObjectPayload::Proxy(p) = payload {
        //     // free p.target and p.handler
        // }
    }

    pub fn class_mark(val: JSValue) {
        if let ObjectPayload::PROXY(p) = val.as_object().unwrap().0.payload.borrow().deref() {
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
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// get proxied method
    #[inline(always)]
    pub fn get_method(&self, ctx: &JSContext, name: JSAtom) -> Result<JSValue, i32> {
        self.payload().get_method(ctx, name)
    }

    pub fn is_array(&self, ctx: &JSContext) -> Result<bool, i32> {
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        let p = self.payload();
        if p.is_revoked.get() {
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

fn js_proxy_call_constructor(
    ctx: &JSContext,
    func_obj: &Proxy,
    new_target: JSValue,
    args: &[JSValue],
) -> JSResult {
    let s = func_obj.payload();
    if !s.target.is_constructor() {
        ctx.throw_type_error("not a constructor");
        return Err(-1);
    }

    let method = s.get_method(ctx, JS_ATOM_construct)?;
    if method.is_undefined() {
        return JS_CallConstructor2(ctx, &s.target.clone().into(), new_target.clone(), args).err();
    }

    let arg_array = ctx.create_array_from_slice(args)?;
    let ret = method.call(
        ctx,
        &s.handler,
        &[s.target.clone().into(), arg_array.into(), new_target],
    )?;
    if !ret.is_object() {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    } else {
        Ok(ret)
    }
}

//
// Proxy class exotic methods
//

fn js_proxy_get_own_property(
    ctx: &JSContext,
    obj: JSValue,
    prop: JSAtom,
) -> Result<Option<JSPropertyDescriptor>, i32> {
    let Some(px) = obj
        .as_object()
        .and_then(|x| x.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = px.get_method(ctx, JS_ATOM_getOwnPropertyDescriptor)?;
    if method.is_undefined() {
        return px.target.get_own_property(ctx, prop);
    }

    let trap_result_obj = method.call(
        ctx,
        &px.handler,
        &[px.target.clone().into(), prop.string_or_symbol(ctx)?],
    )?;
    if !matches!(trap_result_obj, JSValue::Object(_) | JSValue::UNDEFINED) {
        ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor ");
        return Err(-1);
    }

    let target_desc = px.target.get_own_property(ctx, prop)?;

    if let JSValue::Object(trap_result_obj) = trap_result_obj {
        let result_desc = JSPropertyDescriptor::parse(ctx, &trap_result_obj)?;

        if let Some(target_desc) = &target_desc {
            // convert result_desc.flags to defineProperty flags
            let flags1 = result_desc.flags
                | JS_PROP_HAS_CONFIGURABLE
                | JS_PROP_HAS_ENUMERABLE
                | if result_desc.flags & JS_PROP_GETSET != 0 {
                    JS_PROP_HAS_GET | JS_PROP_HAS_SET
                } else {
                    JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE
                };

            // XXX: not complete check: need to compare value & getter/setter as in defineproperty
            if !check_define_prop_flags(target_desc.flags, flags1) {
                ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }
        } else {
            if !px.target.is_extensible(ctx)? {
                ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }
        }

        if result_desc.flags & JS_PROP_CONFIGURABLE == 0 {
            if target_desc
                .as_ref()
                .map_or(true, |d| d.flags & JS_PROP_CONFIGURABLE != 0)
            {
                ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }

            if result_desc.flags & (JS_PROP_GETSET | JS_PROP_WRITABLE) == 0
                && target_desc
                    .as_ref()
                    .map_or(false, |d| d.flags & JS_PROP_WRITABLE != 0)
            {
                // proxy-missing-checks
                ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }
        }

        Ok(Some(result_desc))
    } else {
        // trap_result_obj is undefined
        if let Some(desc) = target_desc {
            if (desc.flags & JS_PROP_CONFIGURABLE == 0) || !px.target.flags().extensible() {
                ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }
        }

        Ok(None)
    }
}

fn js_proxy_define_own_property(
    ctx: &JSContext,
    obj: &JSValue,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let flags = flags as u32;

    let Some(px) = obj
        .as_object()
        .and_then(|x| x.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = px.get_method(ctx, JS_ATOM_defineProperty)?;
    if method.is_undefined() {
        return px
            .target
            .define_property(ctx, prop, val, getter, setter, flags);
    }

    let ret = method
        .call(
            ctx,
            &px.handler,
            &[
                px.target.clone().into(),
                prop.string_or_symbol(ctx)?,
                JSPropertyDescriptor {
                    value: val.clone(),
                    getter: getter.clone(),
                    setter: setter.clone(),
                    flags,
                }
                .make_object(ctx)?
                .into(),
            ],
        )?
        .to_bool()?;

    if !ret {
        return if flags & JS_PROP_THROW != 0 {
            ctx.throw_type_error("proxy: defineProperty exception");
            Err(-1)
        } else {
            Ok(false)
        };
    }

    let setting_not_configurable =
        flags & (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE) == JS_PROP_HAS_CONFIGURABLE;

    if let Some(desc) = px.target.get_own_property(ctx, prop)? {
        if !check_define_prop_flags(desc.flags, flags)
            || (desc.flags & JS_PROP_CONFIGURABLE != 0 && setting_not_configurable)
        {
            ctx.throw_type_error("proxy: inconsistent defineProperty");
            return Err(-1);
        }

        if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
            if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE) == JS_PROP_GETSET {
                if (flags & JS_PROP_HAS_GET) != 0 && !getter.is_same(&desc.getter) {
                    ctx.throw_type_error("proxy: inconsistent defineProperty");
                    return Err(-1);
                }
                if (flags & JS_PROP_HAS_SET) != 0 && !setter.is_same(&desc.setter) {
                    ctx.throw_type_error("proxy: inconsistent defineProperty");
                    return Err(-1);
                }
            }
        } else if flags & JS_PROP_HAS_VALUE != 0 {
            if desc.flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == JS_PROP_WRITABLE
                && flags & JS_PROP_WRITABLE == 0
            {
                // missing-proxy-check feature
                ctx.throw_type_error("proxy: inconsistent defineProperty");
                return Err(-1);
            } else if desc.flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                && !val.is_same(&desc.value)
            {
                ctx.throw_type_error("proxy: inconsistent defineProperty");
                return Err(-1);
            }
        }
        if flags & JS_PROP_HAS_WRITABLE != 0 {
            if (desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE))
                == JS_PROP_WRITABLE
            {
                // proxy-missing-checks
                ctx.throw_type_error("proxy: inconsistent defineProperty");
                return Err(-1);
            }
        }
    } else {
        if setting_not_configurable || !px.target.flags().extensible() {
            ctx.throw_type_error("proxy: inconsistent defineProperty");
            return Err(-1);
        }
    }

    Ok(true)
}

fn js_proxy_delete_property(ctx: &JSContext, obj: JSValue, atom: JSAtom) -> Result<bool, i32> {
    let Some(px) = obj
        .as_object()
        .and_then(|x| x.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = px.get_method(ctx, JS_ATOM_deleteProperty)?;
    if method.is_undefined() {
        return px.target.delete_property(ctx, atom, 0);
    }

    let res = method
        .call(
            ctx,
            &px.handler,
            &[px.target.clone().into(), atom.string_or_symbol(ctx)?],
        )?
        .to_bool()?;

    if res {
        if let Some(desc) = px.target.get_own_property(ctx, atom)? {
            if desc.flags & JS_PROP_CONFIGURABLE == 0 {
                ctx.throw_type_error("proxy: inconsistent deleteProperty");
                return Err(-1);
            }
            if !px.target.is_extensible(ctx)? {
                ctx.throw_type_error("proxy: inconsistent deleteProperty");
                return Err(-1);
            }
        }
    }

    Ok(res)
}

fn js_proxy_get_own_property_names(
    ctx: &JSContext,
    obj: &JSObject,
) -> Result<Box<[PropertyKey]>, i32> {
    let Some(px) = obj.as_Proxy().map(|x| x.payload()) else {
        return Err(-1);
    };

    if px.is_revoked.get() {
        ctx.throw_type_error_revoked_proxy();
        return Err(-1);
    }

    let method = px.get_method(ctx, JS_ATOM_ownKeys)?;
    if method.is_undefined() {
        return px
            .target
            .get_own_property_names(ctx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK);
    }

    let prop_array = method.call(ctx, &px.handler, &[px.target.clone().into()])?;
    let len = prop_array.get_length_prop(ctx)? as usize;

    let mut tab = Vec::<PropertyKey>::new();
    if len > 0 {
        tab.try_reserve_exact(len).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    for i in 0..len {
        let val = prop_array.get_property_u32(ctx, i as u32)?;
        if !matches!(val, JSValue::String(_) | JSValue::Symbol(_)) {
            ctx.throw_type_error("proxy: properties must be strings or symbols");
            return Err(-1);
        }

        let atom = val.to_atom(ctx)?;

        if !tab.is_empty() {
            if tab.iter().any(|k| k.atom == atom) {
                for x in tab.drain(..) {
                    JS_FreeAtom(ctx, x.atom);
                }
                ctx.throw_type_error("proxy: duplicate property");
                return Err(-1);
            }
        }
        tab.push(PropertyKey::new(ctx, atom, false, false));
    }

    // check if there are non configurable properties
    let is_extensible = px.target.is_extensible(ctx)?;
    let mut has_err = false;

    for k2 in px
        .target
        .get_own_property_names(ctx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK)?
    {
        if !has_err {
            match px.target.get_own_property(ctx, k2.atom) {
                Ok(Some(desc)) => {
                    // safety, property should be found
                    if desc.flags & JS_PROP_CONFIGURABLE == 0 || !is_extensible {
                        if let Some(p) = tab.iter_mut().find(|p| p.atom == k2.atom) {
                            // mark the property as found
                            if !is_extensible {
                                p.is_enumerable = true;
                            }
                        } else {
                            ctx.throw_type_error(
                                "proxy: target property must be present in proxy ownKeys",
                            );
                            has_err = true;
                        }
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    has_err = true;
                }
            }
        }

        JS_FreeAtom(ctx, k2.atom);
    }
    if has_err {
        return Err(-1);
    }

    if !is_extensible {
        // check that all property in 'tab' were checked
        if tab.iter().any(|p| !p.is_enumerable) {
            // found an item with is_enumerable=FALSE
            ctx.throw_type_error(
                "proxy: property not present in target were returned by non extensible proxy",
            );
            return Err(-1);
        }
    }

    Ok(tab.into_boxed_slice())
}

fn js_proxy_has(ctx: &JSContext, obj: &JSObject, atom: JSAtom) -> Result<bool, i32> {
    let Some(px) = obj.as_Proxy().map(|x| x.payload()) else {
        return Err(-1);
    };

    let method = px.get_method(ctx, JS_ATOM_has)?;
    if method.is_undefined() {
        return px.target.has_property(ctx, atom);
    }

    let ret = method
        .call(
            ctx,
            &px.handler,
            &[px.target.clone().into(), atom.string_or_symbol(ctx)?],
        )?
        .to_bool()?;

    if !ret {
        if let Some(desc) = px.target.get_own_property(ctx, atom)? {
            if desc.flags & JS_PROP_CONFIGURABLE == 0 || !px.target.flags().extensible() {
                ctx.throw_type_error("proxy: inconsistent has");
                return Err(-1);
            }
        }

        if let Some(desc) = px.target.get_own_property(ctx, atom)? {
            if desc.flags & JS_PROP_CONFIGURABLE == 0 || !px.target.flags().extensible() {
                ctx.throw_type_error("proxy: inconsistent has");
                return Err(-1);
            }
        }
    }

    Ok(ret)
}

fn js_proxy_get(ctx: &JSContext, obj: JSValue, atom: JSAtom, receiver: JSValue) -> JSResult {
    let Some(px) = obj
        .as_object()
        .and_then(|p| p.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = px.get_method(ctx, JS_ATOM_get)?;
    // Note: recursion is possible thru the prototype of s.target
    if method.is_undefined() {
        return px.target.get_property_internal(ctx, atom, &receiver, false);
    }

    let ret = method.call(
        ctx,
        &px.handler,
        &[
            px.target.clone().into(),
            atom.string_or_symbol(ctx)?,
            receiver,
        ],
    )?;

    if let Some(desc) = px.target.get_own_property(ctx, atom)? {
        if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0 {
            if !desc.value.is_same(&ret) {
                ctx.throw_type_error("proxy: inconsistent get ");
                return Err(-1);
            }
        } else if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE) == JS_PROP_GETSET {
            if desc.getter.is_undefined() && !ret.is_undefined() {
                ctx.throw_type_error("proxy: inconsistent get ");
                return Err(-1);
            }
        }
    }

    Ok(ret)
}

fn js_proxy_set(
    ctx: &JSContext,
    obj: JSValue,
    atom: JSAtom,
    value: JSValue,
    receiver: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let Some(s) = obj
        .as_object()
        .and_then(|p| p.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = s.get_method(ctx, JS_ATOM_set)?;
    if method.is_undefined() {
        return JSValue::set_property_internal(
            ctx,
            &s.target.clone().into(),
            atom,
            value,
            &receiver,
            flags as u32,
        );
    }

    let ret = method
        .call(
            ctx,
            &s.handler,
            &[
                s.target.clone().into(),
                atom.string_or_symbol(ctx)?,
                value.clone(),
                receiver,
            ],
        )?
        .to_bool()?;

    if ret {
        if let Some(desc) = s.target.get_own_property(ctx, atom)? {
            if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0 {
                if !desc.value.is_same(&value) {
                    ctx.throw_type_error("proxy: inconsistent set");
                    return Err(-1);
                }
            } else if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE) == JS_PROP_GETSET
                && desc.setter.is_undefined()
            {
                ctx.throw_type_error("proxy: inconsistent set");
                return Err(-1);
            }
        }
    } else {
        if ctx.can_throw(flags as u32) {
            ctx.throw_type_error("proxy: cannot set property");
            return Err(-1);
        }
    }

    Ok(ret)
}

pub static js_proxy_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    get_own_property: Some(js_proxy_get_own_property),
    define_own_property: Some(js_proxy_define_own_property),
    delete_property: Some(js_proxy_delete_property),
    get_own_property_names: Some(js_proxy_get_own_property_names),
    has_property: Some(js_proxy_has),
    get_property: Some(js_proxy_get),
    set_property: Some(js_proxy_set),
};
