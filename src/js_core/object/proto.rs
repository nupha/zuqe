use crate::{
    array::JS_IsArray,
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_ValueToAtom, JS_ATOM_NULL},
    context::JSContext,
    error::JSResult,
    function::{js_poll_interrupts, CallArgs, JSCFunctionListEntry, JS_Invoke},
    utils::unlikely,
    value::{
        object::{
            js_free_desc, JSObject, JSPropertyDescriptor, JS_GetOwnPropertyInternal,
            JS_GetPrototype, JS_GetPrototypeFree, JS_SetPrototypeInternal, JS_ToObject,
            JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
            JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW,
        },
        JSValue, JSValueConst, JS_ConcatString3, JS_FreeValue, JS_IsException, JS_VALUE_GET_OBJ,
    },
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN,
    JS_CLASS_DATE, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_NUMBER, JS_CLASS_REGEXP,
    JS_CLASS_STRING,
};

//
// Prototype definiation
//

// pub unsafe fn js_object_toString(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     let mut tag: JSValue;

//     if JS_IsNull(this_val) {
//         tag = (*ctx).create_string("Null");
//     } else if JS_IsUndefined(this_val) {
//         tag = (*ctx).create_string("Undefined");
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
//                     | JS_CLASS_REGEXP) => (*ctx).rt().class_array[cid as usize].class_name,
//                     _ => JS_ATOM_Object,
//                 }
//             }
//         } else {
//             JS_FreeValue(ctx, obj);
//             return JSValue::EXCEPTION;
//         };

//         tag = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_toStringTag);
//         JS_FreeValue(ctx, obj);
//         if tag.is_exception() {
//             return JSValue::EXCEPTION;
//         }
//         if !tag.is_string() {
//             JS_FreeValue(ctx, tag);
//             tag = atom.to_value(ctx.as_mut().unwrap(), true);
//         }
//     }

//     JS_ConcatString3(ctx, c"[object ", tag, c"]")
// }

pub fn js_object_toString(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let mut tag_str: JSValue;

    if args.this_value.is_null() {
        tag_str = ctx.create_string("Null");
    } else if args.this_value.is_undefined() {
        tag_str = ctx.create_string("Undefined");
    } else {
        let obj = unsafe { JS_ToObject(ctx, args.this_value) }.into_rc(ctx.rt());
        obj.check_exception()?;

        let atom = if unsafe { JS_IsArray(ctx, *obj)? } {
            JS_ATOM_Array
        } else if obj.is_callable(ctx) {
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
                | JS_CLASS_REGEXP) => ctx.rt().class_array[cid as usize].class_name,
                _ => JS_ATOM_Object,
            }
        };

        tag_str = obj.get_property(ctx, JS_ATOM_Symbol_toStringTag);
        if tag_str.is_exception() {
            return Err(-1);
        }

        if !tag_str.is_string() {
            unsafe {
                JS_FreeValue(ctx, tag_str);
            }
            tag_str = atom.to_value(ctx, true);
        }
    }

    unsafe {
        let rv = JS_ConcatString3(ctx, c"[object ", tag_str, c"]");
        rv.check_exception()
    }
}

// pub unsafe fn js_object_toLocaleString(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     JS_Invoke(
//         ctx.as_mut().unwrap(),
//         this_val,
//         JS_ATOM_toString,
//         0,
//         std::ptr::null_mut(),
//     )
// }

pub fn js_object_toLocaleString(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    unsafe {
        JS_Invoke(
            ctx,
            args.this_value,
            JS_ATOM_toString,
            0,
            std::ptr::null_mut(),
        )
        .check_exception()
    }
}

// pub unsafe fn js_object_valueOf(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     JS_ToObject(ctx, this_val)
// }

pub fn js_object_valueOf(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    unsafe { JS_ToObject(ctx, args.this_value).check_exception() }
}

// pub unsafe fn js_object_hasOwnProperty(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     let atom = JS_ValueToAtom(ctx, *argv.offset(0)); // must be done first
//     if unlikely(atom == JS_ATOM_NULL) {
//         return JSValue::EXCEPTION;
//     }

//     let obj = JS_ToObject(ctx, this_val);
//     if JS_IsException(obj) {
//         JS_FreeAtom(ctx, atom);
//         return obj;
//     }

//     let p = JS_VALUE_GET_OBJ(obj);
//     let ret = JS_GetOwnPropertyInternal(ctx, std::ptr::null_mut(), p, atom);
//     JS_FreeAtom(ctx, atom);
//     JS_FreeValue(ctx, obj);
//     if ret < 0 {
//         JSValue::EXCEPTION
//     } else {
//         JS_NewBool(ret != 0)
//     }
// }

