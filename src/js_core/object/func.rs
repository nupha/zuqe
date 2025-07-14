use crate::{
    JS_CFUNC_NATIVE_DEF,
    array::js_array_push,
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom, consts::*},
    context::{JS_GetActiveFunction, JSContext},
    defer,
    error::JSResult,
    function::{CallContext, JS_Call, NativeField},
    iterator::{JS_GetIterator, JS_IteratorClose, JS_IteratorNext, JSIteratorKindEnum},
    js_core::mapset::{js_map_constructor, js_map_get, js_map_set},
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_STRING_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_C_W_E,
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_THROW, JS_PROP_WRITABLE,
        JS_PreventExtensions, JS_SetPrototypeInternal, ObjectDetail, PropertyEnum,
    },
    value::JSValue,
};

pub fn JS_ObjectDefineProperties(
    ctx: &JSContext,
    obj: &JSValue,
    properties: &JSValue,
) -> Result<(), i32> {
    if !obj.is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let props = properties.coerce_to_object(ctx)?;

    if let JSValue::Object(obj) = obj {
        let p = unsafe { props.object_unchecked() };
        let r = p.own_property_names_foreach(
            ctx,
            JS_GPN_ENUM_ONLY | JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
            |cx, name: PropertyEnum| {
                let desc = props.get_property(cx, name.atom).check_exception()?;
                if let JSValue::Object(desc) = desc {
                    obj.define_property_desc(cx, name.atom, &desc, JS_PROP_THROW)
                        .map(|_| ())
                } else {
                    ctx.JS_ThrowTypeErrorNotAnObject();
                    return Err(-1);
                }
            },
        );

        r
    } else {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }
}

pub fn js_object_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    debug_assert!(cc.length() > 0, "at least one slot for argument");

    let new_target = cc.this();
    if !new_target.is_undefined() && unsafe { !JS_GetActiveFunction(ctx).eq(new_target) } {
        ctx.create_instance_from_ctor(new_target, ObjectDetail::OBJECT)
            .map_err(|_| -1)
    } else {
        match cc.arg(0) {
            JSValue::NULL | JSValue::UNDEFINED => ctx.create_base_object(),
            a => a.coerce_to_object(ctx),
        }
    }
}

pub fn js_object_create(ctx: &JSContext, cc: CallContext) -> JSResult {
    let proto = cc.arg(0);
    if !proto.is_object() && !proto.is_null() {
        ctx.JS_ThrowTypeError("not a prototype");
        return Err(-1);
    }

    let obj = ctx.create_base_object_from_proto(proto)?;
    let props = cc.arg(1);
    if !props.is_undefined() {
        JS_ObjectDefineProperties(ctx, &obj, props)?;
    }
    Ok(obj)
}

pub fn js_object_getPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    if !val.is_object() {
        // ES6 feature non compatible with ES5.1: primitive types are accepted
        if cc.magic() != 0 || matches!(val, JSValue::NULL | JSValue::UNDEFINED) {
            ctx.JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }
    }

    val.prototype(ctx)
}

pub fn js_object_setPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    JS_SetPrototypeInternal(ctx, obj, cc.arg(1).clone(), true)?;
    Ok(obj.clone())
}

