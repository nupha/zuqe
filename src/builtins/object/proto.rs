// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ContextImpl,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW,
    },
    utils::unlikely,
    value::JSValue,
};

pub(crate) fn js_object_toString(cc: CallContext) -> JSResult {
    let cx = cc.ctx();

    let txt = match cc.this() {
        JSValue::NULL => "[object Null]".to_string(),
        JSValue::UNDEFINED => "[object Undefined]".to_string(),
        JSValue::Object(o) => o.inspect(cx)?,
        v => v.objectify(cx)?.inspect(cx)?,
    };

    cx.create_string_from_utf8(txt.into_bytes())
        .map(JSValue::String)
}

pub(crate) fn js_object_toLocaleString(cc: CallContext) -> JSResult {
    cc.this().invoke(cc.ctx(), Atom::CONST_toString, &[])
}

pub(crate) fn js_object_valueOf(cc: CallContext) -> JSResult {
    cc.this().objectify(cc.ctx()).map(JSValue::Object)
}

pub(crate) fn js_object_hasOwnProperty(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    // must be done first
    let atom = cc.arg(0).to_atom(ctx)?;
    let obj = cc.this().objectify(ctx)?;
    Ok(obj.get_own_property(ctx, atom)?.is_some().into())
}

pub(crate) fn js_object_isPrototypeOf(cc: CallContext) -> JSResult {
    if let JSValue::Object(ctor) = *cc.arg(0) {
        let ctx = cc.ctx();
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
            ctx.poll_interrupts()?;
        };

        Ok(b.into())
    } else {
        Ok(JSValue::Bool(false))
    }
}

pub(crate) fn js_object_propertyIsEnumerable(cc: CallContext) -> JSResult {
    let cx = cc.ctx();
    let obj = cc.this().objectify(cx)?;
    let atom = cc.arg(0).to_atom(cx)?;

    Ok(if let Some(desc) = obj.get_own_property(cx, atom)? {
        JSValue::Bool(desc.flags & JS_PROP_ENUMERABLE != 0)
    } else {
        JSValue::Bool(false)
    })
}

pub(crate) fn js_object_getset___proto__(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if cc.length() == 0 {
        // getter
        let p = cc.this().objectify(ctx).and_then(|o| o.prototype(ctx))?;
        Ok(p.map_or(JSValue::NULL, JSValue::Object))
    } else {
        // setter
        let mut this_val = *cc.this();
        if this_val.is_undefined_or_null() {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }

        let proto = *cc.arg(0);
        if !proto.is_object() && !proto.is_null() {
            // `proto` is neither an object nor null
            Ok(JSValue::UNDEFINED)
        } else {
            // `proto` is an object or null
            this_val.set_prototype_internal(ctx, proto, true)?;
            Ok(JSValue::UNDEFINED)
        }
    }
}

// magic = 1 if called as __defineSetter__
unsafe fn js_object___defineGetter__(
    ctx: &ContextImpl,
    this_val: JSValue,
    _argc: i32,
    argv: *mut JSValue,
    magic: i32,
) -> JSValue {
    let mut obj = this_val
        .objectify(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::Object);

    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let prop = unsafe { *argv.offset(0) };
    let value = unsafe { *argv.offset(1) };

    if unlikely(value.ensure_function(ctx).is_err()) {
        return JSValue::EXCEPTION;
    }

    let atom = prop.to_atom(ctx).unwrap_or(Atom::NULL);
    if atom.is_null() {
        return JSValue::EXCEPTION;
    }

    let mut flags = JS_PROP_THROW
        | JS_PROP_HAS_ENUMERABLE
        | JS_PROP_ENUMERABLE
        | JS_PROP_HAS_CONFIGURABLE
        | JS_PROP_CONFIGURABLE;

    let (get, set) = if magic != 0 {
        flags |= JS_PROP_HAS_SET;
        (JSValue::UNDEFINED, value)
    } else {
        flags |= JS_PROP_HAS_GET;
        (value, JSValue::UNDEFINED)
    };

    let ret = obj.define_property(ctx, atom, JSValue::UNDEFINED, get, set, flags);
    if ret.is_err() {
        JSValue::EXCEPTION
    } else {
        JSValue::UNDEFINED
    }
}

unsafe fn js_object___lookupGetter__(
    ctx: &ContextImpl,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
    setter: i32,
) -> JSValue {
    let mut obj = this_val
        .objectify(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::Object);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let prop = unsafe { (*argv.offset(0)).to_atom(ctx).unwrap_or(Atom::NULL) };
    if prop.is_null() {
        return JSValue::EXCEPTION;
    }

    loop {
        match obj.get_own_property(ctx, prop) {
            Ok(Some(desc)) => {
                break if desc.flags & JS_PROP_GETSET != 0 {
                    if setter != 0 {
                        desc.setter
                    } else {
                        desc.getter
                    }
                } else {
                    JSValue::UNDEFINED
                };
            }
            Ok(None) => {
                obj = obj.prototype(ctx).unwrap_or(JSValue::EXCEPTION);
                if obj.is_exception() {
                    // obj is Proxy
                    break JSValue::EXCEPTION;
                } else if obj.is_null() {
                    break JSValue::UNDEFINED;
                }

                // avoid infinite loop (possible with proxies)
                if ctx.poll_interrupts().is_err() {
                    break JSValue::EXCEPTION;
                }
            }
            Err(_) => {
                break JSValue::EXCEPTION;
            }
        }
    }
}

pub const js_object_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("toString", 0, js_object_toString, 0),
    FieldDef::CALL("toLocaleString", 0, js_object_toLocaleString, 0),
    FieldDef::CALL("valueOf", 0, js_object_valueOf, 0),
    FieldDef::CALL("hasOwnProperty", 1, js_object_hasOwnProperty, 0),
    FieldDef::CALL("isPrototypeOf", 1, js_object_isPrototypeOf, 0),
    FieldDef::CALL("propertyIsEnumerable", 1, js_object_propertyIsEnumerable, 0),
    FieldDef::GETSET(
        "__proto__",
        Some(js_object_getset___proto__),
        Some(js_object_getset___proto__),
        0,
    ),
    // JS_CFUNC_MAGIC_DEF!(c"__defineGetter__", 2, js_object___defineGetter__, 0),
    // JS_CFUNC_MAGIC_DEF!(c"__defineSetter__", 2, js_object___defineGetter__, 1),
    // JS_CFUNC_MAGIC_DEF!(c"__lookupGetter__", 1, js_object___lookupGetter__, 0),
    // JS_CFUNC_MAGIC_DEF!(c"__lookupSetter__", 1, js_object___lookupGetter__, 1),
];
