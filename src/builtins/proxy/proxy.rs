// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_PROXY,
    atom::Atom,
    class::{ClassExoticMethods, ClassID},
    context::ScopedContext,
    error::JSResult,
    function::{Callback, JS_CALL_FLAG_CONSTRUCTOR},
    object::{
        JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_GETSET,
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
        JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_THROW, JS_PROP_WRITABLE,
        JSPropertyDescriptor, Object, ObjectImpl, ObjectPayload, PropertyKey,
        check_define_prop_flags,
    },
    object_cast,
    runtime::js_check_stack_overflow,
    utils::unlikely,
    value::JSValue,
};

use super::{js_proxy_constructor, js_proxy_funcs};

#[derive(Debug)]
pub struct ProxyPayload {
    target: Object,
    handler: JSValue,
    is_func: bool,
    is_revoked: Cell<bool>,
}

impl GcTrace for ProxyPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.target.trace(tr);
        self.handler.trace(tr);
    }
}

impl ProxyPayload {
    pub fn new(target: Object, handler: JSValue) -> Box<Self> {
        Box::new(Self {
            is_func: target.is_function(),
            target,
            handler,
            is_revoked: Cell::new(false),
        })
    }

    #[inline(always)]
    pub const fn is_func(&self) -> bool {
        self.is_func
    }

    #[inline(always)]
    pub const fn is_revoked(&self) -> bool {
        self.is_revoked.get()
    }

    #[inline(always)]
    pub fn set_revoked(&self) {
        self.is_revoked.set(true);
    }

    pub fn is_extensible_obj(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        let method = self.get_proxy_method(ctx, Atom::CONST_isExtensible)?;
        if method.is_undefined() {
            self.target.is_extensible_obj(ctx)
        } else {
            let b = method
                .call(ctx, self.handler, &[self.target.into()])?
                .to_bool()?;

            if b != self.target.is_extensible_obj(ctx)? {
                ctx.throw_type_error("proxy: inconsistent isExtensible");
                Err(-1)
            } else {
                Ok(b)
            }
        }
    }

    #[inline(always)]
    pub const fn target(&self) -> Object {
        self.target
    }

    /// get proxied method. returns function object or undefined
    pub fn get_proxy_method(&self, ctx: &mut ScopedContext, name: Atom) -> Result<JSValue, i32> {
        // safer to test recursion in all proxy methods
        if unlikely(js_check_stack_overflow(ctx.rt_ref(), 0)) {
            ctx.throw_stack_overflow();
            Err(-1)
        } else if unlikely(self.is_revoked()) {
            ctx.throw_type_error_revoked_proxy();
            Err(-1)
        } else {
            let mut f = self.handler.get_property(ctx, name)?;
            if f.is_null() {
                f = JSValue::UNDEFINED;
            }
            Ok(f)
        }
    }

    pub fn prevent_extensions(&mut self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        let method = self.get_proxy_method(ctx, Atom::CONST_preventExtensions)?;
        if method.is_undefined() {
            self.target.prevent_extensions(ctx)
        } else {
            let b = method
                .call(ctx, self.handler, &[self.target.into()])?
                .to_bool()?;

            if b && self.target.is_extensible_obj(ctx)? {
                ctx.throw_type_error("proxy: inconsistent preventExtensions");
                return Err(-1);
            }

            Ok(b)
        }
    }

