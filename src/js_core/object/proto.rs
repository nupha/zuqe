use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN,
    JS_CLASS_DATE, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_NUMBER, JS_CLASS_REGEXP,
    JS_CLASS_STRING,
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_ValueToAtom, JSAtom, consts::*},
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_Invoke, JSCFunctionListEntry},
    utils::unlikely,
    value::{
        JS_ConcatString3, JSValue, JSValueConst,
        object::{
            JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
            JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW,
            JS_SetPrototypeInternal, JSPropertyDescriptor,
        },
    },
};

//
// Prototype definiation
//

// pub unsafe fn js_object_toString(
//     ctx: &JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     let mut tag: JSValue;

//     if JS_IsNull(this_val) {
//         tag = ctx.create_string("Null");
//     } else if JS_IsUndefined(this_val) {
//         tag = ctx.create_string("Undefined");
//     } else {
//         let obj = JS_ToObject(ctx, this_val);
//         if JS_IsException(obj) {
//             return obj;
//         }

//         let atom = if let Ok(is_array) = JS_IsArray(ctx, obj) {
//             if is_array {
//                 JS_ATOM_Array
//             } else if obj.is_function(ctx.as_ref().unwrap()) {
//                 JS_ATOM_Function
//             } else {
//                 let p = JS_VALUE_GET_OBJ(obj);
//                 match (*p).class_id() {
//                     cid @ (JS_CLASS_STRING
//                     | JS_CLASS_ARGUMENTS
//                     | JS_CLASS_MAPPED_ARGUMENTS
//                     | JS_CLASS_ERROR
//                     | JS_CLASS_BOOLEAN
//                     | JS_CLASS_NUMBER
//                     | JS_CLASS_DATE
//                     | JS_CLASS_REGEXP) => ctx.rt().class_array[cid as usize].class_name,
//                     _ => JS_ATOM_Object,
//                 }
//             }
//         } else {
//             JS_FreeValue(ctx, obj);
//             return JSValue::EXCEPTION;
//         };

//         tag = obj.get_property(ctx, JS_ATOM_Symbol_toStringTag);
//         JS_FreeValue(ctx, obj);
//         if tag.is_exception() {
//             return JSValue::EXCEPTION;
//         }
//         if !tag.is_string() {
//             JS_FreeValue(ctx, tag);
//             tag = atom.to_value(ctx, true);
//         }
//     }

//     JS_ConcatString3(ctx, c"[object ", tag, c"]")
// }

pub fn js_object_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut tag_str: JSValue;

    if cc.this().is_null() {
        tag_str = ctx.create_string("Null");
    } else if cc.this().is_undefined() {
        tag_str = ctx.create_string("Undefined");
    } else {
        let obj = cc.this().coerce_to_object(ctx)?;
        let atom = if obj.is_array(ctx)? {
            JS_ATOM_Array
        } else if obj.is_callable() {
            JS_ATOM_Function
        } else {
            let p = unsafe { obj.object_unchecked() };
            match p.class_id() {
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

        tag_str = obj
            .get_property(ctx, JS_ATOM_Symbol_toStringTag)
            .check_exception()?;

        if !tag_str.is_string() {
            tag_str = atom.to_value(ctx, true);
        }
    }

    unsafe { JS_ConcatString3(ctx, c"[object ", tag_str, c"]").check_exception() }
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
    unsafe { JS_Invoke(ctx, cc.this(), JS_ATOM_toString, &[]).check_exception() }
}

pub fn js_object_valueOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this().coerce_to_object(ctx)
}

pub fn js_object_hasOwnProperty(ctx: &JSContext, cc: CallContext) -> JSResult {
    // must be done first
    let atom = unsafe { JS_ValueToAtom(ctx, cc.arg(0)) };
    if atom == JS_ATOM_NULL {
        return Err(-1);
    }

    let obj = cc.this().coerce_to_object(ctx).inspect_err(|_| {
        JS_FreeAtom(ctx, atom);
    })?;

    let ret = obj.get_own_property(ctx, atom);
    JS_FreeAtom(ctx, atom);
    Ok(JSValue::Bool(ret?.is_some()))
}

pub fn js_object_isPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut target = cc.get_raw(0);
    if !target.is_object() {
        return Ok(JSValue::Bool(false));
    }

    let v = cc.this().coerce_to_object(ctx)?;
    let ptr = unsafe { v.object_unchecked() };

    let res: bool = loop {
        target = target.prototype(ctx)?;
        if target.is_null() {
            break false;
        }
        if ptr == unsafe { target.object_unchecked() } {
            break true;
        }

        // avoid infinite loop (possible with proxies)
        ctx.poll_interrupts()?;
    };
    Ok(JSValue::Bool(res))
}

pub fn js_object_propertyIsEnumerable(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.this().coerce_to_object(ctx)?;
    let prop = JS_ValueToAtom(ctx, cc.arg(0));
    if prop == JS_ATOM_NULL {
        return Err(-1);
    }

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
        cc.this()
            .coerce_to_object(ctx)
            .and_then(|o| o.prototype(ctx))
    } else {
        // setter
        let this_val = cc.this();
        if this_val.is_undefined_or_null() {
            ctx.JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }

        let proto = cc.arg(0);
        if !proto.is_object() && !proto.is_null() {
            // `proto` is neither an object nor null
            Ok(JSValue::UNDEFINED)
        } else {
            // `proto` is an object or null
            unsafe {
                JS_SetPrototypeInternal(ctx, &this_val, proto.clone(), true)?;
            }
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
    let prop = &*argv.offset(0);
    let value = &*argv.offset(1);

    let obj = this_val.coerce_to_object(ctx).unwrap_or(JSValue::EXCEPTION);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    if value.ensure_callable(ctx).is_err() {
        return JSValue::EXCEPTION;
    }

    let atom = JS_ValueToAtom(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        return JSValue::EXCEPTION;
    }

    let mut flags = JS_PROP_THROW
        | JS_PROP_HAS_ENUMERABLE
        | JS_PROP_ENUMERABLE
        | JS_PROP_HAS_CONFIGURABLE
        | JS_PROP_CONFIGURABLE;

    let (get, set) = if magic != 0 {
        flags |= JS_PROP_HAS_SET;
        (&JSValue::UNDEFINED, value)
    } else {
        flags |= JS_PROP_HAS_GET;
        (value, &JSValue::UNDEFINED)
    };

    let ret = obj.define_property(ctx, atom, &JSValue::UNDEFINED, get, set, flags);
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
    let mut obj = this_val.coerce_to_object(ctx).unwrap_or(JSValue::EXCEPTION);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let prop = JS_ValueToAtom(ctx, &*argv.offset(0));
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

pub const js_object_proto_funcs: [JSCFunctionListEntry; 7] = [
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
