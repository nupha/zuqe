// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use smallvec::SmallVec;

use crate::{
    Array,
    atom::Atom,
    builtins::{array::js_array_push, iterator::JSIteratorKindEnum},
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_C_W_E,
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_THROW, JS_PROP_WRITABLE,
        PayloadPtr,
    },
    value::JSValue,
};

pub(crate) fn js_object_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if !new_target.is_undefined() && ctx.rt().current_function() != new_target {
        ctx.ordinary_create_from_constructor_payload(
            new_target,
            PayloadPtr::none(crate::JS_CLASS_OBJECT),
        )
    } else {
        let v = cc.arg(0);
        if v.is_undefined_or_null() {
            ctx.create_base_object()
        } else {
            v.objectify(ctx)
        }
    }
    .map(JSValue::from)
}

pub(crate) fn js_object_create(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let proto = cc.arg(0);
    if !proto.is_object() && !proto.is_null() {
        return Err(ctx.throw_type_error("not a prototype"));
    }

    let mut obj = ctx.create_base_object_with_proto(proto.as_object())?;

    let props = cc.arg(1);
    if !props.is_undefined() {
        obj.define_properties(ctx, props)?;
    }
    Ok(obj.into())
}

pub(crate) fn js_object_getPrototypeOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let val = cc.arg(0);
    if !val.is_object() {
        // ES6 feature non-compatible with ES5.1: primitive types are accepted
        if magic != 0 || val.is_undefined_or_null() {
            return Err(ctx.throw_type_error_not_an_object());
        }
    }
    val.prototype(ctx)
}

pub(crate) fn js_object_setPrototypeOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = cc.arg(0);
    obj.set_prototype_internal(ctx, cc.arg(1), true)?;
    Ok(obj)
}

pub(crate) fn js_object_defineProperty(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let prop = cc.arg(1);
    let desc = cc.arg(2);

    if let Some(mut obj) = cc.arg(0).as_object() {
        let atom = prop.to_atom(ctx)?;

        // magic = 1: Reflect.defineProperty, don't throw on failure
        // magic = 0: Object.defineProperty, throw on failure
        let mut flags = 0;
        if cc.magic() == 0 {
            flags |= JS_PROP_THROW;
        }
        let b = obj.define_property_desc(ctx, atom, desc, flags)?;
        Ok(if cc.magic() != 0 {
            JSValue::from(b)
        } else {
            JSValue::from(obj)
        })
    } else {
        Err(ctx.throw_type_error_not_an_object())
    }
}

/// defineProperties(obj, properties)
pub(crate) fn js_object_defineProperties(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    if let Some(mut o) = obj.as_object() {
        let arg1 = cc.arg(1);
        o.define_properties(ctx, arg1)?;
        Ok(o.into())
    } else {
        Err(ctx.throw_type_error_not_an_object())
    }
}

