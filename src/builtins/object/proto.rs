// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW,
    },
    value::JSValue,
};

pub(crate) fn js_object_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let txt = {
        let val = cc.this();
        match val.tag() {
            crate::JSValueTag::Null => "[object Null]".to_string(),
            crate::JSValueTag::Undefined => "[object Undefined]".to_string(),
            crate::JSValueTag::Object => unsafe { val.as_object_unchecked().repr(ctx)? },
            _ => val.objectify(ctx)?.repr(ctx)?,
        }
    };

    ctx.create_string_from_utf8(txt.into_bytes())
        .map(JSValue::make_string)
}

pub(crate) fn js_object_toLocaleString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    cc.this().invoke(ctx, Atom::CONST_toString, &[])
}

pub(crate) fn js_object_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    cc.this().objectify(ctx).map(JSValue::from)
}

pub(crate) fn js_object_hasOwnProperty(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let atom = cc.arg(0).to_atom(ctx)?; // must be done first
    Ok(cc
        .this()
        .objectify(ctx)
        .and_then(|mut o| o.get_own_property(ctx, atom))?
        .is_some()
        .into())
}

pub(crate) fn js_object_isPrototypeOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(ctor) = cc.arg(0).as_object() {
        let obj = cc.this().objectify(ctx)?;
        let mut cur_obj = ctor;

        let b = loop {
            if let Some(p) = cur_obj.prototype(ctx)? {
                if p == obj {
                    break true;
                } else {
                    cur_obj = p;
                }
            } else {
                break false;
            };

            // avoid infinite loop (possible with proxies)
            if ctx.poll_interrupts() {
                return Err(-1);
            }
        };

        Ok(b.into())
    } else {
        Ok(JSValue::from(false))
    }
}

pub(crate) fn js_object_propertyIsEnumerable(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = cc.this().objectify(ctx)?;
    let atom = cc.arg(0).to_atom(ctx)?;

    Ok(obj
        .get_own_property(ctx, atom)?
        .is_some_and(|desc| desc.flags & JS_PROP_ENUMERABLE != 0)
        .into())
}

/// `Object.prototype.__proto__` getter (B.2.2.1.1)
pub(crate) fn js_object_get___proto__(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let p = cc.this().objectify(ctx).and_then(|o| o.prototype(ctx))?;
    Ok(p.map_or(JSValue::NULL, JSValue::from))
}

/// `Object.prototype.__proto__` setter (B.2.2.1.2)
pub(crate) fn js_object_set___proto__(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // 1. Let O be ? RequireObjectCoercible(this value).
    let this = cc.this();
    if this.is_undefined_or_null() {
        return Err(ctx.throw_type_error_not_an_object());
    }

    // 2. If Type(proto) is neither Object nor Null, return undefined.
    let proto = cc.arg(0);
    if !proto.is_object() && !proto.is_null() {
        return Ok(JSValue::UNDEFINED);
    }

    // 3. If Type(O) is not Object, return undefined.
    let Some(mut obj) = this.as_object() else {
        return Ok(JSValue::UNDEFINED);
    };

    // 4-5. Let status be ? O.[[SetPrototypeOf]](proto); if false, throw TypeError.
    obj.set_prototype_internal(ctx, proto, true)?;
    Ok(JSValue::UNDEFINED)
}

/// `Object.prototype.__defineGetter__` / `__defineSetter__` (Annex B).
/// magic = 1 when called as `__defineSetter__`.
pub(crate) fn js_object_getset_define(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_setter = cc.magic() != 0;
    // 1. Let O be ? ToObject(this value)
    let mut obj = cc.this().objectify(ctx)?;

    // 2. If IsCallable(getter) is false, throw a TypeError (before ToPropertyKey)
    let func = cc.arg(1);
    if !func.is_callable() {
        return Err(ctx.throw_type_error("not a function"));
    }

    // 3. Let key be ? ToPropertyKey(P)
    let atom = cc.arg(0).to_atom(ctx)?;

    // 4-5. DefinePropertyOrThrow(O, key, {[[Get]]/[[Set]]: func, [[Enumerable]]: true, [[Configurable]]: true})
    let mut flags = JS_PROP_THROW
        | JS_PROP_HAS_ENUMERABLE
        | JS_PROP_ENUMERABLE
        | JS_PROP_HAS_CONFIGURABLE
        | JS_PROP_CONFIGURABLE;
    let (get, set) = if is_setter {
        flags |= JS_PROP_HAS_SET;
        (JSValue::UNDEFINED, func)
    } else {
        flags |= JS_PROP_HAS_GET;
        (func, JSValue::UNDEFINED)
    };

    obj.define_property(ctx, atom, JSValue::UNDEFINED, get, set, flags)?;
    Ok(JSValue::UNDEFINED)
}

/// `Object.prototype.__lookupGetter__` / `__lookupSetter__` (Annex B).
/// magic = 1 when called as `__lookupSetter__`.
pub(crate) fn js_object_getset_lookup(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_setter = cc.magic() != 0;
    let mut obj = cc.this().objectify(ctx)?;
    let atom = cc.arg(0).to_atom(ctx)?;

    loop {
        if let Some(desc) = obj.get_own_property(ctx, atom)? {
            return Ok(if desc.flags & JS_PROP_GETSET != 0 {
                if is_setter { desc.setter } else { desc.getter }
            } else {
                JSValue::UNDEFINED
            });
        }

        obj = match obj.prototype(ctx)? {
            Some(p) => p,
            None => return Ok(JSValue::UNDEFINED),
        };

        // avoid infinite loop (possible with proxies)
        if ctx.poll_interrupts() {
            return Err(-1);
        }
    }
}

pub const js_object_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_object_toString, 0),
    FieldDef::FUNC("toLocaleString", 0, js_object_toLocaleString, 0),
    FieldDef::FUNC("valueOf", 0, js_object_valueOf, 0),
    FieldDef::FUNC("hasOwnProperty", 1, js_object_hasOwnProperty, 0),
    FieldDef::FUNC("isPrototypeOf", 1, js_object_isPrototypeOf, 0),
    FieldDef::FUNC("propertyIsEnumerable", 1, js_object_propertyIsEnumerable, 0),
    FieldDef::GETSET(
        "__proto__",
        Some(js_object_get___proto__),
        Some(js_object_set___proto__),
        0,
    ),
    FieldDef::FUNC("__defineGetter__", 2, js_object_getset_define, 0),
    FieldDef::FUNC("__defineSetter__", 2, js_object_getset_define, 1),
    FieldDef::FUNC("__lookupGetter__", 1, js_object_getset_lookup, 0),
    FieldDef::FUNC("__lookupSetter__", 1, js_object_getset_lookup, 1),
];