pub fn js_object_defineProperty(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    let prop = cc.arg(1);
    let desc = cc.arg(2);

    match (obj, desc) {
        (JSValue::Object(obj), JSValue::Object(desc)) => {
            let atom = prop.coerce_to_atom(ctx)?;

            // magic = 1 if called as Reflect.defineProperty
            let mut flags = 0;
            if cc.magic() == 0 {
                flags |= JS_PROP_THROW;
            }
            let ret = obj.define_property_desc(ctx, atom, desc, flags);
            JS_FreeAtom(ctx, atom);

            let b = ret?;
            Ok(if cc.magic() != 0 {
                JSValue::Bool(b)
            } else {
                JSValue::Object(obj.clone())
            })
        }
        _ => {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }
}

/// defineProperties(obj, properties)
pub fn js_object_defineProperties(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    JS_ObjectDefineProperties(ctx, obj, cc.arg(1))?;
    Ok(obj.clone())
}

pub fn js_object_getOwnPropertyNames(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.arg(0).get_own_property_names2(
        ctx,
        JS_GPN_STRING_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub fn js_object_getOwnPropertySymbols(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.arg(0).get_own_property_names2(
        ctx,
        JS_GPN_SYMBOL_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub fn js_object_keys(ctx: &JSContext, cc: CallContext) -> JSResult {
    let kind = cc.magic();

    debug_assert!(
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i32
            ..=JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i32)
            .contains(&kind)
    );

    cc.arg(0)
        .get_own_property_names2(ctx, JS_GPN_ENUM_ONLY | JS_GPN_STRING_MASK, unsafe {
            std::mem::transmute::<u8, JSIteratorKindEnum>(kind as u8)
        })
}

pub fn js_object_groupBy(ctx: &JSContext, cc: CallContext) -> JSResult {
    // "is function?" check must be observed before argv[0] is accessed
    let cb = cc.arg(1);
    cb.ensure_callable(ctx)?;

    let iter = JS_GetIterator(ctx, cc.arg(0), false).check_exception()?;
    let next = iter.get_property(ctx, JS_ATOM_next).check_exception()?;

    let is_map = cc.magic();
    let groups = if is_map != 0 {
        unsafe {
            js_map_constructor(ctx, &JSValue::UNDEFINED, 0, std::ptr::null_mut(), 0)
                .check_exception()?
        }
    } else {
        ctx.create_base_object_from_proto(&JSValue::NULL)?
    };

    let mut idx: i64 = 0;
    loop {
        if idx >= MAX_SAFE_INTEGER {
            ctx.JS_ThrowTypeError("too many elements");
            let _ = JS_IteratorClose(ctx, &iter, true);
            return Err(-1);
        }

        let mut done = false;
        let val = JS_IteratorNext(ctx, &iter, &next, &[], &mut done).check_exception()?;
        if done {
            break; // v is JSValue::UNDEFINED
        }

        let key = JS_Call(
            ctx,
            &cb,
            &ctx.0.borrow().global_obj,
            &[val.clone(), JSValue::from(idx)],
        )
        .check_exception()
        .inspect_err(|_| {
            let _ = JS_IteratorClose(ctx, &iter, true);
        })?;

        // get prop value array and key atom
        let (mut props_arr, key_atom): (JSValue, JSAtom) = if is_map != 0 {
            (
                unsafe {
                    js_map_get(
                        ctx,
                        groups.clone(),
                        1,
                        std::slice::from_ref(&key).as_ptr() as _,
                        0,
                    )
                },
                JS_ATOM_NULL,
            )
        } else {
            let atom = key.coerce_to_atom(ctx).inspect_err(|_| {
                let _ = JS_IteratorClose(ctx, &iter, true);
            })?;
            (groups.get_property(ctx, atom), atom)
        };

        // free `key_atom` when exit
        let rt = ctx.rt_ptr();
        let _a = JSAtom(key_atom.0);
        defer!({
            JS_FreeAtomRT(rt.as_ptr(), _a);
        });

        if props_arr.is_exception() {
            return Err(-1);
        }

        if props_arr.is_undefined() {
            // entry is missing, create the entry
            props_arr = ctx.create_array()?;

            // set to groups
            if is_map != 0 {
                js_map_set(
                    ctx,
                    CallContext::new(
                        ctx,
                        groups.clone(),
                        &[key.clone(), props_arr.clone()],
                        0,
                        None,
                    ),
                )?;
            } else {
                groups.define_property_value(ctx, key_atom, props_arr.clone(), JS_PROP_C_W_E)?;
            }
        }

        js_array_push(
            ctx,
            CallContext::new(ctx, props_arr, std::slice::from_ref(&val), 0, None),
        )?;

        idx += 1;
    } // loop

    Ok(groups)
}

pub fn js_object_isExtensible(ctx: &JSContext, cc: CallContext) -> JSResult {
    let reflect = cc.magic();
    let obj = cc.arg(0);

    if !obj.is_object() {
        return if reflect != 0 {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        } else {
            Ok(JSValue::Bool(false))
        };
    }

    Ok(JSValue::Bool(obj.is_extensible(ctx)?))
}

pub fn js_object_preventExtensions(ctx: &JSContext, cc: CallContext) -> JSResult {
    let reflect = cc.magic();
    let obj = cc.arg(0);

    if !obj.is_object() {
        return if reflect != 0 {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        } else {
            Ok(obj.clone())
        };
    }

    let ret = JS_PreventExtensions(ctx, obj)?;
    if reflect != 0 {
        Ok(JSValue::Bool(ret))
    } else {
        if !ret {
            ctx.JS_ThrowTypeError("proxy preventExtensions handler returned false");
            Err(-1)
        } else {
            Ok(obj.clone())
        }
    }
}

pub fn js_object_getOwnPropertyDescriptor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = if cc.magic() != 0 {
        // Reflect.getOwnPropertyDescriptor case
        if let JSValue::Object(p) = cc.arg(0) {
            JSValue::Object(p.clone())
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }
    } else {
        let o = cc.arg(0).coerce_to_object(ctx)?;
        if o.is_exception() {
            return Ok(JSValue::UNDEFINED);
        } else {
            o
        }
    };

    let prop = cc.arg(1);

    let atom = prop.coerce_to_atom(ctx)?;

    // free atom when exit
    let rt = ctx.rt_ptr();
    let _atom = JSAtom(atom.0);
    defer!({
        JS_FreeAtomRT(rt.as_ptr(), _atom);
    });

    if let JSValue::Object(p) = &obj {
        if let Some(desc) = p.get_own_property(ctx, atom)? {
            let ret = ctx.create_base_object()?;

            let flags = JS_PROP_C_W_E | JS_PROP_THROW;

            if desc.flags & JS_PROP_GETSET != 0 {
                ret.define_property_value(ctx, JS_ATOM_get, desc.getter, flags)?;
                ret.define_property_value(ctx, JS_ATOM_set, desc.setter, flags)?;
            } else {
                ret.define_property_value(ctx, JS_ATOM_value, desc.value, flags)?;
                ret.define_property_value(
                    ctx,
                    JS_ATOM_writable,
                    ((desc.flags & JS_PROP_WRITABLE) != 0).into(),
                    flags,
                )?;
            }

            ret.define_property_value(
                ctx,
                JS_ATOM_enumerable,
                ((desc.flags & JS_PROP_ENUMERABLE) != 0).into(),
                flags,
            )?;
            ret.define_property_value(
                ctx,
                JS_ATOM_configurable,
                ((desc.flags & JS_PROP_CONFIGURABLE) != 0).into(),
                flags,
            )?;

            return Ok(ret);
        }
    }

    Ok(JSValue::UNDEFINED)
}

pub fn js_object_getOwnPropertyDescriptors(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0).coerce_to_object(ctx)?;
    let res_obj = ctx.create_base_object()?;

    let p = unsafe { obj.object_unchecked() };
    p.own_property_names_foreach(ctx, JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK, |cx, name| {
        let atom_value = name.atom.to_value(ctx, false).check_exception()?;

        let desc = js_object_getOwnPropertyDescriptor(
            ctx,
            CallContext::new(ctx, JSValue::UNDEFINED, &[obj.clone(), atom_value], 0, None),
        )?;

        if !desc.is_undefined() {
            res_obj.define_property_value(cx, name.atom, desc, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }

        Ok(())
    })?;

    Ok(res_obj)
}

pub fn js_object_is(ctx: &JSContext, cc: CallContext) -> JSResult {
    Ok(JSValue::Bool(cc.arg(0).is_same(cc.arg(1))))
}

/// Object.assign(obj, source1)
pub fn js_object_assign(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0).coerce_to_object(ctx)?;

    if cc.length() > 1 {
        for a in &cc.arguments()[1..] {
            if !a.is_undefined_or_null() {
                let s = a
                    .coerce_to_object(ctx)
                    .unwrap_or(JSValue::EXCEPTION)
                    .check_exception()?;
                obj.copy_properties_from(ctx, &s, &JSValue::UNDEFINED, true)?;
            }
        }
    }

    Ok(obj)
}

pub fn js_object_seal(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.arg(0).clone().seal(ctx, cc.magic() != 0)
}

pub fn js_object_isSealed(ctx: &JSContext, cc: CallContext) -> JSResult {
    let b = cc.arg(0).is_sealed(ctx, cc.magic() != 0)?;
    Ok(JSValue::Bool(b))
}

pub fn js_object_fromEntries(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = ctx.create_base_object()?;

    // RequireObjectCoercible() not necessary because it is tested in
    // JS_GetIterator() by JS_GetProperty()
    let iterable = cc.arg(0);

    let iter = JS_GetIterator(ctx, iterable, false).check_exception()?;
    let next_method = iter
        .get_property(ctx, JS_ATOM_next)
        .check_exception()
        .inspect_err(|_| {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, &iter, true).unwrap();
            }
        })?;

    loop {
        let mut done = false;
        let item = JS_IteratorNext(ctx, &iter, &next_method, &[], &mut done);
        if item.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, &iter, true)?;
            }
            return Err(-1);
        }

        if done {
            break;
        }

        if !item.is_object() {
            ctx.JS_ThrowTypeErrorNotAnObject();
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, &iter, true)?;
            }
            return Err(-1);
        }

        let key = item.get_property_u32(ctx, 0);
        if key.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, &iter, true)?;
            }
            return Err(-1);
        }

        let value = item.get_property_u32(ctx, 1);
        if value.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, &iter, true)?;
            }
            return Err(-1);
        }

        if obj
            .define_property_value_value(ctx, key, value, JS_PROP_C_W_E | JS_PROP_THROW)
            .is_err()
        {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, &iter, true)?;
            }
            return Err(-1);
        }
    }

    Ok(obj)
}