pub fn js_object_hasOwnProperty(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    // must be done first
    let atom = unsafe { JS_ValueToAtom(ctx, args.get_raw(0)) };
    if unlikely(atom == JS_ATOM_NULL) {
        return Err(-1);
    }

    let obj = unsafe { JS_ToObject(ctx, args.this_value).into_rc(ctx.rt()) };
    if obj.is_exception() {
        unsafe {
            JS_FreeAtom(ctx, atom);
        }
        return Err(-1);
    }

    unsafe {
        let p = obj.object_unchecked();
        let ret = JS_GetOwnPropertyInternal(ctx, std::ptr::null_mut(), p, atom);
        JS_FreeAtom(ctx, atom);
        if ret < 0 {
            Err(-1)
        } else {
            Ok(if ret == 0 {
                JSValue::FALSE
            } else {
                JSValue::TRUE
            })
        }
    }
}

// pub unsafe fn js_object_isPrototypeOf(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     let v = *argv.offset(0);
//     if !JS_IsObject(v) {
//         return JS_FALSE;
//     }
//     let obj = JS_ToObject(ctx, this_val);
//     if JS_IsException(obj) {
//         return JSValue::EXCEPTION;
//     }

//     let mut v1 = JS_DupValue(ctx, v);
//     let mut res = false;

//     loop {
//         v1 = JS_GetPrototype(ctx, v1);
//         if JS_IsException(v1) {
//             break;
//         }
//         if JS_IsNull(v1) {
//             res = false;
//             break;
//         }
//         if JS_VALUE_GET_OBJ(obj) == JS_VALUE_GET_OBJ(v1) {
//             res = true;
//             break;
//         }

//         // avoid infinite loop (possible with proxies)
//         if js_poll_interrupts(ctx.as_mut().unwrap()) != 0 {
//             JS_FreeValue(ctx, v1);
//             JS_FreeValue(ctx, obj);
//             return JSValue::EXCEPTION;
//         }
//     }

//     JS_FreeValue(ctx, v1);
//     JS_FreeValue(ctx, obj);

//     JS_NewBool(res)
// }

pub fn js_object_isPrototypeOf(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let mut target = args.get(0);
    if !target.is_object() {
        return Ok(JSValue::FALSE);
    }

    let tmp = unsafe { JS_ToObject(ctx, args.this_value) }
        .check_exception()?
        .into_rc(ctx.rt());

    let ptr = unsafe { tmp.object_unchecked() as *const JSObject };

    let res: bool = loop {
        target = unsafe {
            JS_GetPrototype(ctx, *target)
                .check_exception()?
                .into_rc(ctx.rt())
        };

        if target.is_null() {
            break false;
        }

        unsafe {
            if ptr == target.object_unchecked() as *const JSObject {
                break true;
            }

            // avoid infinite loop (possible with proxies)
            if js_poll_interrupts(ctx) != 0 {
                return Err(-1);
            }
        }
    };

    Ok(if res { JSValue::TRUE } else { JSValue::FALSE })
}

// pub unsafe fn js_object_propertyIsEnumerable(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     let obj = JS_ToObject(ctx, this_val);
//     if JS_IsException(obj) {
//         JS_FreeValue(ctx, obj);
//         return JSValue::EXCEPTION;
//     }

//     let prop = JS_ValueToAtom(ctx, *argv.offset(0));
//     if unlikely(prop == JS_ATOM_NULL) {
//         JS_FreeValue(ctx, obj);
//         return JSValue::EXCEPTION;
//     }

//     let mut desc: JSPropertyDescriptor = std::mem::zeroed();
//     let has_prop = JS_GetOwnPropertyInternal(ctx, &mut desc, JS_VALUE_GET_OBJ(obj), prop);
//     if has_prop < 0 {
//         JS_FreeAtom(ctx, prop);
//         JS_FreeValue(ctx, obj);
//         return JSValue::EXCEPTION;
//     }
//     let res = if has_prop != 0 {
//         let r = JS_NewBool(desc.flags & JS_PROP_ENUMERABLE != 0);
//         js_free_desc(ctx, &mut desc);
//         r
//     } else {
//         JSValue::FALSE
//     };

//     JS_FreeAtom(ctx, prop);
//     JS_FreeValue(ctx, obj);

//     res
// }

pub fn js_object_propertyIsEnumerable(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let obj = unsafe {
        JS_ToObject(ctx, args.this_value)
            .check_exception()?
            .into_rc(ctx.rt())
    };

    let prop = unsafe { JS_ValueToAtom(ctx, args.get_raw(0)) };
    if prop == JS_ATOM_NULL {
        return Err(-1);
    }

    unsafe {
        let mut desc: JSPropertyDescriptor = std::mem::zeroed();
        let has_prop = JS_GetOwnPropertyInternal(ctx, &mut desc, obj.object_unchecked(), prop);
        if has_prop < 0 {
            JS_FreeAtom(ctx, prop);
            return Err(-1);
        }

        let res = if has_prop != 0 {
            let r: JSValue = (desc.flags & JS_PROP_ENUMERABLE != 0).into();
            js_free_desc(ctx, &mut desc);
            r
        } else {
            JSValue::FALSE
        };

        JS_FreeAtom(ctx, prop);
        Ok(res)
    }
}