pub(crate) fn js_object_getOwnPropertyNames(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let arg0 = cc.arg(0);
    arg0.build_own_property_kvs_array(
        ctx,
        JS_GPN_STR_OR_INT_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub(crate) fn js_object_getOwnPropertySymbols(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    let arg0 = cc.arg(0);
    arg0.build_own_property_kvs_array(
        ctx,
        JS_GPN_SYMBOL_MASK,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
    )
}

pub(crate) fn js_object_keys(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let kind = cc.magic();
    debug_assert!(
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i32
            ..=JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i32)
            .contains(&kind)
    );

    cc.arg(0)
        .build_own_property_kvs_array(ctx, JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK, unsafe {
            std::mem::transmute::<u8, JSIteratorKindEnum>(kind as u8)
        })
}

pub(crate) fn js_object_groupBy(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_map = cc.magic() == 1;
    debug_assert!(
        !is_map,
        "deprecated: use js_map_groupBy for Map.groupBy() instead"
    );

    // check must be observed before argv[0] is accessed
    let func = cc.arg(1);
    func.ensure_callable(ctx)?;

    let mut groups: JSValue = {
        #[cfg(feature = "builtin_mapset")]
        if is_map {
            crate::builtins::mapset::js_map_constructor(
                ctx,
                CallContext::new_simple(JSValue::UNDEFINED, &[]),
            )?
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
            return Err(ctx.throw_type_error("too many elements"));
        }

        let val = val?;
        let key = func.call(ctx, global, &[val, JSValue::from(idx as i64)])?;

        // get prop value array and key atom
        let (mut props_arr, key_atom): (JSValue, Atom) = {
            #[cfg(feature = "builtin_mapset")]
            if is_map {
                (
                    crate::builtins::mapset::js_map_get(
                        ctx,
                        CallContext::new_simple(groups, std::slice::from_ref(&key)),
                    )?,
                    Atom::NULL,
                )
            } else {
                let atom = key.to_atom(ctx)?;
                (groups.get_property(ctx, atom)?, atom)
            }

            #[cfg(not(feature = "builtin_mapset"))]
            {
                let atom = key.to_atom(ctx)?;
                (groups.get_property(ctx, atom)?, atom)
            }
        };

        // don't change order

        if props_arr.is_undefined() {
            // entry is missing, create the entry
            props_arr = Array::create(ctx)?.into();
            // set to groups
            #[cfg(feature = "builtin_mapset")]
            if is_map {
                crate::builtins::mapset::js_map_set(
                    ctx,
                    CallContext::new(groups, &[key, props_arr], 0, None),
                )?;
            } else {
                groups.define_property_value(ctx, key_atom, props_arr, JS_PROP_C_W_E)?;
            }
            #[cfg(not(feature = "builtin_mapset"))]
            groups.define_property_value(ctx, key_atom, props_arr, JS_PROP_C_W_E)?;
        }

        js_array_push(
            ctx,
            CallContext::new(props_arr, std::slice::from_ref(&val), 0, None),
        )?;
    }

    Ok(groups)
}

pub(crate) fn js_object_isExtensible(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let reflect = cc.magic();

    let obj = cc.arg(0);
    if !obj.is_object() {
        return if reflect != 0 {
            Err(ctx.throw_type_error_not_an_object())
        } else {
            Ok(JSValue::from(false))
        };
    }

    Ok(JSValue::from(obj.is_extensible(ctx)?))
}

pub(crate) fn js_object_preventExtensions(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let reflect = cc.magic() != 0;

    let arg = cc.arg(0);
    if let Some(mut obj) = arg.as_object() {
        let ret = obj.prevent_extensions(ctx)?;

        if reflect {
            Ok(JSValue::from(ret))
        } else if !ret {
            Err(ctx.throw_type_error("proxy preventExtensions handler returned false"))
        } else {
            Ok(obj.into())
        }
    } else if reflect {
        Err(ctx.throw_type_error_not_an_object())
    } else {
        Ok(arg)
    }
}

pub(crate) fn js_object_getOwnPropertyDescriptor(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    let reflect = cc.magic() != 0;

    let mut obj = if reflect {
        // Reflect.getOwnPropertyDescriptor
        if let Some(p) = cc.arg(0).as_object() {
            p
        } else {
            return Err(ctx.throw_type_error_not_an_object());
        }
    } else if let Ok(o) = cc.arg(0).objectify(ctx) {
        o
    } else {
        return Err(ctx.throw_type_error_not_an_object());
    };

    let prop = cc.arg(1);
    let atom = prop.to_atom(ctx)?;

    if let Some(desc) = obj.get_own_property(ctx, atom)? {
        let mut ret = ctx.create_base_object()?;
        let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

        if desc.flags & JS_PROP_GETSET != 0 {
            ret.define_property_value(ctx, Atom::CONST_get, desc.getter, prop_flags)?;
            ret.define_property_value(ctx, Atom::CONST_set, desc.setter, prop_flags)?;
        } else {
            ret.define_property_value(ctx, Atom::CONST_value, desc.value, prop_flags)?;
            ret.define_property_value(
                ctx,
                Atom::CONST_writable,
                JSValue::from(desc.flags & JS_PROP_WRITABLE != 0),
                prop_flags,
            )?;
        }

        ret.define_property_value(
            ctx,
            Atom::CONST_enumerable,
            JSValue::from(desc.flags & JS_PROP_ENUMERABLE != 0),
            prop_flags,
        )?;
        ret.define_property_value(
            ctx,
            Atom::CONST_configurable,
            JSValue::from(desc.flags & JS_PROP_CONFIGURABLE != 0),
            prop_flags,
        )?;

        return Ok(ret.into());
    }

    Ok(JSValue::UNDEFINED)
}

pub(crate) fn js_object_getOwnPropertyDescriptors(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    let mut obj = cc.arg(0).objectify(ctx)?;

    let mut keys = SmallVec::<[Atom; 8]>::new();
    obj.own_property_names_foreach(
        ctx,
        JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK,
        |_, pkey| {
            keys.push(pkey.atom);
            Ok(())
        },
    )?;

    let mut res_obj = ctx.create_base_object()?;
    for atom in keys {
        let atom_value = atom.to_value(ctx)?;
        let desc = js_object_getOwnPropertyDescriptor(
            ctx,
            CallContext::new_simple(JSValue::UNDEFINED, &[obj.into(), atom_value]),
        )?;

        if !desc.is_undefined() {
            res_obj.define_property_value(ctx, atom, desc, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }
    }

    Ok(res_obj.into())
}

pub(crate) fn js_object_is(_: &mut ScopedContext, cc: CallContext) -> JSResult {
    Ok(JSValue::from(cc.arg(0).is_same(cc.arg(1))))
}

/// Object.assign(obj, source1)
pub(crate) fn js_object_assign(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = cc.arg(0).objectify(ctx)?;

    if cc.length() > 1 {
        for a in &cc.arguments()[1..] {
            if !a.is_undefined_or_null() {
                let src = a.objectify(ctx)?;
                obj.copy_data_properties_from(ctx, src, None, true)?;
            }
        }
    }

    Ok(obj.into())
}

pub(crate) fn js_object_seal(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let freeze = cc.magic() == 1;
    let mut obj = cc.arg(0);

    if freeze
        && let Some(ta) = obj.as_object_TypedArray()
        && !ta.is_detached()
    {
        // SetIntegrityLevel(frozen): a TypedArray backed by a resizable buffer,
        // or with non-zero length, cannot be frozen.
        if ta.payload().array_buffer().payload().is_resizable() || ta.length() > 0 {
            return Err(ctx.throw_type_error("cannot freeze a typed array"));
        }
    }

    obj.seal(ctx, freeze)?;
    Ok(obj)
}

pub(crate) fn js_object_isSealed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let b = cc.arg(0).clone().is_sealed(ctx, cc.magic() != 0)?;
    Ok(JSValue::from(b))
}

pub(crate) fn js_object_fromEntries(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let target = cc.arg(0);
    let mut enum_obj = target.iter(ctx)?;
    let mut ret = ctx.create_base_object()?;

    while let Some(r) = enum_obj.next() {
        match r.map(|v| v.as_object()) {
            Ok(Some(o)) => {
                let key = o.get_property_u32(ctx, 0)?;
                let value = o.get_property_u32(ctx, 1)?;
                let atom = key.to_atom(ctx)?;
                ret.define_property_value(ctx, atom, value, JS_PROP_C_W_E | JS_PROP_THROW)?;
            }
            Ok(_) => {
                return Err(ctx.throw_type_error_not_an_object());
            }
            Err(_) => {
                // fail calling next()
                // close the iterator object, preserving pending exception
                debug_assert!(!enum_obj.done);
                let _ = enum_obj.close();
                return Err(-1);
            }
        }
    }

    Ok(ret.into())
}

pub(crate) fn js_object_hasOwn(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = cc.arg(0).objectify(ctx)?;
    let atom = cc.arg(1).to_atom(ctx)?;
    Ok(JSValue::from(obj.get_own_property(ctx, atom)?.is_some()))
}

//
// non-standard
//
fn js_object___getClass(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    match cc.magic() {
        0 => {
            // get class name
            let name = if let Some(o) = cc.arg(0).as_object() {
                ctx.rt().class_def(o.class_id()).unwrap().name_atom
            } else {
                Atom::CONST_empty_string
            };
            Ok(JSValue::make_string(name.force_string(ctx).unwrap()))
        }
        1 => {
            // get class id
            Ok(cc
                .arg(0)
                .as_object()
                .map_or(JSValue::NULL, |o| JSValue::from(o.class_id() as i32)))
        }
        _ => {
            unreachable!();
        }
    }
}

pub const js_object_funcs: &[FieldDef] = &[
    FieldDef::FUNC("create", 2, js_object_create, 0),
    FieldDef::FUNC("getPrototypeOf", 1, js_object_getPrototypeOf, 0),
    FieldDef::FUNC("setPrototypeOf", 2, js_object_setPrototypeOf, 0),
    FieldDef::FUNC("defineProperty", 3, js_object_defineProperty, 0),
    FieldDef::FUNC("defineProperties", 2, js_object_defineProperties, 0),
    FieldDef::FUNC("getOwnPropertyNames", 1, js_object_getOwnPropertyNames, 0),
    FieldDef::FUNC(
        "getOwnPropertySymbols",
        1,
        js_object_getOwnPropertySymbols,
        0,
    ),
    FieldDef::FUNC("groupBy", 2, js_object_groupBy, 0),
    FieldDef::FUNC(
        "keys",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16,
    ),
    FieldDef::FUNC(
        "values",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    FieldDef::FUNC(
        "entries",
        1,
        js_object_keys,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16,
    ),
    FieldDef::FUNC("isExtensible", 1, js_object_isExtensible, 0),
    FieldDef::FUNC("preventExtensions", 1, js_object_preventExtensions, 0),
    FieldDef::FUNC(
        "getOwnPropertyDescriptor",
        2,
        js_object_getOwnPropertyDescriptor,
        0,
    ),
    FieldDef::FUNC(
        "getOwnPropertyDescriptors",
        1,
        js_object_getOwnPropertyDescriptors,
        0,
    ),
    FieldDef::FUNC("is", 2, js_object_is, 0),
    FieldDef::FUNC("assign", 2, js_object_assign, 0),
    FieldDef::FUNC("seal", 1, js_object_seal, 0),
    FieldDef::FUNC("freeze", 1, js_object_seal, 1),
    FieldDef::FUNC("isSealed", 1, js_object_isSealed, 0),
    FieldDef::FUNC("isFrozen", 1, js_object_isSealed, 1),
    FieldDef::FUNC("fromEntries", 1, js_object_fromEntries, 0),
    FieldDef::FUNC("hasOwn", 2, js_object_hasOwn, 0),
    // non standard
    FieldDef::FUNC("__getClass", 1, js_object___getClass, 0),
    FieldDef::FUNC("__getClassId", 1, js_object___getClass, 1),
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
