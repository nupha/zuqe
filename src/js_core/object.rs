use crate::{
    array::{js_array_push, JS_IsArray, JS_NewArray},
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_ValueToAtom, JS_ATOM_NULL},
    class::js_create_from_ctor,
    context::{JSContext, JS_GetActiveFunction},
    function::{check_function, js_poll_interrupts, JSCFunctionListEntry, JS_Call, JS_Invoke},
    iterator::{JSIteratorKindEnum, JS_GetIterator, JS_IteratorClose, JS_IteratorNext},
    types::unlikely,
    value::{
        object::{
            js_free_desc, JSPropertyDescriptor, JS_GetOwnPropertyInternal, JS_GetPrototype,
            JS_GetPrototypeFree, JS_IsExtensible, JS_NewObject, JS_NewObjectProto,
            JS_PreventExtensions, JS_SetPrototypeInternal, JS_ToObject, JS_GPN_ENUM_ONLY,
            JS_GPN_STRING_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E,
            JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE,
            JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW, JS_PROP_WRITABLE,
        },
        JSTag, JSValue, JSValueConst, JS_ConcatString3, JS_DupValue, JS_FreeValue, JS_IsException,
        JS_IsNull, JS_IsObject, JS_IsUndefined, JS_NewBool, JS_FALSE, JS_TAG_NULL, JS_TAG_OBJECT,
        JS_TAG_UNDEFINED, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
        MAX_SAFE_INTEGER,
    },
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CGETSET_DEF, JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN,
    JS_CLASS_DATE, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_NUMBER, JS_CLASS_OBJECT,
    JS_CLASS_REGEXP, JS_CLASS_STRING,
};

use super::mapset::{js_map_constructor, js_map_get, js_map_set};

pub unsafe fn JS_ObjectDefineProperties(
    ctx: *mut JSContext,
    obj: JSValueConst,
    properties: JSValueConst,
) -> Result<(), i32> {
    if !obj.is_object() {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let props = JS_ToObject(ctx, properties);
    if props.is_exception() {
        return Err(-1);
    }

    let p = JS_VALUE_GET_OBJ(props);

    let r = (*p).own_property_names_foreach(
        ctx.as_mut().unwrap(),
        JS_GPN_ENUM_ONLY | JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
        |cx, name| {
            let desc = props.get_property(cx, name.atom);
            if desc.is_exception() {
                Err(-1)
            } else {
                let r = obj
                    .define_property_desc(cx, name.atom, desc, JS_PROP_THROW)
                    .map(|_| ());
                JS_FreeValue(ctx, desc);
                r
            }
        },
    );

    JS_FreeValue(ctx, props);
    r

    // let mut atoms: *mut PropertyEnum = std::ptr::null_mut();
    // let mut len: u32 = 0;
    // let mut desc = JSValue::UNDEFINED;

    // macro_rules! exit {
    //     ( $ret:expr) => {
    //         //js_free_prop_enum(ctx, atoms, len as usize);
    //         for item in Box::from_raw(
    //             std::slice::from_raw_parts_mut(atoms, len as usize) as *mut [PropertyEnum]
    //         ) {
    //             JS_FreeAtom(ctx, item.atom);
    //         }
    //         JS_FreeValue(ctx, props);
    //         JS_FreeValue(ctx, desc);
    //         return $ret;
    //     };
    // }

    // if JS_GetOwnPropertyNamesInternal(
    //     ctx,
    //     &mut atoms,
    //     &mut len,
    //     p,
    //     JS_GPN_ENUM_ONLY | JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
    // )
    // .is_err()
    // {
    //     exit!(Err(-1));
    // }

    // for i in 0..len {
    //     JS_FreeValue(ctx, desc);

    //     desc = JS_GetProperty(ctx, props, (*atoms.offset(i as isize)).atom);
    //     if desc.is_exception() {
    //         exit!(Err(-1));
    //     }

    //     if JS_DefinePropertyDesc(
    //         ctx,
    //         obj,
    //         (*atoms.offset(i as isize)).atom,
    //         desc,
    //         JS_PROP_THROW,
    //     )
    //     .is_err()
    //     {
    //         exit!(Err(-1));
    //     }
    // }

    // exit!(Ok(()));
}

pub unsafe fn js_object_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    //debug_assert!(argc > 0, "at least one slot for argument");
    debug_assert!(!argv.is_null());

    if !JS_IsUndefined(new_target)
        && JS_VALUE_GET_OBJ(new_target) != JS_VALUE_GET_OBJ(JS_GetActiveFunction(ctx))
    {
        js_create_from_ctor(ctx, new_target, JS_CLASS_OBJECT)
    } else {
        let a = *argv.offset(0);
        match JS_VALUE_GET_NORM_TAG(a) {
            JSTag::Null | JSTag::Undefined => JS_NewObject(ctx),
            _ => JS_ToObject(ctx, a),
        }
    }
}