    pub fn get_prototype_of(&self, ctx: &mut ScopedContext) -> Result<JSValue, i32> {
        let method = self.get_proxy_method(ctx, Atom::CONST_getPrototypeOf)?;
        if method.is_undefined() {
            return self
                .target
                .prototype(ctx)
                .map(|v| v.map_or(JSValue::NULL, JSValue::Object));
        }

        let ret = method.call(ctx, self.handler, &[self.target.into()])?;
        if !matches!(ret, JSValue::NULL | JSValue::Object(_)) {
            ctx.throw_type_error("proxy: inconsistent prototype");
            return Err(-1);
        }

        if self.target.is_extensible_obj(ctx)? {
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
        &mut self,
        ctx: &mut ScopedContext,
        proto_val: JSValue,
        throw: bool,
    ) -> Result<bool, i32> {
        let method = self.get_proxy_method(ctx, Atom::CONST_setPrototypeOf)?;
        if method.is_undefined() {
            return self.target.set_prototype_internal(ctx, proto_val, false);
        }

        let ret = method.call(ctx, self.handler, &[self.target.into(), proto_val])?;

        match ret.to_bool() {
            Ok(false) => {
                Ok(false)
                // if throw {
                //     ctx.throw_type_error("proxy: bad prototype");
                //     Err(-1)
                // } else {
                //     Ok(false)
                // }
            }
            _ => {
                if !self.target.is_extensible_obj(ctx)?
                    && proto_val
                        != self
                            .target
                            .prototype(ctx)?
                            .map_or(JSValue::NULL, JSValue::Object)
                {
                    ctx.throw_type_error("proxy: inconsistent prototype");
                    Err(-1)
                } else {
                    Ok(true)
                }
            }
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct Proxy(pub(super) GcRef<ObjectImpl>);

object_cast!(Proxy, JS_CLASS_PROXY);

impl Proxy {
    pub fn class_init(ctx: &mut ScopedContext, _: ClassID) -> Result<Option<Object>, i32> {
        let mut proto =
            ctx.create_native_function(Callback::Constructor(js_proxy_constructor), "Proxy", 2, 0)?;

        proto.set_flag_constructor(true);
        proto.install_fields(ctx, js_proxy_funcs).unwrap();

        ctx.global_obj().define_property_value_str(
            ctx,
            "Proxy",
            proto.into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;

        Ok(Some(proto.into()))
    }

    pub fn class_call(
        ctx: &mut ScopedContext,
        func_obj: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let proxy = Proxy::try_from(func_obj).unwrap();

        if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
            return js_proxy_call_constructor(ctx.into(), &proxy, this_obj, args);
        }

        let px = proxy.payload();
        if !px.is_func {
            ctx.throw_type_error("not a function");
            return Err(-1);
        }

        let method = px.get_proxy_method(ctx, Atom::CONST_apply)?;
        if method.is_undefined() {
            return JSValue::from(px.target).call(ctx, this_obj, args);
        }

        let arg_array = ctx.create_array_from_slice(args)?;
        method.call(
            ctx,
            px.handler,
            &[px.target.into(), this_obj, arg_array.into()],
        )
    }

    /// get proxy payload reference
    #[inline(always)]
    pub(super) fn payload(&self) -> &ProxyPayload {
        if let ObjectPayload::PROXY(s) = self.0.payload() {
            s.as_ref()
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// get proxy payload reference
    #[inline(always)]
    pub(super) fn payload_mut(&mut self) -> &mut ProxyPayload {
        if let ObjectPayload::PROXY(s) = self.0.payload_mut() {
            s.as_mut()
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
    pub fn get_method(&self, ctx: &mut ScopedContext, name: Atom) -> Result<JSValue, i32> {
        self.payload().get_proxy_method(ctx, name)
    }

    pub fn is_array(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        if unlikely(js_check_stack_overflow(ctx.rt_ref(), 0)) {
            ctx.throw_stack_overflow();
            Err(-1)
        } else {
            let p = self.payload();
            if p.is_revoked() {
                ctx.throw_type_error_revoked_proxy();
                Err(-1)
            } else {
                p.target.is_array(ctx)
            }
        }
    }

    #[inline(always)]
    pub fn is_extensible_obj(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        self.payload().is_extensible_obj(ctx)
    }

    #[inline(always)]
    pub fn prevent_extensions(&mut self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        self.payload_mut().prevent_extensions(ctx)
    }

    /// returns Object, NULL or Err.
    #[inline(always)]
    pub fn get_prototype_of(&self, ctx: &mut ScopedContext) -> Result<JSValue, i32> {
        self.payload().get_prototype_of(ctx)
    }

    #[inline(always)]
    pub fn set_prototype_of(
        &mut self,
        ctx: &mut ScopedContext,
        proto_val: JSValue,
        throw: bool,
    ) -> Result<bool, i32> {
        self.payload_mut().set_prototype_of(ctx, proto_val, throw)
    }
}

fn js_proxy_call_constructor(
    ctx: &mut ScopedContext,
    func_obj: &Proxy,
    new_target: JSValue,
    args: &[JSValue],
) -> JSResult {
    let px = func_obj.payload();
    if !px.target.is_constructor() {
        ctx.throw_type_error("not a constructor");
        return Err(-1);
    }

    let method = px.get_proxy_method(ctx, Atom::CONST_construct)?;
    if method.is_undefined() {
        return px.target.call_ctor_new_target(ctx, new_target, args);
    }

    let arg_array = ctx.create_array_from_slice(args)?;
    let ret = method.call(
        ctx,
        px.handler,
        &[px.target.into(), arg_array.into(), new_target],
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
    ctx: &mut ScopedContext,
    obj: JSValue,
    prop: Atom,
) -> Result<Option<JSPropertyDescriptor>, i32> {
    let Some(px) = obj.as_object_Proxy().map(|x| x.payload()) else {
        return Err(-1);
    };

    let method = px.get_proxy_method(ctx, Atom::CONST_getOwnPropertyDescriptor)?;
    if method.is_undefined() {
        return px.target.get_own_property(ctx, prop);
    }

    let prop_val = prop.to_value(ctx)?;
    let trap_result_obj = method.call(ctx, px.handler, &[px.target.into(), prop_val])?;
    if !matches!(trap_result_obj, JSValue::Object(_) | JSValue::UNDEFINED) {
        ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor ");
        return Err(-1);
    }

    let target_desc = px.target.get_own_property(ctx, prop)?;

    if let JSValue::Object(trap_result_obj) = trap_result_obj {
        let result_desc = JSPropertyDescriptor::parse(ctx, trap_result_obj)?;

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
        } else if !px.target.is_extensible_obj(ctx)? {
            ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
            return Err(-1);
        }

        if result_desc.flags & JS_PROP_CONFIGURABLE == 0 {
            if target_desc
                .as_ref()
                .is_none_or(|d| d.flags & JS_PROP_CONFIGURABLE != 0)
            {
                ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }

            if result_desc.flags & (JS_PROP_GETSET | JS_PROP_WRITABLE) == 0
                && target_desc
                    .as_ref()
                    .is_some_and(|d| d.flags & JS_PROP_WRITABLE != 0)
            {
                // proxy-missing-checks
                ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }
        }

        Ok(Some(result_desc))
    } else {
        // trap_result_obj is undefined
        if let Some(desc) = target_desc
            && ((desc.flags & JS_PROP_CONFIGURABLE == 0) || !px.target.flags().extensible())
        {
            ctx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
            return Err(-1);
        }

        Ok(None)
    }
}

fn js_proxy_define_own_property(
    ctx: &mut ScopedContext,
    obj: JSValue,
    prop: Atom,
    value: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let flags = flags as u32;

    let Some(px) = obj.as_object_Proxy().map(|x| x.payload()) else {
        return Err(-1);
    };

    let method = px.get_proxy_method(ctx, Atom::CONST_defineProperty)?;
    if method.is_undefined() {
        return {
            let mut t = px.target;
            t.define_property(ctx, prop, value, getter, setter, flags)
        };
    }

    let prop_val = prop.to_value(ctx)?;
    let prop_desc = JSPropertyDescriptor {
        value,
        getter,
        setter,
        flags,
    }
    .make_object(ctx)?;

    let ret = method
        .call(
            ctx,
            px.handler,
            &[px.target.into(), prop_val, prop_desc.into()],
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
                if (flags & JS_PROP_HAS_GET) != 0 && !getter.is_same(desc.getter) {
                    ctx.throw_type_error("proxy: inconsistent defineProperty");
                    return Err(-1);
                }
                if (flags & JS_PROP_HAS_SET) != 0 && !setter.is_same(desc.setter) {
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
                && !value.is_same(desc.value)
            {
                ctx.throw_type_error("proxy: inconsistent defineProperty");
                return Err(-1);
            }
        }
        if flags & JS_PROP_HAS_WRITABLE != 0
            && (desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE))
                == JS_PROP_WRITABLE
        {
            // proxy-missing-checks
            ctx.throw_type_error("proxy: inconsistent defineProperty");
            return Err(-1);
        }
    } else if setting_not_configurable || !px.target.flags().extensible() {
        ctx.throw_type_error("proxy: inconsistent defineProperty");
        return Err(-1);
    }

    Ok(true)
}

fn js_proxy_delete_property(
    ctx: &mut ScopedContext,
    mut obj: JSValue,
    atom: Atom,
) -> Result<bool, i32> {
    let Some(px) = obj.as_object_Proxy_mut().map(|x| x.payload_mut()) else {
        return Err(-1);
    };

    let method = px.get_proxy_method(ctx, Atom::CONST_deleteProperty)?;
    if method.is_undefined() {
        return px.target.delete_property(ctx, atom, 0);
    }

    let atom_val = atom.to_value(ctx)?;
    let res = method
        .call(ctx, px.handler, &[px.target.into(), atom_val])?
        .to_bool()?;

    if res && let Some(desc) = px.target.get_own_property(ctx, atom)? {
        if desc.flags & JS_PROP_CONFIGURABLE == 0 {
            ctx.throw_type_error("proxy: inconsistent deleteProperty");
            return Err(-1);
        }
        if !px.target.is_extensible_obj(ctx)? {
            ctx.throw_type_error("proxy: inconsistent deleteProperty");
            return Err(-1);
        }
    }

    Ok(res)
}

fn js_proxy_get_own_property_names(
    ctx: &mut ScopedContext,
    mut obj: Object,
) -> Result<Box<[PropertyKey]>, i32> {
    let Some(px) = obj.as_Proxy_mut().map(|x| x.payload_mut()) else {
        return Err(-1);
    };

    if px.is_revoked() {
        ctx.throw_type_error_revoked_proxy();
        return Err(-1);
    }

    let method = px.get_proxy_method(ctx, Atom::CONST_ownKeys)?;
    if method.is_undefined() {
        return px
            .target
            .get_own_property_names(ctx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK);
    }

    let prop_array = method.call(ctx, px.handler, &[px.target.into()])?;
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

        #[cfg(feature = "builtin_symbol")]
        if !matches!(val, JSValue::String(_) | JSValue::Symbol(_)) {
            ctx.throw_type_error("proxy: properties must be strings or symbols");
            return Err(-1);
        }
        #[cfg(not(feature = "builtin_symbol"))]
        if !matches!(val, JSValue::String(_)) {
            ctx.throw_type_error("proxy: properties must be strings");
            return Err(-1);
        }

        let atom = val.to_atom(ctx)?;

        if !tab.is_empty() && tab.iter().any(|k| k.atom == atom) {
            ctx.throw_type_error("proxy: duplicate property");
            return Err(-1);
        }
        tab.push(PropertyKey::new(ctx, atom, false, false));
    }

    // check if there are non configurable properties
    let is_extensible = px.target.is_extensible_obj(ctx)?;

    for k2 in px
        .target
        .get_own_property_names(ctx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK)?
    {
        if let Some(desc) = px.target.get_own_property(ctx, k2.atom)? {
            // safety, property should be found
            if desc.flags & JS_PROP_CONFIGURABLE == 0 || !is_extensible {
                if let Some(p) = tab.iter_mut().find(|p| p.atom == k2.atom) {
                    // mark the property as found
                    if !is_extensible {
                        p.is_enumerable = true;
                    }
                } else {
                    ctx.throw_type_error("proxy: target property must be present in proxy ownKeys");
                    return Err(-1);
                }
            }
        }
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

fn js_proxy_has(ctx: &mut ScopedContext, mut obj: Object, atom: Atom) -> Result<bool, i32> {
    let Some(px) = obj.as_Proxy_mut().map(|x| x.payload_mut()) else {
        return Err(-1);
    };

    let method = px.get_proxy_method(ctx, Atom::CONST_has)?;
    if method.is_undefined() {
        return px.target.has_property(ctx, atom);
    }

    let atom_val = atom.to_value(ctx)?;
    let ret = method
        .call(ctx, px.handler, &[px.target.into(), atom_val])?
        .to_bool()?;

    if !ret {
        if let Some(desc) = px.target.get_own_property(ctx, atom)?
            && (desc.flags & JS_PROP_CONFIGURABLE == 0 || !px.target.flags().extensible())
        {
            ctx.throw_type_error("proxy: inconsistent has");
            return Err(-1);
        }

        if let Some(desc) = px.target.get_own_property(ctx, atom)?
            && (desc.flags & JS_PROP_CONFIGURABLE == 0 || !px.target.flags().extensible())
        {
            ctx.throw_type_error("proxy: inconsistent has");
            return Err(-1);
        }
    }

    Ok(ret)
}

fn js_proxy_get(ctx: &mut ScopedContext, obj: JSValue, atom: Atom, receiver: JSValue) -> JSResult {
    let Some(px) = obj.as_object_Proxy().map(|x| x.payload()) else {
        return Err(-1);
    };

    let method = px.get_proxy_method(ctx, Atom::CONST_get)?;
    // Note: recursion is possible thru the prototype of s.target
    if method.is_undefined() {
        return px.target.get_property_internal(ctx, atom, receiver, false);
    }

    let atom_val = atom.to_value(ctx)?;
    let ret = method.call(ctx, px.handler, &[px.target.into(), atom_val, receiver])?;

    if let Some(desc) = px.target.get_own_property(ctx, atom)? {
        if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0 {
            if !desc.value.is_same(ret) {
                ctx.throw_type_error("proxy: inconsistent get ");
                return Err(-1);
            }
        } else if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE) == JS_PROP_GETSET
            && desc.getter.is_undefined()
            && !ret.is_undefined()
        {
            ctx.throw_type_error("proxy: inconsistent get ");
            return Err(-1);
        }
    }

    Ok(ret)
}

fn js_proxy_set(
    ctx: &mut ScopedContext,
    obj: JSValue,
    atom: Atom,
    value: JSValue,
    receiver: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let Some(s) = obj.as_object_Proxy().map(|x| x.payload()) else {
        return Err(-1);
    };

    let method = s.get_proxy_method(ctx, Atom::CONST_set)?;
    if method.is_undefined() {
        return s.target.set_property_internal(
            ctx,
            atom,
            value,
            match receiver {
                JSValue::NULL | JSValue::UNDEFINED => None,
                JSValue::Object(o) => Some(o),
                _ => {
                    ctx.throw_type_error("receiver must be an object");
                    return Err(-1);
                }
            },
            flags as u32,
        );
    }

    let atom_val = atom.to_value(ctx)?;
    let ret = method
        .call(
            ctx,
            s.handler,
            &[s.target.into(), atom_val, value, receiver],
        )?
        .to_bool()?;

    if ret {
        if let Some(desc) = s.target.get_own_property(ctx, atom)? {
            if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0 {
                if !desc.value.is_same(value) {
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
    } else if ctx.can_throw(flags as u32) {
        ctx.throw_type_error("proxy: cannot set property");
        return Err(-1);
    }

    Ok(ret)
}

fn js_proxy_get_prototype(ctx: &mut ScopedContext, obj: JSValue) -> Result<JSValue, i32> {
    if let Some(px) = obj.as_object_Proxy() {
        px.get_prototype_of(ctx)
    } else {
        Err(-1)
    }
}

fn js_proxy_set_prototype(
    ctx: &mut ScopedContext,
    mut obj: JSValue,
    proto_val: JSValue,
) -> Result<bool, i32> {
    if let Some(s) = obj.as_object_Proxy_mut() {
        s.set_prototype_of(ctx, proto_val, false)
    } else {
        Err(-1)
    }
}

pub(crate) const js_proxy_exotic_methods: ClassExoticMethods = ClassExoticMethods {
    get_own_property: Some(js_proxy_get_own_property),
    define_own_property: Some(js_proxy_define_own_property),
    delete_property: Some(js_proxy_delete_property),
    get_own_property_names: Some(js_proxy_get_own_property_names),
    has_property: Some(js_proxy_has),
    get_property: Some(js_proxy_get),
    set_property: Some(js_proxy_set),
    get_prototype: Some(js_proxy_get_prototype),
    set_prototype: Some(js_proxy_set_prototype),
};