pub fn js_object_hasOwn(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0).coerce_to_object(ctx)?;

    let atom = cc.arg(1).coerce_to_atom(ctx)?;

    // free atom when exit
    let rt = ctx.rt_ptr();
    let _atom = JSAtom(atom.0);
    defer!({
        JS_FreeAtomRT(rt.as_ptr(), _atom);
    });

    Ok(JSValue::Bool(obj.get_own_property(ctx, atom)?.is_some()))
}

pub const js_object_funcs: [NativeField; 23] = [
    JS_CFUNC_NATIVE_DEF!("create", 2, js_object_create, 0),
    JS_CFUNC_NATIVE_DEF!("getPrototypeOf", 1, js_object_getPrototypeOf, 0),
    JS_CFUNC_NATIVE_DEF!("setPrototypeOf", 2, js_object_setPrototypeOf, 0),
    JS_CFUNC_NATIVE_DEF!("defineProperty", 3, js_object_defineProperty, 0),
    JS_CFUNC_NATIVE_DEF!("defineProperties", 2, js_object_defineProperties, 0),
    JS_CFUNC_NATIVE_DEF!("getOwnPropertyNames", 1, js_object_getOwnPropertyNames, 0),
    JS_CFUNC_NATIVE_DEF!(
        "getOwnPropertySymbols",
        1,
        js_object_getOwnPropertySymbols,
        0
    ),
    JS_CFUNC_NATIVE_DEF!("groupBy", 2, js_object_groupBy, 0),
    JS_CFUNC_NATIVE_DEF!(
        "keys",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "values",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "entries",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16
    ),
    JS_CFUNC_NATIVE_DEF!("isExtensible", 1, js_object_isExtensible, 0),
    JS_CFUNC_NATIVE_DEF!("preventExtensions", 1, js_object_preventExtensions, 0),
    JS_CFUNC_NATIVE_DEF!(
        "getOwnPropertyDescriptor",
        2,
        js_object_getOwnPropertyDescriptor,
        0
    ),
    JS_CFUNC_NATIVE_DEF!(
        "getOwnPropertyDescriptors",
        1,
        js_object_getOwnPropertyDescriptors,
        0
    ),
    JS_CFUNC_NATIVE_DEF!("is", 2, js_object_is, 0),
    JS_CFUNC_NATIVE_DEF!("assign", 2, js_object_assign, 0),
    JS_CFUNC_NATIVE_DEF!("seal", 1, js_object_seal, 0),
    JS_CFUNC_NATIVE_DEF!("freeze", 1, js_object_seal, 1),
    JS_CFUNC_NATIVE_DEF!("isSealed", 1, js_object_isSealed, 0),
    JS_CFUNC_NATIVE_DEF!("isFrozen", 1, js_object_isSealed, 1),
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
    JS_CFUNC_NATIVE_DEF!("fromEntries", 1, js_object_fromEntries, 0),
    JS_CFUNC_NATIVE_DEF!("hasOwn", 2, js_object_hasOwn, 0),
];
