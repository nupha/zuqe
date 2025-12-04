use std::{cell::Cell, ops::Deref};

use gc_lite::{Collect, GcRef};

use crate::{
    JS_CLASS_PROXY,
    atom::Atom,
    class::{ClassID, JSClassExoticMethods},
    context::{Context, ContextImpl, ContextPtr},
    error::JSResult,
    function::{Callback, JS_CALL_FLAG_CONSTRUCTOR, JS_CallConstructor2},
    object::{
        JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_GETSET,
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
        JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_THROW, JS_PROP_WRITABLE,
        JSPropertyDescriptor, ObjectImpl, ObjectPayload, ObjectRef, PropertyKey,
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
    target: ObjectRef,
    handler: JSValue,
    is_func: bool,
    is_revoked: Cell<bool>,
}

unsafe impl Collect for ProxyPayload {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.target.trace(tracer);
        self.handler.trace(tracer);
    }
}

impl ProxyPayload {
    pub fn new(target: ObjectRef, handler: JSValue) -> Box<Self> {
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

    pub fn is_extensible(&self, ctx: &ContextImpl) -> Result<bool, i32> {
        let method = self.get_method(ctx, Atom::CONST_isExtensible)?;
        if method.is_undefined() {
            self.target.is_extensible(ctx)
        } else {
            let b = method
                .call(ctx, self.handler, &[self.target.into()])?
                .to_bool()?;

            if b != self.target.is_extensible(ctx)? {
                ctx.throw_type_error("proxy: inconsistent isExtensible");
                Err(-1)
            } else {
                Ok(b)
            }
        }
    }

    #[inline(always)]
    pub const fn target(&self) -> ObjectRef {
        self.target
    }

    /// get proxied method. returns function object or undefined
    pub fn get_method(&self, ctx: &ContextImpl, name: Atom) -> Result<JSValue, i32> {
        // safer to test recursion in all proxy methods
        if js_check_stack_overflow(ctx.rt_ref(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        if self.is_revoked() {
            ctx.throw_type_error_revoked_proxy();
            return Err(-1);
        }

        let mut method = self.handler.get_property(ctx, name)?;
        if method.is_null() {
            method = JSValue::UNDEFINED;
        }
        Ok(method)
    }

    pub fn prevent_extensions(&mut self, ctx: &ContextImpl) -> Result<bool, i32> {
        let method = self.get_method(ctx, Atom::CONST_preventExtensions)?;
        if method.is_undefined() {
            self.target.prevent_extensions(ctx)
        } else {
            let b = method
                .call(ctx, self.handler, &[self.target.into()])?
                .to_bool()?;

            if b && self.target.is_extensible(ctx)? {
                ctx.throw_type_error("proxy: inconsistent preventExtensions");
                return Err(-1);
            }

            Ok(b)
        }
    }

    pub fn get_prototype_of(&self, ctx: &ContextImpl) -> Result<JSValue, i32> {
        let method = self.get_method(ctx, Atom::CONST_getPrototypeOf)?;
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
        &mut self,
        ctx: &ContextImpl,
        proto_val: JSValue,
        throw: bool,
    ) -> Result<bool, i32> {
        let method = self.get_method(ctx, Atom::CONST_setPrototypeOf)?;
        if method.is_undefined() {
            return self.target.set_prototype_internal(ctx, proto_val, throw);
        }

        let ret = method.call(ctx, self.handler, &[self.target.into(), proto_val])?;

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
                if !self.target.is_extensible(ctx)?
                    && proto_val
                        != self
                            .target
                            .prototype(ctx)?
                            .map_or(JSValue::NULL, JSValue::Object)
                {
                    ctx.throw_type_error("proxy: inconsistent prototype");
                    return Err(-1);
                }
                Ok(true)
            }
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct Proxy(pub(super) GcRef<ObjectImpl>);

object_cast!(Proxy, JS_CLASS_PROXY);

impl Proxy {
    pub fn class_initialize(ctx: Context, _: ClassID) -> Result<Option<ObjectRef>, i32> {
        let mut proto =
            ctx.create_native_function(Callback::Constructor(js_proxy_constructor), "Proxy", 2, 0)?;

        proto.set_flag_constructor(true);
        proto.install_field_defs(&ctx, &js_proxy_funcs).unwrap();

        ctx.global_obj().define_property_value_str(
            &ctx,
            "Proxy",
            proto.into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;

        Ok(Some(proto))
    }

    pub fn class_call(
        ctx: Context,
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

        let method = px.get_method(&ctx, Atom::CONST_apply)?;
        if method.is_undefined() {
            return JSValue::from(px.target).call(&ctx, this_obj, args);
        }

        let arg_array = ctx.create_array_from_slice(args)?;
        method.call(
            &ctx,
            px.handler,
            &[px.target.into(), this_obj, arg_array.into()],
        )
    }

    /// get proxy payload reference
    #[inline(always)]
    pub(super) fn payload(&self) -> &ProxyPayload {
        if let ObjectPayload::PROXY(s) = unsafe { self.0.as_ref().payload() } {
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
        if let ObjectPayload::PROXY(s) = unsafe { self.0.as_mut().payload_mut() } {
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
    pub fn get_method(&self, ctx: &ContextImpl, name: Atom) -> Result<JSValue, i32> {
        self.payload().get_method(ctx, name)
    }

    pub fn is_array(&self, ctx: &ContextImpl) -> Result<bool, i32> {
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
    pub fn is_extensible(&self, ctx: &ContextImpl) -> Result<bool, i32> {
        self.payload().is_extensible(ctx)
    }

    #[inline(always)]
    pub fn prevent_extensions(&mut self, ctx: &ContextImpl) -> Result<bool, i32> {
        self.payload_mut().prevent_extensions(ctx)
    }

    /// returns Object, NULL or Err.
    #[inline(always)]
    pub fn get_prototype_of(&self, ctx: &ContextImpl) -> Result<JSValue, i32> {
        self.payload().get_prototype_of(ctx)
    }

    #[inline(always)]
    pub fn set_prototype_of(
        &mut self,
        ctx: &ContextImpl,
        proto_val: JSValue,
        throw: bool,
    ) -> Result<bool, i32> {
        self.payload_mut().set_prototype_of(ctx, proto_val, throw)
    }
}

fn js_proxy_call_constructor(
    ctx: ContextPtr,
    func_obj: &Proxy,
    new_target: JSValue,
    args: &[JSValue],
) -> JSResult {
    let s = func_obj.payload();
    if !s.target.is_constructor() {
        ctx.throw_type_error("not a constructor");
        return Err(-1);
    }

    let method = s.get_method(&ctx, Atom::CONST_construct)?;
    if method.is_undefined() {
        return JS_CallConstructor2(&ctx, s.target.into(), new_target, args).err();
    }

    let arg_array = ctx.create_array_from_slice(args)?;
    let ret = method.call(
        &ctx,
        s.handler,
        &[s.target.into(), arg_array.into(), new_target],
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
    ctx: ContextPtr,
    obj: JSValue,
    prop: Atom,
) -> Result<Option<JSPropertyDescriptor>, i32> {
    let cx = ctx.deref();

    let Some(px) = obj
        .as_object()
        .and_then(|x| x.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = px.get_method(cx, Atom::CONST_getOwnPropertyDescriptor)?;
    if method.is_undefined() {
        return px.target.get_own_property(cx, prop);
    }

    let trap_result_obj = method.call(
        cx,
        px.handler,
        &[px.target.into(), prop.string_or_symbol(cx)?],
    )?;
    if !matches!(trap_result_obj, JSValue::Object(_) | JSValue::UNDEFINED) {
        cx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor ");
        return Err(-1);
    }

    let target_desc = px.target.get_own_property(cx, prop)?;

    if let JSValue::Object(trap_result_obj) = trap_result_obj {
        let result_desc = JSPropertyDescriptor::parse(cx, trap_result_obj)?;

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
                cx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }
        } else if !px.target.is_extensible(cx)? {
            cx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
            return Err(-1);
        }

        if result_desc.flags & JS_PROP_CONFIGURABLE == 0 {
            if target_desc
                .as_ref()
                .is_none_or(|d| d.flags & JS_PROP_CONFIGURABLE != 0)
            {
                cx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }

            if result_desc.flags & (JS_PROP_GETSET | JS_PROP_WRITABLE) == 0
                && target_desc
                    .as_ref()
                    .is_some_and(|d| d.flags & JS_PROP_WRITABLE != 0)
            {
                // proxy-missing-checks
                cx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
                return Err(-1);
            }
        }

        Ok(Some(result_desc))
    } else {
        // trap_result_obj is undefined
        if let Some(desc) = target_desc
            && ((desc.flags & JS_PROP_CONFIGURABLE == 0) || !px.target.flags().extensible())
        {
            cx.throw_type_error("proxy: inconsistent getOwnPropertyDescriptor");
            return Err(-1);
        }

        Ok(None)
    }
}

fn js_proxy_define_own_property(
    ctx: ContextPtr,
    obj: JSValue,
    prop: Atom,
    value: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let cx = ctx.deref();

    let flags = flags as u32;

    let Some(px) = obj
        .as_object()
        .and_then(|x| x.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = px.get_method(cx, Atom::CONST_defineProperty)?;
    if method.is_undefined() {
        return {
            let mut t = px.target;
            t.define_property(cx, prop, value, getter, setter, flags)
        };
    }

    let ret = method
        .call(
            cx,
            px.handler,
            &[
                px.target.into(),
                prop.string_or_symbol(cx)?,
                JSPropertyDescriptor {
                    value,
                    getter,
                    setter,
                    flags,
                }
                .make_object(cx)?
                .into(),
            ],
        )?
        .to_bool()?;

    if !ret {
        return if flags & JS_PROP_THROW != 0 {
            cx.throw_type_error("proxy: defineProperty exception");
            Err(-1)
        } else {
            Ok(false)
        };
    }

    let setting_not_configurable =
        flags & (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE) == JS_PROP_HAS_CONFIGURABLE;

    if let Some(desc) = px.target.get_own_property(cx, prop)? {
        if !check_define_prop_flags(desc.flags, flags)
            || (desc.flags & JS_PROP_CONFIGURABLE != 0 && setting_not_configurable)
        {
            cx.throw_type_error("proxy: inconsistent defineProperty");
            return Err(-1);
        }

        if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
            if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE) == JS_PROP_GETSET {
                if (flags & JS_PROP_HAS_GET) != 0 && !getter.is_same(desc.getter) {
                    cx.throw_type_error("proxy: inconsistent defineProperty");
                    return Err(-1);
                }
                if (flags & JS_PROP_HAS_SET) != 0 && !setter.is_same(desc.setter) {
                    cx.throw_type_error("proxy: inconsistent defineProperty");
                    return Err(-1);
                }
            }
        } else if flags & JS_PROP_HAS_VALUE != 0 {
            if desc.flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == JS_PROP_WRITABLE
                && flags & JS_PROP_WRITABLE == 0
            {
                // missing-proxy-check feature
                cx.throw_type_error("proxy: inconsistent defineProperty");
                return Err(-1);
            } else if desc.flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                && !value.is_same(desc.value)
            {
                cx.throw_type_error("proxy: inconsistent defineProperty");
                return Err(-1);
            }
        }
        if flags & JS_PROP_HAS_WRITABLE != 0
            && (desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE))
                == JS_PROP_WRITABLE
        {
            // proxy-missing-checks
            cx.throw_type_error("proxy: inconsistent defineProperty");
            return Err(-1);
        }
    } else if setting_not_configurable || !px.target.flags().extensible() {
        cx.throw_type_error("proxy: inconsistent defineProperty");
        return Err(-1);
    }

    Ok(true)
}

fn js_proxy_delete_property(ctx: ContextPtr, mut obj: JSValue, atom: Atom) -> Result<bool, i32> {
    let Some(px) = obj
        .as_object_mut()
        .and_then(|x| x.as_Proxy_mut())
        .map(|x| x.payload_mut())
    else {
        return Err(-1);
    };

    let method = px.get_method(&ctx, Atom::CONST_deleteProperty)?;
    if method.is_undefined() {
        return px.target.delete_property(&ctx, atom, 0);
    }

    let res = method
        .call(
            &ctx,
            px.handler,
            &[px.target.into(), atom.string_or_symbol(&ctx)?],
        )?
        .to_bool()?;

    if res && let Some(desc) = px.target.get_own_property(&ctx, atom)? {
        if desc.flags & JS_PROP_CONFIGURABLE == 0 {
            ctx.throw_type_error("proxy: inconsistent deleteProperty");
            return Err(-1);
        }
        if !px.target.is_extensible(&ctx)? {
            ctx.throw_type_error("proxy: inconsistent deleteProperty");
            return Err(-1);
        }
    }

    Ok(res)
}

fn js_proxy_get_own_property_names(
    ctx: ContextPtr,
    mut obj: ObjectRef,
) -> Result<Box<[PropertyKey]>, i32> {
    let cx = &*ctx;

    let Some(px) = obj.as_Proxy_mut().map(|x| x.payload_mut()) else {
        return Err(-1);
    };

    if px.is_revoked() {
        cx.throw_type_error_revoked_proxy();
        return Err(-1);
    }

    let method = px.get_method(cx, Atom::CONST_ownKeys)?;
    if method.is_undefined() {
        return px
            .target
            .get_own_property_names(cx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK);
    }

    let prop_array = method.call(cx, px.handler, &[px.target.into()])?;
    let len = prop_array.get_length_prop(cx)? as usize;

    let mut tab = Vec::<PropertyKey>::new();
    if len > 0 {
        tab.try_reserve_exact(len).map_err(|_| {
            cx.throw_out_of_memory();
            -1
        })?;
    }

    for i in 0..len {
        let val = prop_array.get_property_u32(cx, i as u32)?;
        if !matches!(val, JSValue::String(_) | JSValue::Symbol(_)) {
            cx.throw_type_error("proxy: properties must be strings or symbols");
            return Err(-1);
        }

        let atom = val.to_atom(cx)?;

        if !tab.is_empty() && tab.iter().any(|k| k.atom == atom) {
            cx.throw_type_error("proxy: duplicate property");
            return Err(-1);
        }
        tab.push(PropertyKey::new(atom, false, false));
    }

    // check if there are non configurable properties
    let is_extensible = px.target.is_extensible(cx)?;

    for k2 in px
        .target
        .get_own_property_names(cx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK)?
    {
        if let Some(desc) = px.target.get_own_property(cx, k2.atom)? {
            // safety, property should be found
            if desc.flags & JS_PROP_CONFIGURABLE == 0 || !is_extensible {
                if let Some(p) = tab.iter_mut().find(|p| p.atom == k2.atom) {
                    // mark the property as found
                    if !is_extensible {
                        p.is_enumerable = true;
                    }
                } else {
                    cx.throw_type_error("proxy: target property must be present in proxy ownKeys");
                    return Err(-1);
                }
            }
        }
    }

    if !is_extensible {
        // check that all property in 'tab' were checked
        if tab.iter().any(|p| !p.is_enumerable) {
            // found an item with is_enumerable=FALSE
            cx.throw_type_error(
                "proxy: property not present in target were returned by non extensible proxy",
            );
            return Err(-1);
        }
    }

    Ok(tab.into_boxed_slice())
}

fn js_proxy_has(ctx: ContextPtr, mut obj: ObjectRef, atom: Atom) -> Result<bool, i32> {
    let cx = &*ctx;

    let Some(px) = obj.as_Proxy_mut().map(|x| x.payload_mut()) else {
        return Err(-1);
    };

    let method = px.get_method(cx, Atom::CONST_has)?;
    if method.is_undefined() {
        return px.target.has_property(cx, atom);
    }

    let ret = method
        .call(
            cx,
            px.handler,
            &[px.target.into(), atom.string_or_symbol(cx)?],
        )?
        .to_bool()?;

    if !ret {
        if let Some(desc) = px.target.get_own_property(cx, atom)?
            && (desc.flags & JS_PROP_CONFIGURABLE == 0 || !px.target.flags().extensible())
        {
            cx.throw_type_error("proxy: inconsistent has");
            return Err(-1);
        }

        if let Some(desc) = px.target.get_own_property(cx, atom)?
            && (desc.flags & JS_PROP_CONFIGURABLE == 0 || !px.target.flags().extensible())
        {
            cx.throw_type_error("proxy: inconsistent has");
            return Err(-1);
        }
    }

    Ok(ret)
}

fn js_proxy_get(ctx: ContextPtr, obj: JSValue, atom: Atom, receiver: JSValue) -> JSResult {
    let cx = &*ctx;

    let Some(px) = obj
        .as_object()
        .and_then(|p| p.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = px.get_method(cx, Atom::CONST_get)?;
    // Note: recursion is possible thru the prototype of s.target
    if method.is_undefined() {
        return px.target.get_property_internal(cx, atom, receiver, false);
    }

    let ret = method.call(
        cx,
        px.handler,
        &[px.target.into(), atom.string_or_symbol(cx)?, receiver],
    )?;

    if let Some(desc) = px.target.get_own_property(cx, atom)? {
        if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0 {
            if !desc.value.is_same(ret) {
                cx.throw_type_error("proxy: inconsistent get ");
                return Err(-1);
            }
        } else if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE) == JS_PROP_GETSET
            && desc.getter.is_undefined()
            && !ret.is_undefined()
        {
            cx.throw_type_error("proxy: inconsistent get ");
            return Err(-1);
        }
    }

    Ok(ret)
}

fn js_proxy_set(
    ctx: ContextPtr,
    obj: JSValue,
    atom: Atom,
    value: JSValue,
    receiver: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let cx = &*ctx;

    let Some(s) = obj
        .as_object()
        .and_then(|p| p.as_Proxy())
        .map(|x| x.payload())
    else {
        return Err(-1);
    };

    let method = s.get_method(cx, Atom::CONST_set)?;
    if method.is_undefined() {
        return s.target.set_property_internal(
            cx,
            atom,
            value,
            match receiver {
                JSValue::NULL | JSValue::UNDEFINED => None,
                JSValue::Object(o) => Some(o),
                _ => {
                    cx.throw_type_error("receiver must be an object");
                    return Err(-1);
                }
            },
            flags as u32,
        );
    }

    let ret = method
        .call(
            cx,
            s.handler,
            &[s.target.into(), atom.string_or_symbol(cx)?, value, receiver],
        )?
        .to_bool()?;

    if ret {
        if let Some(desc) = s.target.get_own_property(cx, atom)? {
            if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0 {
                if !desc.value.is_same(value) {
                    cx.throw_type_error("proxy: inconsistent set");
                    return Err(-1);
                }
            } else if desc.flags & (JS_PROP_GETSET | JS_PROP_CONFIGURABLE) == JS_PROP_GETSET
                && desc.setter.is_undefined()
            {
                cx.throw_type_error("proxy: inconsistent set");
                return Err(-1);
            }
        }
    } else if cx.can_throw(flags as u32) {
        cx.throw_type_error("proxy: cannot set property");
        return Err(-1);
    }

    Ok(ret)
}

pub(crate) const js_proxy_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    get_own_property: Some(js_proxy_get_own_property),
    define_own_property: Some(js_proxy_define_own_property),
    delete_property: Some(js_proxy_delete_property),
    get_own_property_names: Some(js_proxy_get_own_property_names),
    has_property: Some(js_proxy_has),
    get_property: Some(js_proxy_get),
    set_property: Some(js_proxy_set),
};