pub unsafe fn js_object_create(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let proto = *argv.offset(0);
    if !JS_IsObject(proto) && !JS_IsNull(proto) {
        return ctx.as_mut().unwrap().JS_ThrowTypeError(c"not a prototype");
    }
    let obj = JS_NewObjectProto(ctx, proto);
    if JS_IsException(obj) {
        return JSValue::EXCEPTION;
    }
    let props = *argv.offset(1);
    if !JS_IsUndefined(props) {
        if JS_ObjectDefineProperties(ctx, obj, props).is_err() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    obj
}

pub unsafe fn js_object_getPrototypeOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let val = *argv.offset(0);
    if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
        // ES6 feature non compatible with ES5.1: primitive types are accepted
        if magic != 0
            || JS_VALUE_GET_TAG(val) == JS_TAG_NULL
            || JS_VALUE_GET_TAG(val) == JS_TAG_UNDEFINED
        {
            return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        }
    }
    JS_GetPrototype(ctx, val)
}

pub unsafe fn js_object_setPrototypeOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = *argv.offset(0);
    if JS_SetPrototypeInternal(ctx, obj, *argv.offset(1), true).is_err() {
        JSValue::EXCEPTION
    } else {
        obj.dup()
    }
}

// magic = 1 if called as Reflect.defineProperty
pub unsafe fn js_object_defineProperty(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let obj = *argv.offset(0);
    let prop = *argv.offset(1);
    let desc = *argv.offset(2);

    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
    }

    let atom = JS_ValueToAtom(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        return JSValue::EXCEPTION;
    }

    let mut flags = 0;
    if magic == 0 {
        flags |= JS_PROP_THROW;
    }
    let ret = obj.define_property_desc(ctx.as_mut().unwrap(), atom, desc, flags);
    JS_FreeAtom(ctx, atom);
    match ret {
        Ok(b) => {
            if magic != 0 {
                if b {
                    JSValue::TRUE
                } else {
                    JSValue::FALSE
                }
            } else {
                obj.dup()
            }
        }
        Err(_) => JSValue::EXCEPTION,
    }
}

pub unsafe fn js_object_defineProperties(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    // defineProperties(obj, properties)
    let obj: JSValueConst = *argv.offset(0);
    if JS_ObjectDefineProperties(ctx, obj, *argv.offset(1)).is_err() {
        JSValue::EXCEPTION
    } else {
        JS_DupValue(ctx, obj)
    }
}

pub unsafe fn js_object_getOwnPropertyNames(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    (*argv.offset(0)).get_own_property_names2(
        ctx.as_mut().unwrap(),
        JS_GPN_STRING_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub unsafe fn js_object_getOwnPropertySymbols(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    (*argv.offset(0)).get_own_property_names2(
        ctx.as_mut().unwrap(),
        JS_GPN_SYMBOL_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub unsafe fn js_object_keys(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    kind: i32,
) -> JSValue {
    debug_assert!((JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i32
        ..=JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i32)
        .contains(&kind));

    (*argv.offset(0)).get_own_property_names2(
        ctx.as_mut().unwrap(),
        JS_GPN_ENUM_ONLY | JS_GPN_STRING_MASK,
        std::mem::transmute::<u8, JSIteratorKindEnum>(kind as u8),
    )
}

pub unsafe fn js_object_groupBy(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    is_map: i32,
) -> JSValue {
    // "is function?" check must be observed before argv[0] is accessed
    let cb = *argv.offset(1);
    if check_function(ctx, cb).is_err() {
        return JSValue::EXCEPTION;
    }

    let iter = JS_GetIterator(ctx, *argv.offset(0), false);
    if iter.is_exception() {
        return JSValue::EXCEPTION;
    }

    let next = iter.get_property(ctx.as_mut().unwrap(), JS_ATOM_next);
    if next.is_exception() {
        JS_FreeValue(ctx, iter);
        return JSValue::EXCEPTION;
    }

    let groups = if is_map != 0 {
        js_map_constructor(ctx, JSValue::UNDEFINED, 0, std::ptr::null_mut(), 0)
    } else {
        JS_NewObjectProto(ctx, JSValue::NULL)
    };

    if groups.is_exception() {
        JS_FreeValue(ctx, iter);
        JS_FreeValue(ctx, next);
        return JSValue::EXCEPTION;
    }

    let mut idx: i64 = 0;
    loop {
        if idx >= MAX_SAFE_INTEGER {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"too many elements");
            // C2RS: goto iterator_close_exception replaced with cleanup and return
            JS_IteratorClose(ctx, iter, true);
            JS_FreeValue(ctx, next);
            JS_FreeValue(ctx, groups);
            return JSValue::EXCEPTION;
        }

        let mut done = false;
        let v = JS_IteratorNext(ctx, iter, next, 0, std::ptr::null_mut(), &mut done);
        if JS_IsException(v) {
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, next);
            JS_FreeValue(ctx, groups);
            return JSValue::EXCEPTION;
        }

        if done {
            break; // v is JSValue::UNDEFINED
        }

        let key = JS_Call(
            ctx.as_mut().unwrap(),
            cb,
            (*ctx).global_obj,
            2,
            [v, JSValue::from_i64(idx)].as_mut_ptr(),
        );
        if JS_IsException(key) {
            // C2RS: goto iterator_close_exception replaced with cleanup and return
            JS_IteratorClose(ctx, iter, true);
            JS_FreeValue(ctx, next);
            JS_FreeValue(ctx, groups);
            return JSValue::EXCEPTION;
        }

        // get prop value and key atom
        let (mut prop, key_atom): (JSValue, JSAtom) = if is_map != 0 {
            (
                js_map_get(ctx, groups, 1, [key].as_mut_ptr(), 0),
                JS_ATOM_NULL,
            )
        } else {
            let atom = JS_ValueToAtom(ctx, key);
            JS_FreeValue(ctx, key);
            if atom == JS_ATOM_NULL {
                JS_IteratorClose(ctx, iter, true);
                JS_FreeValue(ctx, next);
                JS_FreeValue(ctx, groups);
                return JSValue::EXCEPTION;
            }
            (groups.get_property(ctx.as_mut().unwrap(), atom), atom)
        };

        if prop.is_exception() {
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, next);
            JS_FreeValue(ctx, groups);
            return JSValue::EXCEPTION;
        }

        if prop.is_undefined() {
            // entry is missing, create the entry
            prop = JS_NewArray(ctx);
            if prop.is_exception() {
                JS_FreeValue(ctx, iter);
                JS_FreeValue(ctx, next);
                JS_FreeValue(ctx, groups);
                return JSValue::EXCEPTION;
            }
            if is_map != 0 {
                let res = js_map_set(ctx, groups, 2, [key, prop].as_ptr(), 0);
                if res.is_exception() {
                    JS_FreeValue(ctx, iter);
                    JS_FreeValue(ctx, next);
                    JS_FreeValue(ctx, groups);
                    return JSValue::EXCEPTION;
                } else {
                    JS_FreeValue(ctx, res);
                }
            } else {
                if groups
                    .define_property_value(
                        ctx.as_mut().unwrap(),
                        key_atom,
                        prop.dup(),
                        JS_PROP_C_W_E,
                    )
                    .is_err()
                {
                    JS_FreeValue(ctx, iter);
                    JS_FreeValue(ctx, next);
                    JS_FreeValue(ctx, groups);
                    return JSValue::EXCEPTION;
                }
            }
        }

        let res = js_array_push(ctx, prop, 1, &v, 0);
        if res.is_exception() {
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, next);
            JS_FreeValue(ctx, groups);
            return JSValue::EXCEPTION;
        }
        // res is an int64

        JS_FreeValue(ctx, prop);
        JS_FreeValue(ctx, key);
        JS_FreeAtom(ctx, key_atom);
        JS_FreeValue(ctx, v);

        idx += 1;
    } // loop

    JS_FreeValue(ctx, iter);
    JS_FreeValue(ctx, next);

    groups
}

