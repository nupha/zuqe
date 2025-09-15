use crate::{
    JS_CFUNC_NATIVE_DEF,
    array::js_array_push,
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom, consts::*},
    context::{JS_GetActiveFunction, JSContext},
    defer,
    error::JSResult,
    function::{CallContext, FieldDef},
    iterator::JSIteratorKindEnum,
    js_core::mapset::{js_map_constructor, js_map_get, js_map_set},
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_C_W_E,
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_THROW, JS_PROP_WRITABLE,
        ObjectPayload,
    },
    value::JSValue,
};

pub fn js_object_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if !new_target.is_undefined() && unsafe { !JS_GetActiveFunction(ctx).eq(new_target) } {
        ctx.create_object_apply_ctor(new_target, ObjectPayload::OBJECT)
            .map_err(|_| -1)
    } else {
        match cc.arg(0) {
            JSValue::NULL | JSValue::UNDEFINED => ctx.create_base_object(),
            a => a.to_object(ctx),
        }
    }
    .map(JSValue::Object)
}

pub fn js_object_create(ctx: &JSContext, cc: CallContext) -> JSResult {
    let proto = cc.arg(0);
    if !proto.is_object() && !proto.is_null() {
        ctx.throw_type_error("not a prototype");
        return Err(-1);
    }

    let obj = ctx.create_base_object_from_proto(proto.clone())?;
    let props = cc.arg(1);
    if !props.is_undefined() {
        obj.define_property_desc_multi(ctx, props)?;
    }
    Ok(obj.into())
}

pub fn js_object_getPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    if !val.is_object() {
        // ES6 feature non-compatible with ES5.1: primitive types are accepted
        if cc.magic() != 0 || matches!(val, JSValue::NULL | JSValue::UNDEFINED) {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }
    }

    val.prototype(ctx)
}

pub fn js_object_setPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    obj.set_prototype_internal(ctx, cc.arg(1).clone(), true)?;
    Ok(obj.clone())
}

