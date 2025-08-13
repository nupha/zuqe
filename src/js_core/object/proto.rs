use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN,
    JS_CLASS_DATE, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_NUMBER, JS_CLASS_REGEXP,
    JS_CLASS_STRING,
    atom::{JS_ATOM_NULL, JS_FreeAtom, consts::*},
    context::JSContext,
    error::JSResult,
    function::{CallContext, NativeField},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW,
        JS_SetPrototypeInternal,
    },
    string::JS_ConcatString3,
    value::{JSValue, JSValueConst},
};

pub fn js_object_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut tag_str: JSValue;

    if cc.this().is_null() {
        tag_str = ctx.create_string_from_str("Null")?.into();
    } else if cc.this().is_undefined() {
        tag_str = ctx.create_string_from_str("Undefined")?.into();
    } else {
        let obj = cc.this().to_object(ctx)?;
        let atom = if obj.is_array(ctx)? {
            JS_ATOM_Array
        } else if obj.is_callable() {
            JS_ATOM_Function
        } else {
            match obj.class_id() {
                cid @ (JS_CLASS_STRING
                | JS_CLASS_ARGUMENTS
                | JS_CLASS_MAPPED_ARGUMENTS
                | JS_CLASS_ERROR
                | JS_CLASS_BOOLEAN
                | JS_CLASS_NUMBER
                | JS_CLASS_DATE
                | JS_CLASS_REGEXP) => ctx.rt().class_defs[cid as usize].class_name,
                _ => JS_ATOM_Object,
            }
        };

        tag_str = obj.get_property(ctx, JS_ATOM_Symbol_toStringTag)?;
        if !tag_str.is_string() {
            tag_str = atom.to_string_or_symbol(ctx, true)?;
        }
    }

    unsafe { JS_ConcatString3(ctx, c"[object ", tag_str, c"]").map(JSValue::String) }
}

// pub unsafe fn js_object_toLocaleString(
//     ctx: &JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     JS_Invoke(
//         ctx,
//         this_val,
//         JS_ATOM_toString,
//         0,
//         std::ptr::null_mut(),
//     )
// }

pub fn js_object_toLocaleString(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this().invoke(ctx, JS_ATOM_toString, &[])
}

pub fn js_object_valueOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this().to_object(ctx).map(JSValue::Object)
}

pub fn js_object_hasOwnProperty(ctx: &JSContext, cc: CallContext) -> JSResult {
    // must be done first
    let atom = cc.arg(0).to_atom(ctx)?;
    let obj = cc.this().to_object(ctx).inspect_err(|_| {
        JS_FreeAtom(ctx, atom);
    })?;

    let ret = obj.get_own_property(ctx, atom);
    JS_FreeAtom(ctx, atom);
    Ok(JSValue::Bool(ret?.is_some()))
}

pub fn js_object_isPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(ctor) = cc.arg(0).clone() {
        let this = cc.this().to_object(ctx)?;

        for p in ctor.prototype_iter(ctx, true) {
            if p? == this {
                return Ok(JSValue::Bool(true));
            }

            // avoid infinite loop (possible with proxies)
            ctx.poll_interrupts()?;
        }
    }

    Ok(JSValue::Bool(false))

    // original logic
    // let mut target = cc.arg(0).clone();
    // if !target.is_object() {
    //     return Ok(JSValue::Bool(false));
    // }
    // let ob = cc.this().coerce_to_object(ctx)?;

    // let res: bool = loop {
    //     // walk up prototype chain
    //     target = target.prototype(ctx)?;
    //     if target.is_null() {
    //         break false;
    //     }
    //     if ob == unsafe { target.object_unchecked().clone() } {
    //         break true;
    //     }

    //     // avoid infinite loop (possible with proxies)
    //     ctx.poll_interrupts()?;
    // };
    // Ok(JSValue::Bool(res))
}

pub fn js_object_propertyIsEnumerable(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.this().to_object(ctx)?;
    let prop = cc.arg(0).to_atom(ctx)?;

    let ret = obj.get_own_property(ctx, prop);
    JS_FreeAtom(ctx, prop);

    Ok(if let Some(desc) = ret? {
        JSValue::Bool(desc.flags & JS_PROP_ENUMERABLE != 0)
    } else {
        JSValue::Bool(false)
    })
}

pub fn js_object_getset___proto__(ctx: &JSContext, cc: CallContext) -> JSResult {
    if cc.length() == 0 {
        // getter
        let p = cc.this().to_object(ctx).and_then(|o| o.prototype(ctx))?;
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
            JS_SetPrototypeInternal(ctx, &this_val, proto.clone(), true)?;
            Ok(JSValue::UNDEFINED)
        }
    }
}

// magic = 1 if called as __defineSetter__
pub unsafe fn js_object___defineGetter__(
    ctx: &JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let obj = this_val
        .to_object(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::Object);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let prop = &(*argv.offset(0));
    let value = &(*argv.offset(1));

    if value.ensure_callable(ctx).is_err() {
        return JSValue::EXCEPTION;
    }

    let atom = prop.to_atom(ctx).unwrap_or(JS_ATOM_NULL);
    if atom == JS_ATOM_NULL {
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

    let ret = obj.define_property(ctx, atom, JSValue::UNDEFINED, get, set, flags);
    JS_FreeAtom(ctx, atom);

    if ret.is_err() {
        JSValue::EXCEPTION
    } else {
        JSValue::UNDEFINED
    }
}

pub unsafe fn js_object___lookupGetter__(
    ctx: &JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    setter: i32,
) -> JSValue {
    let mut obj = this_val
        .to_object(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::Object);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let prop = (*argv.offset(0)).to_atom(ctx).unwrap_or(JS_ATOM_NULL);

    if prop == JS_ATOM_NULL {
        JS_FreeAtom(ctx, prop);
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

    JS_FreeAtom(ctx, prop);
    res
}

pub const js_object_proto_funcs: [NativeField; 7] = [
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
    // JS_CFUNC_DEF!(c"toString", 0, js_object_toString),
    // JS_CFUNC_DEF!(c"toLocaleString", 0, js_object_toLocaleString),
    // JS_CFUNC_DEF!(c"valueOf", 0, js_object_valueOf),
    // JS_CFUNC_DEF!(c"hasOwnProperty", 1, js_object_hasOwnProperty),
    // JS_CFUNC_DEF!(c"isPrototypeOf", 1, js_object_isPrototypeOf),
    // JS_CFUNC_DEF!(c"propertyIsEnumerable", 1, js_object_propertyIsEnumerable),
    // JS_CGETSET_DEF!(
    //     c"__proto__",
    //     js_object_get___proto__,
    //     js_object_set___proto__
    // ),

    // JS_CFUNC_MAGIC_DEF!(c"__defineGetter__", 2, js_object___defineGetter__, 0),
    // JS_CFUNC_MAGIC_DEF!(c"__defineSetter__", 2, js_object___defineGetter__, 1),
    // JS_CFUNC_MAGIC_DEF!(c"__lookupGetter__", 1, js_object___lookupGetter__, 0),
    // JS_CFUNC_MAGIC_DEF!(c"__lookupSetter__", 1, js_object___lookupGetter__, 1),
];
