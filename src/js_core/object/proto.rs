use std::ops::Deref;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF,
    atom::{Atom, consts::*},
    branch_hints::unlikely,
    class::ClassID,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW, JSObject,
    },
    value::JSValue,
};

pub fn js_object_toString<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    let txt = match cc.this() {
        JSValue::NULL => "[object Null]".to_string(),
        JSValue::UNDEFINED => "[object Undefined]".to_string(),
        JSValue::Object(o) => o.inspect(ctx)?,
        v => v.clone().objectify(ctx)?.inspect(ctx)?,
    };

    ctx.create_string_from_utf8(txt.into_bytes())
        .map(JSValue::String)
}

pub fn js_object_toLocaleString<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    cc.this().invoke(ctx, JS_ATOM_toString.deref(), &[])
}

pub fn js_object_valueOf<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    cc.this().clone().objectify(ctx).map(JSValue::Object)
}

pub fn js_object_hasOwnProperty<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    // must be done first
    let atom = cc.arg(0).to_atom(ctx)?;
    let obj = cc.this().clone().objectify(ctx)?;
    Ok(obj.get_own_property(ctx, &atom)?.is_some().into())
}

pub fn js_object_isPrototypeOf<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    if let JSValue::Object(ctor) = cc.arg(0).clone() {
        let obj = cc.this().clone().objectify(ctx)?;

        let mut cur_obj = ctor.clone();
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

pub fn js_object_propertyIsEnumerable<'a>(
    ctx: &JSContext<'a>,
    cc: CallContext<'_, 'a>,
) -> JSResult<'a> {
    let obj = cc.this().clone().objectify(ctx)?;
    let atom = cc.arg(0).to_atom(ctx)?;

    Ok(if let Some(desc) = obj.get_own_property(ctx, &atom)? {
        JSValue::Bool(desc.flags & JS_PROP_ENUMERABLE != 0)
    } else {
        JSValue::Bool(false)
    })
}

pub fn js_object_getset___proto__<'a>(
    ctx: &JSContext<'a>,
    cc: CallContext<'_, 'a>,
) -> JSResult<'a> {
    if cc.length() == 0 {
        // getter
        let p = cc
            .this()
            .clone()
            .objectify(ctx)
            .and_then(|o| o.prototype(ctx))?;
        Ok(p.map_or(JSValue::NULL, JSValue::Object))
    } else {
        // setter
        let this_val = cc.this();
        if this_val.is_undefined_or_null() {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }

        let proto = cc.arg(0);
        if !proto.is_object() && !proto.is_null() {
            // `proto` is neither an object nor null
            Ok(JSValue::UNDEFINED)
        } else {
            // `proto` is an object or null
            this_val.set_prototype_internal(ctx, proto.clone(), true)?;
            Ok(JSValue::UNDEFINED)
        }
    }
}

// magic = 1 if called as __defineSetter__
pub unsafe fn js_object___defineGetter__<'a>(
    ctx: &JSContext<'a>,
    this_val: JSValue<'a>,
    _argc: i32,
    argv: *mut JSValue<'a>,
    magic: i32,
) -> JSValue<'a> {
    let obj = this_val
        .objectify(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::Object);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let prop = &(*argv.offset(0));
    let value = &(*argv.offset(1));

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
        (JSValue::UNDEFINED, value.clone())
    } else {
        flags |= JS_PROP_HAS_GET;
        (value.clone(), JSValue::UNDEFINED)
    };

    let ret = obj.define_property(ctx, &atom, JSValue::UNDEFINED, get, set, flags);
    if ret.is_err() {
        JSValue::EXCEPTION
    } else {
        JSValue::UNDEFINED
    }
}

pub unsafe fn js_object___lookupGetter__<'a>(
    ctx: &JSContext<'a>,
    this_val: JSValue<'a>,
    argc: i32,
    argv: *mut JSValue<'a>,
    setter: i32,
) -> JSValue<'a> {
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
        match obj.get_own_property(ctx, &prop) {
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
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_object_toString, 0),
    JS_CFUNC_NATIVE_DEF!("toLocaleString", 0, js_object_toLocaleString, 0),
    JS_CFUNC_NATIVE_DEF!("valueOf", 0, js_object_valueOf, 0),
    JS_CFUNC_NATIVE_DEF!("hasOwnProperty", 1, js_object_hasOwnProperty, 0),
    JS_CFUNC_NATIVE_DEF!("isPrototypeOf", 1, js_object_isPrototypeOf, 0),
    JS_CFUNC_NATIVE_DEF!("propertyIsEnumerable", 1, js_object_propertyIsEnumerable, 0),
    JS_CGETSET_NATIVE_DEF!(
        "__proto__",
        js_object_getset___proto__,
        js_object_getset___proto__,
        0
    ),
    // JS_CFUNC_MAGIC_DEF!(c"__defineGetter__", 2, js_object___defineGetter__, 0),
    // JS_CFUNC_MAGIC_DEF!(c"__defineSetter__", 2, js_object___defineGetter__, 1),
    // JS_CFUNC_MAGIC_DEF!(c"__lookupGetter__", 1, js_object___lookupGetter__, 0),
    // JS_CFUNC_MAGIC_DEF!(c"__lookupSetter__", 1, js_object___lookupGetter__, 1),
];