pub fn js_object_defineProperty(ctx: &JSContext, cc: CallContext) -> JSResult {
    let prop = cc.arg(1);
    let desc = cc.arg(2);

    if let Some(obj) = cc.arg(0).object() {
        let atom = prop.to_atom(ctx)?;

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
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

/// defineProperties(obj, properties)
pub fn js_object_defineProperties(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    if let Some(o) = obj.object() {
        o.define_property_desc_multi(ctx, cc.arg(1))?;
        Ok(obj.clone())
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_object_getOwnPropertyNames(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.arg(0).build_own_property_kvs_array(
        ctx,
        JS_GPN_INDEX_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub fn js_object_getOwnPropertySymbols(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.arg(0).build_own_property_kvs_array(
        ctx,
        JS_GPN_SYMBOL_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub fn js_object_keys(ctx: &JSContext, cc: CallContext) -> JSResult {
    let kind = cc.magic();

    debug_assert!(
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i32
            ..=JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i32)
            .contains(&kind)
    );

    cc.arg(0)
        .build_own_property_kvs_array(ctx, JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK, unsafe {
            std::mem::transmute::<u8, JSIteratorKindEnum>(kind as u8)
        })
}

pub fn js_object_groupBy(ctx: &JSContext, cc: CallContext) -> JSResult {
    // "is function?" check must be observed before argv[0] is accessed
    let cb = cc.arg(1);
    cb.ensure_callable(ctx)?;

    let it = cc.arg(0).iter(ctx)?;

    let is_map = cc.magic();
    let groups = if is_map != 0 {
        js_map_constructor(ctx, CallContext::new(ctx, JSValue::UNDEFINED, &[], 0, None))?
    } else {
        ctx.create_base_object_from_proto(JSValue::NULL)?.into()
    };

    let mut idx: i64 = 0;
    for val in it {
        let val = val?;

        if idx >= MAX_SAFE_INTEGER {
            ctx.throw_type_error("too many elements");
            return Err(-1);
        }

        let global = ctx.0.borrow().global_obj.clone();
        let key = cb.call(ctx, &global, &[val.clone(), JSValue::from(idx)])?;

        // get prop value array and key atom
        let (props_arr, key_atom): (JSResult, JSAtom) = if is_map != 0 {
            (
                js_map_get(
                    ctx,
                    CallContext::new_simple(ctx, groups.clone(), std::slice::from_ref(&key)),
                ),
                JS_ATOM_NULL,
            )
        } else {
            let atom = key.to_atom(ctx)?;
            (groups.get_property(ctx, atom), atom)
        };

        // don't change order
        let key_atom = key_atom.into_rc(ctx.rt());
        let mut props_arr = props_arr?;

        if props_arr.is_undefined() {
            // entry is missing, create the entry
            props_arr = ctx.create_array()?.into();

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
                groups.define_property_value(ctx, *key_atom, props_arr.clone(), JS_PROP_C_W_E)?;
            }
        }

        js_array_push(
            ctx,
            CallContext::new(ctx, props_arr, std::slice::from_ref(&val), 0, None),
        )?;

        idx += 1;
    }

    Ok(groups)
}

pub fn js_object_isExtensible(ctx: &JSContext, cc: CallContext) -> JSResult {
    let reflect = cc.magic();
    let obj = cc.arg(0);

    if !obj.is_object() {
        return if reflect != 0 {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        } else {
            Ok(JSValue::Bool(false))
        };
    }

    Ok(JSValue::Bool(obj.is_extensible(ctx)?))
}

pub fn js_object_preventExtensions(ctx: &JSContext, cc: CallContext) -> JSResult {
    let reflect = cc.magic() != 0;
    let arg = cc.arg(0);

    if let Some(obj) = arg.object() {
        let ret = obj.prevent_extensions(ctx)?;
        if reflect {
            Ok(JSValue::Bool(ret))
        } else {
            if !ret {
                ctx.throw_type_error("proxy preventExtensions handler returned false");
                Err(-1)
            } else {
                Ok(obj.clone().into())
            }
        }
    } else {
        return if reflect {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        } else {
            Ok(arg.clone())
        };
    }
}

pub fn js_object_getOwnPropertyDescriptor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let reflect = cc.magic() != 0;

    let obj = if reflect {
        // Reflect.getOwnPropertyDescriptor
        if let JSValue::Object(p) = cc.arg(0) {
            p.clone()
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }
    } else {
        if let Ok(o) = cc.arg(0).to_object(ctx) {
            o
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }
    };

    let prop = cc.arg(1);
    let atom = prop.to_atom(ctx)?.into_rc(ctx.rt());

    if let Some(desc) = obj.get_own_property(ctx, *atom)? {
        let ret = ctx.create_base_object()?;
        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

        if desc.flags & JS_PROP_GETSET != 0 {
            ret.define_property_value(ctx, JS_ATOM_get, desc.getter, prop_flags)?;
            ret.define_property_value(ctx, JS_ATOM_set, desc.setter, prop_flags)?;
        } else {
            ret.define_property_value(ctx, JS_ATOM_value, desc.value, prop_flags)?;
            ret.define_property_value(
                ctx,
                JS_ATOM_writable,
                JSValue::Bool(desc.flags & JS_PROP_WRITABLE != 0),
                prop_flags,
            )?;
        }

        ret.define_property_value(
            ctx,
            JS_ATOM_enumerable,
            JSValue::Bool(desc.flags & JS_PROP_ENUMERABLE != 0),
            prop_flags,
        )?;
        ret.define_property_value(
            ctx,
            JS_ATOM_configurable,
            JSValue::Bool(desc.flags & JS_PROP_CONFIGURABLE != 0),
            prop_flags,
        )?;

        return Ok(ret.into());
    }

    Ok(JSValue::UNDEFINED)
}

pub fn js_object_getOwnPropertyDescriptors(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0).to_object(ctx)?;
    let res_obj = ctx.create_base_object()?;

    obj.own_property_names_foreach(ctx, JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK, |cx, name| {
        let atom_value = name.atom.to_string_or_symbol(ctx)?;
        let desc = js_object_getOwnPropertyDescriptor(
            ctx,
            CallContext::new(
                ctx,
                JSValue::UNDEFINED,
                &[obj.clone().into(), atom_value],
                0,
                None,
            ),
        )?;

        if !desc.is_undefined() {
            res_obj.define_property_value(cx, name.atom, desc, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }

        Ok(())
    })?;

    Ok(res_obj.into())
}

pub fn js_object_is(ctx: &JSContext, cc: CallContext) -> JSResult {
    Ok(JSValue::Bool(cc.arg(0).is_same(cc.arg(1))))
}

/// Object.assign(obj, source1)
pub fn js_object_assign(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0).to_object(ctx)?;

    if cc.length() > 1 {
        for a in &cc.arguments()[1..] {
            if !a.is_undefined_or_null() {
                let src = a.to_object(ctx)?;
                obj.copy_data_properties_from(ctx, &src.into(), None, true)?;
            }
        }
    }

    Ok(obj.into())
}

pub fn js_object_seal(ctx: &JSContext, cc: CallContext) -> JSResult {
    let freeze = cc.magic() == 1;
    let obj = cc.arg(0).clone();
    obj.seal(ctx, freeze)?;
    Ok(obj)
}

pub fn js_object_isSealed(ctx: &JSContext, cc: CallContext) -> JSResult {
    let b = cc.arg(0).is_sealed(ctx, cc.magic() != 0)?;
    Ok(JSValue::Bool(b))
}

pub fn js_object_fromEntries(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = ctx.create_base_object()?;

    // RequireObjectCoercible() not necessary because it is tested in
    // JS_GetIterator() by JS_GetProperty()
    for item in cc.arg(0).iter(ctx)? {
        if let Some(o) = item?.object() {
            let key = o.get_property_u32(ctx, 0)?;
            let value = o.get_property_u32(ctx, 1)?;
            obj.define_property_value_value(ctx, key, value, JS_PROP_C_W_E | JS_PROP_THROW)?;
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }
    }

    Ok(obj.into())
}

pub fn js_object_hasOwn(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0).to_object(ctx)?;
    let atom = cc.arg(1).to_atom(ctx)?;

    // free atom when exit
    let rt = ctx.rt_ptr();
    let _atom = JSAtom(atom.0);
    defer!({
        JS_FreeAtomRT(rt.as_ptr(), _atom);
    });

    Ok(JSValue::Bool(obj.get_own_property(ctx, atom)?.is_some()))
}

pub static js_object_funcs: [FieldDef; 23] = [
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