pub fn js_object_getset___proto__(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    if args.length() == 0 {
        // getter
        let tmp = unsafe { JS_ToObject(ctx, args.this_value).into_rc(ctx.rt()) };
        tmp.check_exception()?;
        unsafe { JS_GetPrototype(ctx, *tmp) }.check_exception()
    } else {
        // setter
        if args.this_value.is_undefined_or_null() {
            ctx.JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }

        let proto = args.get_raw(0);
        if !proto.is_object() && !proto.is_null() {
            // `proto` is neither an object nor null
            Ok(JSValue::UNDEFINED)
        } else {
            // `proto` is an object or null
            unsafe {
                JS_SetPrototypeInternal(ctx, args.this_value, proto, true)?;
            }
            Ok(JSValue::UNDEFINED)
        }
    }
}

// pub unsafe fn js_object_get___proto__(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
//     let val = JS_ToObject(ctx, this_val);
//     if JS_IsException(val) {
//         val
//     } else {
//         let ret = JS_GetPrototype(ctx, val);
//         JS_FreeValue(ctx, val);
//         ret
//     }
// }

// pub unsafe fn js_object_set___proto__(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     proto: JSValueConst,
// ) -> JSValue {
//     if this_val.is_undefined_or_null() {
//         return (*ctx).JS_ThrowTypeErrorNotAnObject();
//     }
//     if !JS_IsObject(proto) && !JS_IsNull(proto) {
//         return JSValue::UNDEFINED;
//     }

//     if JS_SetPrototypeInternal(ctx, this_val, proto, true).is_ok() {
//         JSValue::UNDEFINED
//     } else {
//         JSValue::EXCEPTION
//     }
// }

// magic = 1 if called as __defineSetter__
pub unsafe fn js_object___defineGetter__(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let prop = *argv.offset(0);
    let value = *argv.offset(1);

    let obj = JS_ToObject(ctx, this_val);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    if value.ensure_callable(ctx.as_mut().unwrap()).is_err() {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let atom = JS_ValueToAtom(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        JS_FreeValue(ctx, obj);
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

    let ret = obj.define_property(
        ctx.as_mut().unwrap(),
        atom,
        JSValue::UNDEFINED,
        get,
        set,
        flags,
    );
    JS_FreeValue(ctx, obj);
    JS_FreeAtom(ctx, atom);

    if ret.is_err() {
        JSValue::EXCEPTION
    } else {
        JSValue::UNDEFINED
    }
}

pub unsafe fn js_object___lookupGetter__(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    setter: i32,
) -> JSValue {
    let mut obj = JS_ToObject(ctx, this_val);
    if JS_IsException(obj) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let prop = JS_ValueToAtom(ctx, *argv.offset(0));
    if unlikely(prop == JS_ATOM_NULL) {
        JS_FreeAtom(ctx, prop);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut res: JSValue;

    loop {
        let mut desc: JSPropertyDescriptor = std::mem::zeroed();
        let prop: JSAtom = JS_ATOM_NULL;
        let has_prop = JS_GetOwnPropertyInternal(ctx, &mut desc, JS_VALUE_GET_OBJ(obj), prop);

        if has_prop < 0 {
            JS_FreeAtom(ctx, prop);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if has_prop != 0 {
            if desc.flags & JS_PROP_GETSET != 0 {
                res = if setter != 0 {
                    desc.setter
                } else {
                    desc.getter
                }
                .dup();
            } else {
                res = JSValue::UNDEFINED;
            }
            js_free_desc(ctx, &mut desc);
            break;
        }

        obj = JS_GetPrototypeFree(ctx, obj);
        if obj.is_exception() {
            // obj is Proxy
            JS_FreeAtom(ctx, prop);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        } else if obj.is_null() {
            res = JSValue::UNDEFINED;
            break;
        }

        // avoid infinite loop (possible with proxies)
        if js_poll_interrupts(ctx.as_mut().unwrap()) != 0 {
            JS_FreeAtom(ctx, prop);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    // Cleanup and return result
    JS_FreeAtom(ctx, prop);
    JS_FreeValue(ctx, obj);
    res
}

pub const js_object_proto_funcs: [JSCFunctionListEntry; 7] = [
    JS_CFUNC_NATIVE_DEF!(c"toString", 0, js_object_toString, 0),
    JS_CFUNC_NATIVE_DEF!(c"toLocaleString", 0, js_object_toLocaleString, 0),
    JS_CFUNC_NATIVE_DEF!(c"valueOf", 0, js_object_valueOf, 0),
    JS_CFUNC_NATIVE_DEF!(c"hasOwnProperty", 1, js_object_hasOwnProperty, 0),
    JS_CFUNC_NATIVE_DEF!(c"isPrototypeOf", 1, js_object_isPrototypeOf, 0),
    JS_CFUNC_NATIVE_DEF!(
        c"propertyIsEnumerable",
        1,
        js_object_propertyIsEnumerable,
        0
    ),
    JS_CGETSET_NATIVE_DEF!(
        c"__proto__",
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
