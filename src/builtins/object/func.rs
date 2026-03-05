// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    builtins::{array::js_array_push, iterator::JSIteratorKindEnum},
    error::JSResult,
    function::{CallContext, FieldDef},
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_C_W_E,
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_THROW, JS_PROP_WRITABLE,
        ObjectPayload,
    },
    value::JSValue,
};

pub(crate) fn js_object_constructor(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let new_target = cc.this();

    if !new_target.is_undefined() && ctx.rt_ref().current_function() != new_target {
        ctx.create_object_from_ctor(new_target, ObjectPayload::OBJECT)
            .map_err(|_| -1)
    } else {
        match cc.arg(0) {
            JSValue::NULL | JSValue::UNDEFINED => ctx.create_base_object(),
            a => a.objectify(&mut ctx),
        }
    }
    .map(JSValue::Object)
}

pub(crate) fn js_object_create(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let proto = cc.arg(0);
    if !proto.is_object() && !proto.is_null() {
        ctx.throw_type_error("not a prototype");
        return Err(-1);
    }

    let mut obj = ctx.create_base_object_with_proto(proto.as_object().cloned())?;

    let props = cc.arg(1);
    if !props.is_undefined() {
        obj.define_properties(&mut ctx, props)?;
    }
    Ok(obj.into())
}

pub(crate) fn js_object_getPrototypeOf(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let mut ctx = cc.ctx_ptr();

    let val = cc.arg(0);
    if !val.is_object() {
        // ES6 feature non-compatible with ES5.1: primitive types are accepted
        if magic != 0 || val.is_undefined_or_null() {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }
    }
    val.prototype(&mut ctx)
}

pub(crate) fn js_object_setPrototypeOf(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let mut obj = cc.arg(0);
    obj.set_prototype_internal(&mut ctx, cc.arg(1), true)?;
    Ok(obj)
}