pub unsafe fn js_object_isExtensible(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    reflect: i32,
) -> JSValue {
    let obj: JSValueConst = *argv.offset(0);

    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        return if reflect != 0 {
            (*ctx).JS_ThrowTypeErrorNotAnObject()
        } else {
            JS_FALSE
        };
    }

    if let Ok(ret) = JS_IsExtensible(ctx, obj) {
        JS_NewBool(ret)
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_object_preventExtensions(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    reflect: i32,
) -> JSValue {
    let obj: JSValueConst = *argv.offset(0);

    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        if reflect != 0 {
            return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        } else {
            return JS_DupValue(ctx, obj);
        }
    }

    if let Ok(ret) = JS_PreventExtensions(ctx, obj) {
        if reflect != 0 {
            JS_NewBool(ret)
        } else {
            if !ret {
                ctx.as_mut()
                    .unwrap()
                    .JS_ThrowTypeError(c"proxy preventExtensions handler returned false")
            } else {
                JS_DupValue(ctx, obj)
            }
        }
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_object_getOwnPropertyDescriptor(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let obj = if magic != 0 {
        // Reflect.getOwnPropertyDescriptor case
        if JS_VALUE_GET_TAG(*argv.offset(0)) != JS_TAG_OBJECT {
            return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        }
        JS_DupValue(ctx, *argv.offset(0))
    } else {
        let o = JS_ToObject(ctx, *argv.offset(0));
        if JS_IsException(o) {
            return o;
        }
        o
    };

    let prop = *argv.offset(1);
    let atom = JS_ValueToAtom(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        // C2RS: goto exception replaced with early return logic
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut ret = JSValue::UNDEFINED;

    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let mut desc: JSPropertyDescriptor = std::mem::zeroed();
        let res = JS_GetOwnPropertyInternal(ctx, &mut desc, JS_VALUE_GET_OBJ(obj), atom);
        if res < 0 {
            // C2RS: goto exception replaced with early return logic
            JS_FreeAtom(ctx, atom);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if res != 0 {
            ret = JS_NewObject(ctx);
            if JS_IsException(ret) {
                // C2RS: goto exception1 replaced with early return logic
                js_free_desc(ctx, &mut desc);
                JS_FreeAtom(ctx, atom);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            let flags = JS_PROP_C_W_E | JS_PROP_THROW;
            if desc.flags & JS_PROP_GETSET != 0 {
                if ret
                    .define_property_value(
                        ctx.as_mut().unwrap(),
                        JS_ATOM_get,
                        desc.getter.dup(),
                        flags,
                    )
                    .is_err()
                    || ret
                        .define_property_value(
                            ctx.as_mut().unwrap(),
                            JS_ATOM_set,
                            desc.setter.dup(),
                            flags,
                        )
                        .is_err()
                {
                    // C2RS: goto exception1 replaced with early return logic
                    js_free_desc(ctx, &mut desc);
                    JS_FreeValue(ctx, ret);
                    JS_FreeAtom(ctx, atom);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            } else {
                if ret
                    .define_property_value(
                        ctx.as_mut().unwrap(),
                        JS_ATOM_value,
                        desc.value.dup(),
                        flags,
                    )
                    .is_err()
                    || ret
                        .define_property_value(
                            ctx.as_mut().unwrap(),
                            JS_ATOM_writable,
                            JS_NewBool((desc.flags & JS_PROP_WRITABLE) != 0),
                            flags,
                        )
                        .is_err()
                {
                    // C2RS: goto exception1 replaced with early return logic
                    js_free_desc(ctx, &mut desc);
                    JS_FreeValue(ctx, ret);
                    JS_FreeAtom(ctx, atom);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
            if ret
                .define_property_value(
                    ctx.as_mut().unwrap(),
                    JS_ATOM_enumerable,
                    JS_NewBool((desc.flags & JS_PROP_ENUMERABLE) != 0),
                    flags,
                )
                .is_err()
                || ret
                    .define_property_value(
                        ctx.as_mut().unwrap(),
                        JS_ATOM_configurable,
                        JS_NewBool((desc.flags & JS_PROP_CONFIGURABLE) != 0),
                        flags,
                    )
                    .is_err()
            {
                js_free_desc(ctx, &mut desc);
                JS_FreeValue(ctx, ret);
                JS_FreeAtom(ctx, atom);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            js_free_desc(ctx, &mut desc);
        }
    }

    JS_FreeAtom(ctx, atom);
    JS_FreeValue(ctx, obj);
    ret
}

pub unsafe fn js_object_getOwnPropertyDescriptors(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, *argv.offset(0));
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let res_obj = JS_NewObject(ctx);
    if res_obj.is_exception() {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_OBJ(obj);

    let r = (*p).own_property_names_foreach(
        ctx.as_mut().unwrap(),
        JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
        |cx, name| {
            let atom_value = name.atom.to_value(ctx.as_mut().unwrap(), false);
            if atom_value.is_exception() {
                return Err(-1);
            }

            let desc = js_object_getOwnPropertyDescriptor(
                ctx,
                JSValue::UNDEFINED,
                2,
                [obj, atom_value].as_mut_ptr(),
                0,
            );
            JS_FreeValue(ctx, atom_value);

            if desc.is_exception() {
                Err(-1)
            } else if !desc.is_undefined() {
                res_obj
                    .define_property_value(cx, name.atom, desc, JS_PROP_C_W_E | JS_PROP_THROW)
                    .map(|_| ())
            } else {
                Ok(())
            }
        },
    );

    if r.is_err() {
        JS_FreeValue(ctx, res_obj);
        JS_FreeValue(ctx, obj);
        JSValue::EXCEPTION
    } else {
        res_obj
    }

    // let mut props = std::ptr::null_mut::<PropertyEnum>();
    // let mut len: u32 = 0;

    // if JS_GetOwnPropertyNamesInternal(
    //     ctx,
    //     &mut props,
    //     &mut len,
    //     p,
    //     JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
    // )
    // .is_err()
    // {
    //     // C2RS: goto exception replaced with early return logic
    //     JS_FreeValue(ctx, obj);
    //     return JSValue::EXCEPTION;
    // }

    // r = JS_NewObject(ctx);
    // if r.is_exception() {
    //     //js_free_prop_enum(ctx, props, len as _);
    //     for item in Box::from_raw(
    //         std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum]
    //     ) {
    //         JS_FreeAtom(ctx, item.atom);
    //     }

    //     JS_FreeValue(ctx, obj);
    //     return JSValue::EXCEPTION;
    // }

    // for i in 0..len {
    //     let atomValue = (*props.offset(i as isize))
    //         .atom
    //         .to_value(ctx.as_mut().unwrap(), false);

    //     if atomValue.is_exception() {
    //         // js_free_prop_enum(ctx, props, len as _);
    //         for item in Box::from_raw(
    //             std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum]
    //         ) {
    //             JS_FreeAtom(ctx, item.atom);
    //         }

    //         JS_FreeValue(ctx, obj);
    //         JS_FreeValue(ctx, r);
    //         return JSValue::EXCEPTION;
    //     }

    //     let mut args = [obj, atomValue];
    //     let desc =
    //         js_object_getOwnPropertyDescriptor(ctx, JSValue::UNDEFINED, 2, args.as_mut_ptr(), 0);
    //     JS_FreeValue(ctx, atomValue);
    //     if desc.is_exception() {
    //         //js_free_prop_enum(ctx, props, len as _);
    //         for item in Box::from_raw(
    //             std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum]
    //         ) {
    //             JS_FreeAtom(ctx, item.atom);
    //         }

    //         JS_FreeValue(ctx, obj);
    //         JS_FreeValue(ctx, r);
    //         return JSValue::EXCEPTION;
    //     }

    //     if !desc.is_undefined() {
    //         if r.define_property_value(
    //             ctx.as_mut().unwrap(),
    //             (*props.offset(i as isize)).atom,
    //             desc,
    //             JS_PROP_C_W_E | JS_PROP_THROW,
    //         )
    //         .is_err()
    //         {
    //             //js_free_prop_enum(ctx, props, len as _);
    //             for item in
    //                 Box::from_raw(std::slice::from_raw_parts_mut(props, len as usize)
    //                     as *mut [PropertyEnum])
    //             {
    //                 JS_FreeAtom(ctx, item.atom);
    //             }

    //             JS_FreeValue(ctx, obj);
    //             JS_FreeValue(ctx, r);
    //             return JSValue::EXCEPTION;
    //         }
    //     }
    // }

    // //js_free_prop_enum(ctx, props, len as _);
    // for item in
    //     Box::from_raw(std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum])
    // {
    //     JS_FreeAtom(ctx, item.atom);
    // }

    // JS_FreeValue(ctx, obj);

    // r
}

pub unsafe fn js_object_is(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    JS_NewBool((*argv.add(0)).is_same(*argv.add(1)))
}

/// Object.assign(obj, source1)
pub unsafe fn js_object_assign(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, *argv.offset(0));
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    for i in 1..argc {
        let a = *argv.add(i as usize);

        if !a.is_undefined_or_null() {
            let s = JS_ToObject(ctx, a);
            if s.is_exception() {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            if obj
                .copy_properties_from(ctx.as_mut().unwrap(), s, JSValue::UNDEFINED, true)
                .is_err()
            {
                JS_FreeValue(ctx, obj);
                JS_FreeValue(ctx, s);
                return JSValue::EXCEPTION;
            }

            JS_FreeValue(ctx, s);
        }
    }

    obj
}

pub unsafe fn js_object_seal(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    freeze_flag: i32,
) -> JSValue {
    (*argv.offset(0)).seal(ctx.as_mut().unwrap(), freeze_flag != 0)

    // let obj = *argv.offset(0);
    // if !JS_IsObject(obj) {
    //     return JS_DupValue(ctx, obj);
    // }

    // match JS_PreventExtensions(ctx, obj) {
    //     Err(_) => {
    //         return JSValue::EXCEPTION;
    //     }
    //     Ok(false) => {
    //         return ctx
    //             .as_mut()
    //             .unwrap()
    //             .JS_ThrowTypeError(c"proxy preventExtensions handler returned false");
    //     }
    //     Ok(true) => {}
    // }

    // let p = JS_VALUE_GET_OBJ(obj);

    // let mut props = std::ptr::null_mut::<PropertyEnum>();
    // let mut len: u32 = 0;
    // if JS_GetOwnPropertyNamesInternal(
    //     ctx,
    //     &mut props,
    //     &mut len,
    //     p,
    //     JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
    // )
    // .is_err()
    // {
    //     return JSValue::EXCEPTION;
    // }

    // for i in 0..len {
    //     let prop = (*props.offset(i as isize)).atom;
    //     let mut desc_flags = JS_PROP_THROW | JS_PROP_HAS_CONFIGURABLE;

    //     if freeze_flag != 0 {
    //         let mut desc: JSPropertyDescriptor = std::mem::zeroed();
    //         let res = JS_GetOwnPropertyInternal(ctx, &mut desc, p, prop);
    //         if res < 0 {
    //             // js_free_prop_enum(ctx, props, len as _);
    //             for item in
    //                 Box::from_raw(std::slice::from_raw_parts_mut(props, len as usize)
    //                     as *mut [PropertyEnum])
    //             {
    //                 JS_FreeAtom(ctx, item.atom);
    //             }
    //             return JSValue::EXCEPTION;
    //         }
    //         if res != 0 {
    //             if desc.flags & JS_PROP_WRITABLE != 0 {
    //                 desc_flags |= JS_PROP_HAS_WRITABLE;
    //             }
    //             js_free_desc(ctx, &mut desc);
    //         }
    //     }

    //     if obj
    //         .define_property(
    //             ctx.as_mut().unwrap(),
    //             prop,
    //             JSValue::UNDEFINED,
    //             JSValue::UNDEFINED,
    //             JSValue::UNDEFINED,
    //             desc_flags,
    //         )
    //         .is_err()
    //     {
    //         //js_free_prop_enum(ctx, props, len as _);
    //         for item in Box::from_raw(
    //             std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum]
    //         ) {
    //             JS_FreeAtom(ctx, item.atom);
    //         }
    //         return JSValue::EXCEPTION;
    //     }
    // }

    // //js_free_prop_enum(ctx, props, len as _);
    // for item in
    //     Box::from_raw(std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum])
    // {
    //     JS_FreeAtom(ctx, item.atom);
    // }

    // obj.dup()
}

pub unsafe fn js_object_isSealed(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    is_frozen: i32,
) -> JSValue {
    match (*argv.offset(0)).is_sealed(ctx.as_mut().unwrap(), is_frozen != 0) {
        Ok(true) => JSValue::TRUE,
        Ok(false) => JSValue::FALSE,
        Err(_) => JSValue::EXCEPTION,
    }

    // if !JS_IsObject(obj) {
    //     return JS_TRUE;
    // }

    // let p = JS_VALUE_GET_OBJ(obj);

    // let mut props: *mut PropertyEnum = std::ptr::null_mut();
    // let mut len: u32 = 0;
    // if JS_GetOwnPropertyNamesInternal(
    //     ctx,
    //     &mut props,
    //     &mut len,
    //     p,
    //     JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
    // )
    // .is_err()
    // {
    //     return JSValue::EXCEPTION;
    // }

    // for i in 0..len {
    //     let prop = (*props.offset(i as isize)).atom;

    //     let mut desc: JSPropertyDescriptor = std::mem::zeroed();
    //     let res = JS_GetOwnPropertyInternal(ctx, &mut desc, p, prop);
    //     if res < 0 {
    //         // js_free_prop_enum(ctx, props, len as _);
    //         for item in Box::from_raw(
    //             std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum]
    //         ) {
    //             JS_FreeAtom(ctx, item.atom);
    //         }
    //         return JSValue::EXCEPTION;
    //     }
    //     if res != 0 {
    //         js_free_desc(ctx, &mut desc);
    //         if (desc.flags & JS_PROP_CONFIGURABLE != 0)
    //             || (is_frozen != 0 && (desc.flags & JS_PROP_WRITABLE != 0))
    //         {
    //             //js_free_prop_enum(ctx, props, len as _);
    //             for item in
    //                 Box::from_raw(std::slice::from_raw_parts_mut(props, len as usize)
    //                     as *mut [PropertyEnum])
    //             {
    //                 JS_FreeAtom(ctx, item.atom);
    //             }

    //             return JSValue::FALSE;
    //         }
    //     }
    // }

    // match JS_IsExtensible(ctx, obj) {
    //     Ok(b) => {
    //         //js_free_prop_enum(ctx, props, len as _);
    //         for item in Box::from_raw(
    //             std::slice::from_raw_parts_mut(props, len as usize) as *mut [PropertyEnum]
    //         ) {
    //             JS_FreeAtom(ctx, item.atom);
    //         }

    //         // returns value of !b
    //         if b {
    //             JSValue::FALSE
    //         } else {
    //             JSValue::TRUE
    //         }
    //     }
    //     Err(_) => {
    //         // should do js_free_prop_enum(ctx, props, len as _)?
    //         JSValue::EXCEPTION
    //     }
    // }
}

pub unsafe fn js_object_fromEntries(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_NewObject(ctx);
    if JS_IsException(obj) {
        return obj;
    }

    // RequireObjectCoercible() not necessary because it is tested in
    // JS_GetIterator() by JS_GetProperty()
    let iterable = *argv.offset(0);

    let iter = JS_GetIterator(ctx, iterable, false);
    if JS_IsException(iter) {
        // goto fail
        if JS_IsObject(iter) {
            // close the iterator object, preserving pending exception
            JS_IteratorClose(ctx, iter, true);
        }
        JS_FreeValue(ctx, iter);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let next_method = iter.get_property(ctx.as_mut().unwrap(), JS_ATOM_next);
    if next_method.is_exception() {
        if iter.is_object() {
            // close the iterator object, preserving pending exception
            JS_IteratorClose(ctx, iter, true);
        }
        JS_FreeValue(ctx, next_method);
        JS_FreeValue(ctx, iter);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    loop {
        let mut done = false;
        let item = JS_IteratorNext(ctx, iter, next_method, 0, std::ptr::null_mut(), &mut done);
        if item.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, iter, true);
            }
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if done {
            JS_FreeValue(ctx, item);
            break;
        }

        if !item.is_object() {
            ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
            JS_FreeValue(ctx, item);
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, iter, true);
            }
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let key = item.get_property_u32(ctx.as_mut().unwrap(), 0);
        if key.is_exception() {
            JS_FreeValue(ctx, item);
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, iter, true);
            }
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let value = item.get_property_u32(ctx.as_mut().unwrap(), 1);
        if value.is_exception() {
            JS_FreeValue(ctx, key);
            JS_FreeValue(ctx, item);
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, iter, true);
            }
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if obj
            .define_property_value_value(
                ctx.as_mut().unwrap(),
                key,
                value,
                JS_PROP_C_W_E | JS_PROP_THROW,
            )
            .is_err()
        {
            JS_FreeValue(ctx, item);
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, iter, true).expect("TODO");
            }
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        JS_FreeValue(ctx, item);
    }

    JS_FreeValue(ctx, next_method);
    JS_FreeValue(ctx, iter);

    obj
}

pub unsafe fn js_object_hasOwn(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, *argv.offset(0));
    if JS_IsException(obj) {
        return obj;
    }

    let atom = JS_ValueToAtom(ctx, *argv.offset(1));
    if unlikely(atom == JS_ATOM_NULL) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_OBJ(obj);
    let ret = JS_GetOwnPropertyInternal(ctx, std::ptr::null_mut(), p, atom);
    JS_FreeAtom(ctx, atom);
    JS_FreeValue(ctx, obj);
    if ret < 0 {
        JSValue::EXCEPTION
    } else {
        JS_NewBool(ret != 0)
    }
}

pub const js_object_funcs: [JSCFunctionListEntry; 23] = [
    JS_CFUNC_DEF!(c"create", 2, js_object_create),
    JS_CFUNC_MAGIC_DEF!(c"getPrototypeOf", 1, js_object_getPrototypeOf, 0),
    JS_CFUNC_DEF!(c"setPrototypeOf", 2, js_object_setPrototypeOf),
    JS_CFUNC_MAGIC_DEF!(c"defineProperty", 3, js_object_defineProperty, 0),
    JS_CFUNC_DEF!(c"defineProperties", 2, js_object_defineProperties),
    JS_CFUNC_DEF!(c"getOwnPropertyNames", 1, js_object_getOwnPropertyNames),
    JS_CFUNC_DEF!(c"getOwnPropertySymbols", 1, js_object_getOwnPropertySymbols),
    JS_CFUNC_MAGIC_DEF!(c"groupBy", 2, js_object_groupBy, 0),
    JS_CFUNC_MAGIC_DEF!(
        c"keys",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"values",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"entries",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16
    ),
    JS_CFUNC_MAGIC_DEF!(c"isExtensible", 1, js_object_isExtensible, 0),
    JS_CFUNC_MAGIC_DEF!(c"preventExtensions", 1, js_object_preventExtensions, 0),
    JS_CFUNC_MAGIC_DEF!(
        c"getOwnPropertyDescriptor",
        2,
        js_object_getOwnPropertyDescriptor,
        0
    ),
    JS_CFUNC_DEF!(
        c"getOwnPropertyDescriptors",
        1,
        js_object_getOwnPropertyDescriptors
    ),
    JS_CFUNC_DEF!(c"is", 2, js_object_is),
    JS_CFUNC_DEF!(c"assign", 2, js_object_assign),
    JS_CFUNC_MAGIC_DEF!(c"seal", 1, js_object_seal, 0),
    JS_CFUNC_MAGIC_DEF!(c"freeze", 1, js_object_seal, 1),
    JS_CFUNC_MAGIC_DEF!(c"isSealed", 1, js_object_isSealed, 0),
    JS_CFUNC_MAGIC_DEF!(c"isFrozen", 1, js_object_isSealed, 1),
    //JS_CFUNC_DEF!(c"__getClass", 1, js_object___getClass),
    //JS_CFUNC_DEF(c"__isObject", 1, js_object___isObject ),
    //JS_CFUNC_DEF(c"__isConstructor", 1, js_object___isConstructor ),
    //JS_CFUNC_DEF(c"__toObject", 1, js_object___toObject ),
    //JS_CFUNC_DEF(c"__setOwnProperty", 3, js_object___setOwnProperty ),
    //JS_CFUNC_DEF(c"__toPrimitive", 2, js_object___toPrimitive ),
    //JS_CFUNC_DEF(c"__toPropertyKey", 1, js_object___toPropertyKey ),
    //JS_CFUNC_DEF(c"__speciesConstructor", 2, js_object___speciesConstructor ),
    //JS_CFUNC_DEF(c"__isSameValueZero", 2, js_object___isSameValueZero ),
    //JS_CFUNC_DEF(c"__getObjectData", 1, js_object___getObjectData ),
    //JS_CFUNC_DEF(c"__setObjectData", 2, js_object___setObjectData ),
    JS_CFUNC_DEF!(c"fromEntries", 1, js_object_fromEntries),
    JS_CFUNC_DEF!(c"hasOwn", 2, js_object_hasOwn),
];

//
// Prototype definiation
//

pub unsafe fn js_object_toString(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let mut tag: JSValue;

    if JS_IsNull(this_val) {
        tag = (*ctx).create_string("Null");
    } else if JS_IsUndefined(this_val) {
        tag = (*ctx).create_string("Undefined");
    } else {
        let obj = JS_ToObject(ctx, this_val);
        if JS_IsException(obj) {
            return obj;
        }

        let atom = if let Ok(is_array) = JS_IsArray(ctx, obj) {
            if is_array {
                JS_ATOM_Array
            } else if obj.is_function(ctx.as_ref().unwrap()) {
                JS_ATOM_Function
            } else {
                let p = JS_VALUE_GET_OBJ(obj);
                match (*p).class_id() {
                    cid @ (JS_CLASS_STRING
                    | JS_CLASS_ARGUMENTS
                    | JS_CLASS_MAPPED_ARGUMENTS
                    | JS_CLASS_ERROR
                    | JS_CLASS_BOOLEAN
                    | JS_CLASS_NUMBER
                    | JS_CLASS_DATE
                    | JS_CLASS_REGEXP) => (*ctx).rt().class_array[cid as usize].class_name,
                    _ => JS_ATOM_Object,
                }
            }
        } else {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };

        tag = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_toStringTag);
        JS_FreeValue(ctx, obj);
        if tag.is_exception() {
            return JSValue::EXCEPTION;
        }
        if !tag.is_string() {
            JS_FreeValue(ctx, tag);
            tag = atom.to_value(ctx.as_mut().unwrap(), true);
        }
    }

    JS_ConcatString3(ctx, c"[object ", tag, c"]")
}

pub unsafe fn js_object_toLocaleString(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_Invoke(
        ctx.as_mut().unwrap(),
        this_val,
        JS_ATOM_toString,
        0,
        std::ptr::null_mut(),
    )
}

pub unsafe fn js_object_valueOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_ToObject(ctx, this_val)
}

pub unsafe fn js_object_hasOwnProperty(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let atom = JS_ValueToAtom(ctx, *argv.offset(0)); // must be done first
    if unlikely(atom == JS_ATOM_NULL) {
        return JSValue::EXCEPTION;
    }

    let obj = JS_ToObject(ctx, this_val);
    if JS_IsException(obj) {
        JS_FreeAtom(ctx, atom);
        return obj;
    }

    let p = JS_VALUE_GET_OBJ(obj);
    let ret = JS_GetOwnPropertyInternal(ctx, std::ptr::null_mut(), p, atom);
    JS_FreeAtom(ctx, atom);
    JS_FreeValue(ctx, obj);
    if ret < 0 {
        JSValue::EXCEPTION
    } else {
        JS_NewBool(ret != 0)
    }
}

pub unsafe fn js_object_isPrototypeOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let v = *argv.offset(0);
    if !JS_IsObject(v) {
        return JS_FALSE;
    }
    let obj = JS_ToObject(ctx, this_val);
    if JS_IsException(obj) {
        return JSValue::EXCEPTION;
    }

    let mut v1 = JS_DupValue(ctx, v);
    let mut res = false;

    loop {
        v1 = JS_GetPrototype(ctx, v1);
        if JS_IsException(v1) {
            break;
        }
        if JS_IsNull(v1) {
            res = false;
            break;
        }
        if JS_VALUE_GET_OBJ(obj) == JS_VALUE_GET_OBJ(v1) {
            res = true;
            break;
        }

        // avoid infinite loop (possible with proxies)
        if js_poll_interrupts(ctx.as_mut().unwrap()) != 0 {
            JS_FreeValue(ctx, v1);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    JS_FreeValue(ctx, v1);
    JS_FreeValue(ctx, obj);

    JS_NewBool(res)
}

pub unsafe fn js_object_propertyIsEnumerable(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    if JS_IsException(obj) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let prop = JS_ValueToAtom(ctx, *argv.offset(0));
    if unlikely(prop == JS_ATOM_NULL) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut desc: JSPropertyDescriptor = std::mem::zeroed();
    let has_prop = JS_GetOwnPropertyInternal(ctx, &mut desc, JS_VALUE_GET_OBJ(obj), prop);
    if has_prop < 0 {
        JS_FreeAtom(ctx, prop);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }
    let res = if has_prop != 0 {
        let r = JS_NewBool(desc.flags & JS_PROP_ENUMERABLE != 0);
        js_free_desc(ctx, &mut desc);
        r
    } else {
        JSValue::FALSE
    };

    JS_FreeAtom(ctx, prop);
    JS_FreeValue(ctx, obj);

    res
}

pub unsafe fn js_object_get___proto__(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
    let val = JS_ToObject(ctx, this_val);
    if JS_IsException(val) {
        val
    } else {
        let ret = JS_GetPrototype(ctx, val);
        JS_FreeValue(ctx, val);
        ret
    }
}

pub unsafe fn js_object_set___proto__(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    proto: JSValueConst,
) -> JSValue {
    if this_val.is_undefined_or_null() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }
    if !JS_IsObject(proto) && !JS_IsNull(proto) {
        return JSValue::UNDEFINED;
    }

    if JS_SetPrototypeInternal(ctx, this_val, proto, true).is_ok() {
        JSValue::UNDEFINED
    } else {
        JSValue::EXCEPTION
    }
}

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
    if JS_IsException(obj) {
        return JSValue::EXCEPTION;
    }

    if check_function(ctx, value).is_err() {
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
                res = JS_DupValue(
                    ctx,
                    if setter != 0 {
                        desc.setter
                    } else {
                        desc.getter
                    },
                );
            } else {
                res = JSValue::UNDEFINED;
            }
            js_free_desc(ctx, &mut desc);
            break;
        }

        obj = JS_GetPrototypeFree(ctx, obj);
        if JS_IsException(obj) {
            // obj is Proxy
            JS_FreeAtom(ctx, prop);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        if JS_IsNull(obj) {
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

pub const js_object_proto_funcs: [JSCFunctionListEntry; 11] = [
    JS_CFUNC_DEF!(c"toString", 0, js_object_toString),
    JS_CFUNC_DEF!(c"toLocaleString", 0, js_object_toLocaleString),
    JS_CFUNC_DEF!(c"valueOf", 0, js_object_valueOf),
    JS_CFUNC_DEF!(c"hasOwnProperty", 1, js_object_hasOwnProperty),
    JS_CFUNC_DEF!(c"isPrototypeOf", 1, js_object_isPrototypeOf),
    JS_CFUNC_DEF!(c"propertyIsEnumerable", 1, js_object_propertyIsEnumerable),
    JS_CGETSET_DEF!(
        c"__proto__",
        js_object_get___proto__,
        js_object_set___proto__
    ),
    JS_CFUNC_MAGIC_DEF!(c"__defineGetter__", 2, js_object___defineGetter__, 0),
    JS_CFUNC_MAGIC_DEF!(c"__defineSetter__", 2, js_object___defineGetter__, 1),
    JS_CFUNC_MAGIC_DEF!(c"__lookupGetter__", 1, js_object___lookupGetter__, 0),
    JS_CFUNC_MAGIC_DEF!(c"__lookupSetter__", 1, js_object___lookupGetter__, 1),
];
