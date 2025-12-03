use std::ops::Deref;

use crate::{
    atom::Atom,
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW,
    },
    utils::unlikely,
    value::JSValue,
};

pub fn js_object_toString(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let txt = match cc.this() {
        JSValue::NULL => "[object Null]".to_string(),
        JSValue::UNDEFINED => "[object Undefined]".to_string(),
        JSValue::Object(o) => o.inspect(cx)?,
        v => v.objectify(cx)?.inspect(cx)?,
    };

    cx.create_string_from_utf8(txt.into_bytes())
        .map(JSValue::String)
}

pub fn js_object_toLocaleString(ctx: ContextRef, cc: CallContext) -> JSResult {
    cc.this().invoke(&ctx, Atom::CONST_toString, &[])
}

pub fn js_object_valueOf(ctx: ContextRef, cc: CallContext) -> JSResult {
    cc.this().objectify(&ctx).map(JSValue::Object)
}

pub fn js_object_hasOwnProperty(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    // must be done first
    let atom = cc.arg(0).to_atom(cx)?;
    let obj = cc.this().objectify(cx)?;
    Ok(obj.get_own_property(cx, atom)?.is_some().into())
}

pub fn js_object_isPrototypeOf(ctx: ContextRef, cc: CallContext) -> JSResult {
    if let JSValue::Object(ctor) = *cc.arg(0) {
        let obj = cc.this().objectify(&ctx)?;

        let mut cur_obj = ctor;
        let b = loop {
            if let Some(p) = cur_obj.prototype(&ctx)? {
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

pub fn js_object_propertyIsEnumerable(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    let obj = cc.this().objectify(cx)?;
    let atom = cc.arg(0).to_atom(cx)?;

    Ok(if let Some(desc) = obj.get_own_property(cx, atom)? {
        JSValue::Bool(desc.flags & JS_PROP_ENUMERABLE != 0)
    } else {
        JSValue::Bool(false)
    })
}

pub fn js_object_getset___proto__(ctx: ContextRef, cc: CallContext) -> JSResult {
    let cx = ctx.deref();

    if cc.length() == 0 {
        // getter
        let p = cc.this().objectify(cx).and_then(|o| o.prototype(cx))?;
        Ok(p.map_or(JSValue::NULL, JSValue::Object))
    } else {
        // setter
        let mut this_val = *cc.this();
        if this_val.is_undefined_or_null() {
            cx.throw_type_error_not_an_object();
            return Err(-1);
        }

        let proto = *cc.arg(0);
        if !proto.is_object() && !proto.is_null() {
            // `proto` is neither an object nor null
            Ok(JSValue::UNDEFINED)
        } else {
            // `proto` is an object or null
            this_val.set_prototype_internal(cx, proto, true)?;
            Ok(JSValue::UNDEFINED)
        }
    }
}

// magic = 1 if called as __defineSetter__
pub unsafe fn js_object___defineGetter__(
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

    let prop = *argv.offset(0);
    let value = *argv.offset(1);

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

pub unsafe fn js_object___lookupGetter__(
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

    let prop = (*argv.offset(0)).to_atom(ctx).unwrap_or(Atom::NULL);
    if prop.is_null() {
        return JSValue::EXCEPTION;
    }

    let res = loop {
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

        // obj = obj.prototype(ctx).unwrap_or(JSValue::EXCEPTION);
        // if obj.is_exception() {
        //     // obj is Proxy
        //     JS_FreeAtom(ctx, prop);
        //     return JSValue::EXCEPTION;
        // } else if obj.is_null() {
        //     res = JSValue::UNDEFINED;
        //     break;
        // }

        // // avoid infinite loop (possible with proxies)
        // if ctx.poll_interrupts().is_err() {
        //     JS_FreeAtom(ctx, prop);
        //     return JSValue::EXCEPTION;
        // }
    };

    res
}

pub static js_object_proto_funcs: [FieldDef; 7] = [
    FieldDef::FUNCTION_V1("toString", 0, js_object_toString, 0),
    FieldDef::FUNCTION_V1("toLocaleString", 0, js_object_toLocaleString, 0),
    FieldDef::FUNCTION_V1("valueOf", 0, js_object_valueOf, 0),
    FieldDef::FUNCTION_V1("hasOwnProperty", 1, js_object_hasOwnProperty, 0),
    FieldDef::FUNCTION_V1("isPrototypeOf", 1, js_object_isPrototypeOf, 0),
    FieldDef::FUNCTION_V1("propertyIsEnumerable", 1, js_object_propertyIsEnumerable, 0),
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