pub(crate) fn js_object_defineProperty(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let prop = cc.arg(1);
    let desc = cc.arg(2);

    if let Some(mut obj) = cc.arg(0).as_object().copied() {
        let atom = prop.to_atom(&mut ctx)?;

        // magic = 1: Reflect.defineProperty, don't throw on failure
        // magic = 0: Object.defineProperty, throw on failure
        let mut flags = 0;
        if cc.magic() == 0 {
            flags |= JS_PROP_THROW;
        }
        let b = obj.define_property_desc(&mut ctx, atom, desc, flags)?;
        Ok(if cc.magic() != 0 {
            JSValue::Bool(b)
        } else {
            JSValue::Object(obj)
        })
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

/// defineProperties(obj, properties)
pub(crate) fn js_object_defineProperties(mut cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let obj = cc.arg(0);
    if let Some(mut o) = obj.as_object().copied() {
        let arg1 = cc.arg(1);
        o.define_properties(cc.ctx_mut(), arg1)?;
        Ok(o.into())
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub(crate) fn js_object_getOwnPropertyNames(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let arg0 = cc.arg(0);
    arg0.build_own_property_kvs_array(
        &mut ctx,
        JS_GPN_INDEX_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub(crate) fn js_object_getOwnPropertySymbols(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let arg0 = cc.arg(0);
    arg0.build_own_property_kvs_array(
        &mut ctx,
        JS_GPN_SYMBOL_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub(crate) fn js_object_keys(cc: CallContext) -> JSResult {
    let kind = cc.magic();
    debug_assert!(
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i32
            ..=JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i32)
            .contains(&kind)
    );
    let mut ctx = cc.ctx_ptr();

    cc.arg(0)
        .build_own_property_kvs_array(&mut ctx, JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK, unsafe {
            std::mem::transmute::<u8, JSIteratorKindEnum>(kind as u8)
        })
}

pub(crate) fn js_object_groupBy(cc: CallContext) -> JSResult {
    let is_map = cc.magic() == 1;
    debug_assert!(
        !is_map,
        "deprecated: use js_map_groupBy for Map.groupBy() instead"
    );

    let mut ctx = cc.ctx_ptr();

    // check must be observed before argv[0] is accessed
    let func = cc.arg(1);
    func.ensure_function(&mut ctx)?;

    let mut groups: JSValue = {
        #[cfg(feature = "builtin_mapset")]
        if is_map {
            crate::builtins::mapset::js_map_constructor(CallContext::new_simple(
                ctx,
                JSValue::UNDEFINED,
                &[],
            ))?
        } else {
            ctx.create_base_object_with_proto(None)?.into()
        }

        #[cfg(not(feature = "builtin_mapset"))]
        ctx.create_base_object_with_proto(None)?.into()
    };

    let global = JSValue::from(ctx.global_obj());

    let obj = cc.arg(0);
    let it = obj.iter(ctx)?;

    for (idx, val) in it.enumerate() {
        if idx >= MAX_SAFE_INTEGER as usize {
            ctx.throw_type_error("too many elements");
            return Err(-1);
        }

        let val = val?;
        let key = func.call(&mut ctx, global, &[val, JSValue::from(idx as i64)])?;

        // get prop value array and key atom
        let (mut props_arr, key_atom): (JSValue, Atom) = {
            #[cfg(feature = "builtin_mapset")]
            if is_map {
                (
                    crate::builtins::mapset::js_map_get(CallContext::new_simple(
                        ctx,
                        groups,
                        std::slice::from_ref(&key),
                    ))?,
                    Atom::NULL,
                )
            } else {
                let atom = key.to_atom(&mut ctx)?;
                (groups.get_property(&mut ctx, atom)?, atom)
            }

            #[cfg(not(feature = "builtin_mapset"))]
            {
                let atom = key.to_atom(&mut ctx)?;
                (groups.get_property(&mut ctx, atom)?, atom)
            }
        };

        // don't change order

        if props_arr.is_undefined() {
            // entry is missing, create the entry
            props_arr = ctx.create_array()?.into();
            // set to groups
            #[cfg(feature = "builtin_mapset")]
            if is_map {
                crate::builtins::mapset::js_map_set(CallContext::new(
                    ctx,
                    groups,
                    &[key, props_arr],
                    0,
                    None,
                ))?;
            } else {
                groups.define_property_value(&mut ctx, key_atom, props_arr, JS_PROP_C_W_E)?;
            }
            #[cfg(not(feature = "builtin_mapset"))]
            groups.define_property_value(&mut ctx, key_atom, props_arr, JS_PROP_C_W_E)?;
        }

        js_array_push(CallContext::new(
            ctx,
            props_arr,
            std::slice::from_ref(&val),
            0,
            None,
        ))?;
    }

    Ok(groups)
}

pub(crate) fn js_object_isExtensible(cc: CallContext) -> JSResult {
    let reflect = cc.magic();
    let mut ctx = cc.ctx_ptr();

    let obj = cc.arg(0);
    if !obj.is_object() {
        return if reflect != 0 {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        } else {
            Ok(JSValue::Bool(false))
        };
    }

    Ok(JSValue::Bool(obj.is_extensible(&mut ctx)?))
}

pub(crate) fn js_object_preventExtensions(cc: CallContext) -> JSResult {
    let reflect = cc.magic() != 0;
    let mut ctx = cc.ctx_ptr();

    let arg = cc.arg(0);
    if let Some(mut obj) = arg.as_object().copied() {
        let ret = obj.prevent_extensions(&mut ctx)?;

        if reflect {
            Ok(JSValue::Bool(ret))
        } else if !ret {
            ctx.throw_type_error("proxy preventExtensions handler returned false");
            Err(-1)
        } else {
            Ok(obj.into())
        }
    } else if reflect {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    } else {
        Ok(arg)
    }
}

pub(crate) fn js_object_getOwnPropertyDescriptor(cc: CallContext) -> JSResult {
    let reflect = cc.magic() != 0;
    let mut ctx = cc.ctx_ptr();

    let obj = if reflect {
        // Reflect.getOwnPropertyDescriptor
        if let JSValue::Object(p) = cc.arg(0) {
            p
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }
    } else if let Ok(o) = cc.arg(0).objectify(&mut ctx) {
        o
    } else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let prop = cc.arg(1);
    let atom = prop.to_atom(&mut ctx)?;

    if let Some(desc) = obj.get_own_property(&mut ctx, atom)? {
        let mut ret = ctx.create_base_object()?;
        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

        if desc.flags & JS_PROP_GETSET != 0 {
            ret.define_property_value(&mut ctx, Atom::CONST_get, desc.getter, prop_flags)?;
            ret.define_property_value(&mut ctx, Atom::CONST_set, desc.setter, prop_flags)?;
        } else {
            ret.define_property_value(&mut ctx, Atom::CONST_value, desc.value, prop_flags)?;
            ret.define_property_value(
                &mut ctx,
                Atom::CONST_writable,
                JSValue::Bool(desc.flags & JS_PROP_WRITABLE != 0),
                prop_flags,
            )?;
        }

        ret.define_property_value(
            &mut ctx,
            Atom::CONST_enumerable,
            JSValue::Bool(desc.flags & JS_PROP_ENUMERABLE != 0),
            prop_flags,
        )?;
        ret.define_property_value(
            &mut ctx,
            Atom::CONST_configurable,
            JSValue::Bool(desc.flags & JS_PROP_CONFIGURABLE != 0),
            prop_flags,
        )?;

        return Ok(ret.into());
    }

    Ok(JSValue::UNDEFINED)
}

pub(crate) fn js_object_getOwnPropertyDescriptors(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let obj = cc.arg(0).objectify(&mut ctx)?;
    let mut res_obj = ctx.create_base_object()?;

    obj.own_property_names_foreach(
        &mut ctx,
        JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK,
        |cx, pkey| {
            let atom_value = pkey.atom.to_value(cx)?;
            let desc = js_object_getOwnPropertyDescriptor(CallContext::new_simple(
                cx.into(),
                JSValue::UNDEFINED,
                [obj.into(), atom_value].as_slice(),
            ))?;

            if !desc.is_undefined() {
                res_obj.define_property_value(
                    cx,
                    pkey.atom,
                    desc,
                    JS_PROP_C_W_E | JS_PROP_THROW,
                )?;
            }

            Ok(())
        },
    )?;

    Ok(res_obj.into())
}

pub(crate) fn js_object_is(cc: CallContext) -> JSResult {
    Ok(JSValue::Bool(cc.arg(0).is_same(cc.arg(1))))
}

/// Object.assign(obj, source1)
pub(crate) fn js_object_assign(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let mut obj = cc.arg(0).objectify(&mut ctx)?;

    if cc.length() > 1 {
        for a in &cc.arguments()[1..] {
            if !a.is_undefined_or_null() {
                let src = a.objectify(&mut ctx)?;
                obj.copy_data_properties_from(ctx, src, None, true)?;
            }
        }
    }

    Ok(obj.into())
}

pub(crate) fn js_object_seal(cc: CallContext) -> JSResult {
    let freeze = cc.magic() == 1;
    let mut ctx = cc.ctx_ptr();
    let mut obj = cc.arg(0);
    obj.seal(&mut ctx, freeze)?;
    Ok(obj)
}

pub(crate) fn js_object_isSealed(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let b = cc.arg(0).clone().is_sealed(&mut ctx, cc.magic() != 0)?;
    Ok(JSValue::Bool(b))
}

pub(crate) fn js_object_fromEntries(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let target = cc.arg(0);
    let mut enum_obj = target.iter(ctx)?;
    let mut ret = ctx.create_base_object()?;

    while let Some(r) = enum_obj.next() {
        match r {
            Ok(JSValue::Object(o)) => {
                let key = o.get_property_u32(&mut ctx, 0)?;
                let value = o.get_property_u32(&mut ctx, 1)?;

                let atom = key.to_atom(&mut ctx)?;
                ret.define_property_value(&mut ctx, atom, value, JS_PROP_C_W_E | JS_PROP_THROW)?;
            }
            Ok(_) => {
                ctx.throw_type_error_not_an_object();
                return Err(-1);
            }
            Err(_) => {
                // fail calling next()
                // close the iterator object, preserving pending exception
                let _ = enum_obj.close(true);
                return Err(-1);
            }
        }
    }

    Ok(ret.into())
}

pub(crate) fn js_object_hasOwn(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();
    let obj = cc.arg(0).objectify(&mut ctx)?;
    let atom = cc.arg(1).to_atom(&mut ctx)?;
    Ok(JSValue::Bool(
        obj.get_own_property(&mut ctx, atom)?.is_some(),
    ))
}

//
// non-standard
//
fn js_object___getClass(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    let name = if let Some(o) = cc.arg(0).as_object() {
        let cls = &cc.ctx().rt_ptr().class_registry[o.class_id() as usize];
        cls.class_name
    } else {
        Atom::CONST_empty_string
    };

    Ok(name.force_string(&mut ctx).unwrap().into())
}

pub const js_object_funcs: &[FieldDef] = &[
    FieldDef::CALL("create", 2, js_object_create, 0),
    FieldDef::CALL("getPrototypeOf", 1, js_object_getPrototypeOf, 0),
    FieldDef::CALL("setPrototypeOf", 2, js_object_setPrototypeOf, 0),
    FieldDef::CALL("defineProperty", 3, js_object_defineProperty, 0),
    FieldDef::CALL("defineProperties", 2, js_object_defineProperties, 0),
    FieldDef::CALL("getOwnPropertyNames", 1, js_object_getOwnPropertyNames, 0),
    FieldDef::CALL(
        "getOwnPropertySymbols",
        1,
        js_object_getOwnPropertySymbols,
        0,
    ),
    FieldDef::CALL("groupBy", 2, js_object_groupBy, 0),
    FieldDef::CALL(
        "keys",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16,
    ),
    FieldDef::CALL(
        "values",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    FieldDef::CALL(
        "entries",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16,
    ),
    FieldDef::CALL("isExtensible", 1, js_object_isExtensible, 0),
    FieldDef::CALL("preventExtensions", 1, js_object_preventExtensions, 0),
    FieldDef::CALL(
        "getOwnPropertyDescriptor",
        2,
        js_object_getOwnPropertyDescriptor,
        0,
    ),
    FieldDef::CALL(
        "getOwnPropertyDescriptors",
        1,
        js_object_getOwnPropertyDescriptors,
        0,
    ),
    FieldDef::CALL("is", 2, js_object_is, 0),
    FieldDef::CALL("assign", 2, js_object_assign, 0),
    FieldDef::CALL("seal", 1, js_object_seal, 0),
    FieldDef::CALL("freeze", 1, js_object_seal, 1),
    FieldDef::CALL("isSealed", 1, js_object_isSealed, 0),
    FieldDef::CALL("isFrozen", 1, js_object_isSealed, 1),
    FieldDef::CALL("fromEntries", 1, js_object_fromEntries, 0),
    FieldDef::CALL("hasOwn", 2, js_object_hasOwn, 0),
    // non standard
    FieldDef::CALL("__getClass", 1, js_object___getClass, 0),
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
];
