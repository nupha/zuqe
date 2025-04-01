use std::ffi::{c_void, CStr};

use crate::{
    array::{
        add_fast_array_element, convert_fast_array_to_array, set_array_length, JS_ToArrayLengthFree,
    },
    atom::{
        JSAtom, JS_AtomIsArrayIndex, JS_AtomIsNumericIndex, JS_AtomIsNumericIndex1, JS_FreeAtom,
        JS_NewAtom, JS_NewAtomInt64, JS_ValueToAtom, __JS_AtomToUInt32, consts::*, JS_ATOM_MAX_INT,
        JS_ATOM_NULL,
    },
    context::{is_strict_mode, JSContext, JS_DupContext},
    error::{
        JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized,
        JS_ThrowTypeErrorAtom, JS_ThrowTypeErrorReadOnly,
    },
    function::{call_setter, JSVarRef, JS_CallFree},
    proxy::js_proxy_isExtensible,
    runtime::{js_autoinit_func_table, JSRuntime},
    types::{likely, unlikely},
    value::{
        set_value, JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_FreeValueRT,
        JS_GetPrototypePrimitive, JS_IsException, JS_IsObject, JS_IsUndefined, JS_IsUninitialized,
        JS_NumberIsNegativeOrMinusZero, JS_ToBoolFree, JS_ToFloat64Free, JS_ToLengthFree,
        JS_ToNumberFree, JS_ToStringInternal, JS_ToUint32, JS_ToUint32Free, JS_ToUint8ClampFree,
        JS_TAG_EXCEPTION, JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_STRING, JS_TAG_UNDEFINED,
        JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_STRING,
        JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY,
    JS_CLASS_INT8_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_PROXY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT8_ARRAY,
    JS_MKPTR,
};

use super::{
    obj::{
        js_autoinit_free, js_autoinit_get_id, js_autoinit_get_realm, JSAutoInitIDEnum, JSObject,
        JS_IsFunction, JS_ToObject,
    },
    property_name::{js_free_desc, js_free_prop_enum, JS_GetOwnPropertyNamesInternal},
    shape::{
        find_hashed_shape_prop, js_clone_shape, js_dup_shape, js_free_shape, js_shape_hash_link,
        JSShapeProperty,
    },
};

// Property flags
pub const JS_PROP_CONFIGURABLE: u32 = 1 << 0;
pub const JS_PROP_WRITABLE: u32 = 1 << 1;
pub const JS_PROP_ENUMERABLE: u32 = 1 << 2;
pub const JS_PROP_C_W_E: u32 = JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE | JS_PROP_ENUMERABLE;
pub const JS_PROP_LENGTH: u32 = 1 << 3;
pub const JS_PROP_TMASK: u32 = 3 << 4;
pub const JS_PROP_NORMAL: u32 = 0 << 4;
pub const JS_PROP_GETSET: u32 = 1 << 4;
pub const JS_PROP_VARREF: u32 = 2 << 4;
pub const JS_PROP_AUTOINIT: u32 = 3 << 4;

// Property definition flags
pub const JS_PROP_HAS_SHIFT: u32 = 8;
pub const JS_PROP_HAS_CONFIGURABLE: u32 = 1 << 8;
pub const JS_PROP_HAS_WRITABLE: u32 = 1 << 9;
pub const JS_PROP_HAS_ENUMERABLE: u32 = 1 << 10;
pub const JS_PROP_HAS_GET: u32 = 1 << 11;
pub const JS_PROP_HAS_SET: u32 = 1 << 12;
pub const JS_PROP_HAS_VALUE: u32 = 1 << 13;
pub const JS_PROP_THROW: u32 = 1 << 14;
pub const JS_PROP_THROW_STRICT: u32 = 1 << 15;
pub const JS_PROP_NO_ADD: u32 = 1 << 16;
pub const JS_PROP_NO_EXOTIC: u32 = 1 << 17;

// Property types
pub const PROP_TYPE_IDENT: i32 = 0;
pub const PROP_TYPE_VAR: i32 = 1;
pub const PROP_TYPE_GET: i32 = 2;
pub const PROP_TYPE_SET: i32 = 3;
pub const PROP_TYPE_STAR: i32 = 4;
pub const PROP_TYPE_ASYNC: i32 = 5;
pub const PROP_TYPE_ASYNC_STAR: i32 = 6;
pub const PROP_TYPE_PRIVATE: i32 = (1 << 4);

// Get property names flags
pub const JS_GPN_STRING_MASK: i32 = (1 << 0);
pub const JS_GPN_SYMBOL_MASK: i32 = (1 << 1);
pub const JS_GPN_PRIVATE_MASK: i32 = (1 << 2);
/* only include the enumerable properties */
pub const JS_GPN_ENUM_ONLY: i32 = (1 << 4);
/* set theJSPropertyEnum.is_enumerable field */
pub const JS_GPN_SET_ENUM: i32 = (1 << 5);

pub struct JSPropertyEnum {
    pub is_enumerable: bool,
    pub atom: JSAtom,
}

pub struct JSPropertyDescriptor {
    pub flags: u32,
    pub value: JSValue,
    pub getter: JSValue,
    pub setter: JSValue,
}

#[derive(Clone, Copy)]
pub struct JSProperty {
    pub u: JSPropertyUnion,
}

#[derive(Clone, Copy)]
pub union JSPropertyUnion {
    pub value: JSValue,           // JS_PROP_NORMAL
    pub getset: JSPropertyGetSet, // JS_PROP_GETSET
    pub var_ref: *mut JSVarRef,   // JS_PROP_VARREF, TODO: use NonNull<JSVarRef>
    pub init: JSPropertyAutoInit, // JS_PROP_AUTOINIT
}

#[derive(Clone, Copy)]
pub struct JSPropertyGetSet {
    pub getter: *mut JSObject, // NULL if undefined
    pub setter: *mut JSObject, // NULL if undefined
}

#[derive(Clone, Copy)]
pub struct JSPropertyAutoInit {
    /// realm and init_id (JS_AUTOINIT_ID_x) in the 2 low bits
    pub realm_and_id: usize,
    // opaque pointer
    pub opaque: *mut c_void,
}

pub unsafe fn JS_ToPropertyKey(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    JS_ToStringInternal(ctx, val, true)
}

/* allowed flags:
   JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JS_PROP_ENUMERABLE
   JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
   JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_HAS_ENUMERABLE,
   JS_PROP_THROW, JS_PROP_NO_EXOTIC.
   If JS_PROP_THROW is set, return an exception instead of FALSE.
   if JS_PROP_NO_EXOTIC is set, do not call the exotic
   define_own_property callback.
   return -1 (exception), FALSE or TRUE.
*/
/// Note: property `val` is duplicated inside.
pub unsafe fn JS_DefineProperty(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    mut val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: u32,
) -> i32 {
    let mut mask: u32;
    let res: i32;

    if JS_VALUE_GET_TAG(this_obj) != JSTag::Object as i32 {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return -1;
    }
    let p = JS_VALUE_GET_OBJ(this_obj);

    loop {
        let mut pr = std::ptr::null_mut::<JSProperty>();
        let mut prs = find_own_property(std::ptr::addr_of_mut!(pr), p, prop);

        if !prs.is_null() {
            /* the range of the Array length property is always tested before */
            if ((*prs).flags() & JS_PROP_LENGTH) != 0 && (flags & JS_PROP_HAS_VALUE) != 0 {
                let array_length = if let Ok(n) = JS_ToArrayLengthFree(ctx, val.dup(), false) {
                    n
                } else {
                    return -1;
                };

                /* this code relies on the fact that Uint32 are never allocated */
                val = JSValue::from_u32(array_length);
                /* prs may have been modified */
                prs = find_own_property(&mut pr, p, prop);
                assert!(!prs.is_null());
            }
            /* property already exists */
            if !check_define_prop_flags((*prs).flags(), flags) {
                return (*ctx)
                    .JS_ThrowTypeErrorOrFalse(flags as _, c"property is not configurable");
            }

            if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_AUTOINIT {
                /* Instantiate property and retry */
                if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                    return -1;
                }
                continue; // redo_prop_update
            }

            if flags
                & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)
                != 0
            {
                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                    let new_getter = if JS_IsFunction(ctx, getter) {
                        JS_VALUE_GET_OBJ(getter)
                    } else {
                        std::ptr::null_mut()
                    };

                    let new_setter = if JS_IsFunction(ctx, setter) {
                        JS_VALUE_GET_OBJ(setter)
                    } else {
                        std::ptr::null_mut()
                    };

                    if ((*prs).flags() & JS_PROP_TMASK) != JS_PROP_GETSET {
                        if p.as_mut().unwrap().js_shape_prepare_update(ctx, &mut prs) != 0 {
                            return -1;
                        }
                        /* convert to getset */
                        if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
                            if let Some(vr) = Box::from_raw((*pr).u.var_ref).free((*ctx).rt_mut()) {
                                std::mem::forget(vr);
                            }
                            //  free_var_ref((*ctx).rt, (*pr).u.var_ref);
                        } else {
                            JS_FreeValue(ctx, (*pr).u.value);
                        }
                        (*prs).set_flags(
                            ((*prs).flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE))
                                | JS_PROP_GETSET,
                        );
                        (*pr).u.getset.getter = std::ptr::null_mut();
                        (*pr).u.getset.setter = std::ptr::null_mut();
                    } else {
                        if ((*prs).flags() & JS_PROP_CONFIGURABLE) == 0 {
                            if flags & JS_PROP_HAS_GET != 0 && new_getter != (*pr).u.getset.getter {
                                return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"property is not configurable",
                                );
                            }
                            if flags & JS_PROP_HAS_SET != 0 && new_setter != (*pr).u.getset.setter {
                                return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"property is not configurable",
                                );
                            }
                        }
                    }
                    if flags & JS_PROP_HAS_GET != 0 {
                        if !(*pr).u.getset.getter.is_null() {
                            JS_FreeValue(
                                ctx,
                                JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()),
                            );
                        }
                        if !new_getter.is_null() {
                            JS_DupValue(ctx, getter);
                        }
                        (*pr).u.getset.getter = new_getter;
                    }
                    if flags & JS_PROP_HAS_SET != 0 {
                        if !(*pr).u.getset.setter.is_null() {
                            JS_FreeValue(
                                ctx,
                                JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()),
                            );
                        }
                        if !new_setter.is_null() {
                            JS_DupValue(ctx, setter);
                        }
                        (*pr).u.getset.setter = new_setter;
                    }
                } else {
                    if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_GETSET {
                        /* convert to data descriptor */
                        if p.as_mut().unwrap().js_shape_prepare_update(ctx, &mut prs) != 0 {
                            return -1;
                        }
                        if !(*pr).u.getset.getter.is_null() {
                            JS_FreeValue(
                                ctx,
                                JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()),
                            );
                        }
                        if !(*pr).u.getset.setter.is_null() {
                            JS_FreeValue(
                                ctx,
                                JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()),
                            );
                        }
                        (*prs).set_flags((*prs).flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                        (*pr).u.value = JSValue::UNDEFINED;
                    } else if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
                        /* Note: JS_PROP_VARREF is always writable */
                    } else {
                        if ((*prs).flags() & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE)) == 0
                            && (flags & JS_PROP_HAS_VALUE) != 0
                        {
                            if !val.is_same((*pr).u.value) {
                                return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"property is not configurable",
                                );
                            } else {
                                return 1;
                            }
                        }
                    }
                    if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
                        if flags & JS_PROP_HAS_VALUE != 0 {
                            if (*p).class_id() == JS_CLASS_MODULE_NS {
                                /* JS_PROP_WRITABLE is always true for variable
                                references, but they are write protected in module name
                                spaces. */
                                if !val.is_same(*(*(*pr).u.var_ref).pvalue) {
                                    return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                                        flags as _,
                                        c"property is not configurable",
                                    );
                                }
                            } else {
                                /* update the reference */
                                set_value(ctx, (*(*pr).u.var_ref).pvalue, val.dup());
                            }
                        }
                        /* if writable is set to false, no longer a
                        reference (for mapped arguments) */
                        if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE
                        {
                            if (*p).class_id() == JS_CLASS_MODULE_NS {
                                return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"module namespace properties have writable = false",
                                );
                            }
                            if p.as_mut().unwrap().js_shape_prepare_update(ctx, &mut prs) != 0 {
                                return -1;
                            }
                            let val1 = JS_DupValue(ctx, *(*(*pr).u.var_ref).pvalue);
                            if let Some(vr) = Box::from_raw((*pr).u.var_ref).free((*ctx).rt_mut()) {
                                std::mem::forget(vr);
                                //  unreachable!();
                            }
                            // free_var_ref((*ctx).rt, (*pr).u.var_ref);
                            (*pr).u.value = val1;
                            (*prs).set_flags((*prs).flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                        }
                    } else if ((*prs).flags() & JS_PROP_LENGTH) != 0 {
                        if flags & JS_PROP_HAS_VALUE != 0 {
                            /* Note: no JS code is executable because
                            'val' is guaranted to be a Uint32 */
                            res = set_array_length(ctx, p, val.dup(), flags);
                        } else {
                            res = 1;
                        }
                        /* still need to reset the writable flag if
                        needed.  The JS_PROP_LENGTH is kept because the
                        Uint32 test is still done if the length
                        property is read-only. */
                        if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE
                        {
                            (_, prs) = (*p)
                                .shape_mut()
                                .and_then(|sh| sh.props.first_mut())
                                .unwrap();
                            //  prs =get_shape_prop((*p).shape);
                            if p.as_mut().unwrap().js_update_property_flags(
                                ctx,
                                std::ptr::addr_of_mut!(prs),
                                (*prs).flags() & !JS_PROP_WRITABLE,
                            ) != 0
                            {
                                return -1;
                            }
                        }
                        return res;
                    } else {
                        if flags & JS_PROP_HAS_VALUE != 0 {
                            JS_FreeValue(ctx, (*pr).u.value);
                            (*pr).u.value = val.dup();
                        }
                        if flags & JS_PROP_HAS_WRITABLE != 0 {
                            if p.as_mut().unwrap().js_update_property_flags(
                                ctx,
                                std::ptr::addr_of_mut!(prs),
                                ((*prs).flags() & !JS_PROP_WRITABLE)
                                    | (flags as u32 & JS_PROP_WRITABLE),
                            ) != 0
                            {
                                return -1;
                            }
                        }
                    }
                }
            }

            mask = 0;
            if flags & JS_PROP_HAS_CONFIGURABLE != 0 {
                mask |= JS_PROP_CONFIGURABLE;
            }
            if flags & JS_PROP_HAS_ENUMERABLE != 0 {
                mask |= JS_PROP_ENUMERABLE;
            }
            if p.as_mut().unwrap().js_update_property_flags(
                ctx,
                std::ptr::addr_of_mut!(prs),
                ((*prs).flags() & !mask) | (flags & mask),
            ) != 0
            {
                return -1;
            }

            return 1;
        }

        /* handle modification of fast array elements */
        if (*p).detailed.fast_array() {
            if (*p).class_id() == JS_CLASS_ARRAY {
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < (*p).u.array.count {
                        let prop_flags = get_prop_flags(flags, JS_PROP_C_W_E);
                        if prop_flags != JS_PROP_C_W_E {
                            // convert_to_slow_array标签
                            if convert_fast_array_to_array(ctx, p) != 0 {
                                return -1;
                            } else {
                                continue; // redo_prop_update
                            }
                        }
                        if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                            // convert_to_slow_array标签
                            if convert_fast_array_to_array(ctx, p) != 0 {
                                return -1;
                            } else {
                                continue; // redo_prop_update
                            }
                        }
                        if flags & JS_PROP_HAS_VALUE != 0 {
                            set_value(
                                ctx,
                                &mut *(*p).u.array.u.values.offset(idx as isize),
                                val.dup(),
                            );
                        }
                        return 1;
                    }
                }
            } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&(*p).class_id()) {
                if !prop.is_int_type() {
                    /* slow path with to handle all numeric indexes */
                    let num = JS_AtomIsNumericIndex1(ctx, prop);
                    if JS_IsUndefined(num) {
                        break; // goto typed_array_done
                    }
                    if JS_IsException(num) {
                        return -1;
                    }

                    if let Ok(b) = num.is_integer(ctx.as_mut().unwrap()) {
                        if !b {
                            JS_FreeValue(ctx, num);
                            return (*ctx).JS_ThrowTypeErrorOrFalse(
                                flags as _,
                                c"non integer index in typed array",
                            );
                        }
                    } else {
                        JS_FreeValue(ctx, num);
                        return -1;
                    };

                    let ret = JS_NumberIsNegativeOrMinusZero(ctx, num);
                    JS_FreeValue(ctx, num);
                    if ret {
                        return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                            flags as _,
                            c"negative index in typed array",
                        );
                    }
                    if !prop.is_int_type() {
                        // goto typed_array_oob
                        return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                            flags as _,
                            c"out-of-bound index in typed array",
                        );
                    }
                }

                let idx = __JS_AtomToUInt32(prop);
                /* if the typed array is detached, p->u.array.count = 0 */
                if idx >= (*p).u.array.count {
                    return ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                        flags as _,
                        c"out-of-bound index in typed array",
                    );
                }
                let prop_flags = get_prop_flags(
                    flags,
                    JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                );
                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0
                    || prop_flags != (JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
                {
                    return ctx
                        .as_mut()
                        .unwrap()
                        .JS_ThrowTypeErrorOrFalse(flags as _, c"invalid descriptor flags");
                }
                if flags & JS_PROP_HAS_VALUE != 0 {
                    return JS_SetPropertyValue(
                        ctx.as_mut().unwrap(),
                        this_obj,
                        JSValue::from_i32(idx as i32),
                        val.dup(),
                        flags,
                    );
                }

                return 1;
            }
        }

        break;
    }

    #[cfg(debug_assertions)]
    let _val_rc = val.ref_count();

    let r = JS_CreateProperty(ctx, p, prop, val, getter, setter, flags);

    #[cfg(debug_assertions)]
    if flags & JS_PROP_HAS_VALUE != 0 {
        if let Some(c) = val.ref_count() {
            debug_assert!(
                c > _val_rc.unwrap(),
                "before:{}, after:{}",
                _val_rc.unwrap(),
                c
            );
        }
    }

    r.map_or(-1, |b| b as i32)
}

/// shortcut to add or redefine a new property value.
/// Note: property value is consumed.
pub unsafe fn JS_DefinePropertyValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
    flags: u32,
) -> i32 {
    #[cfg(debug_assertions)]
    let _val_rc = val.ref_count();

    let ret = JS_DefineProperty(
        ctx,
        this_obj,
        prop,
        val, // will be duplicated inside
        JSValue::UNDEFINED,
        JSValue::UNDEFINED,
        flags
            | JS_PROP_HAS_VALUE
            | JS_PROP_HAS_CONFIGURABLE
            | JS_PROP_HAS_WRITABLE
            | JS_PROP_HAS_ENUMERABLE,
    );

    #[cfg(debug_assertions)]
    if let Some(c) = val.ref_count() {
        debug_assert!(
            c > _val_rc.unwrap(),
            "before:{}, after:{}",
            _val_rc.unwrap(),
            c
        );
    }

    // consume value
    JS_FreeValue(ctx, val);
    ret
}

pub unsafe fn JS_DefinePropertyValueStr(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: &CStr,
    val: JSValue,
    flags: u32,
) -> i32 {
    let atom = JS_NewAtom(ctx, prop);
    let ret = JS_DefinePropertyValue(ctx, this_obj, atom, val, flags);
    JS_FreeAtom(ctx, atom);
    ret
}

pub unsafe fn JS_DefinePropertyValueValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSValue,
    val: JSValue,
    flags: u32,
) -> i32 {
    let atom = JS_ValueToAtom(ctx, prop);
    JS_FreeValue(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        JS_FreeValue(ctx, val);
        return -1;
    }
    let ret = JS_DefinePropertyValue(ctx, this_obj, atom, val, flags);
    JS_FreeAtom(ctx, atom);
    return ret;
}

pub unsafe fn JS_CreateDataPropertyUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
    flags: u32,
) -> i32 {
    JS_DefinePropertyValueValue(
        ctx,
        this_obj,
        JSValue::from_i64(idx),
        val,
        flags | JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
    )
}

pub unsafe fn JS_DefinePropertyValueUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: u32,
    val: JSValue,
    flags: u32,
) -> i32 {
    JS_DefinePropertyValueValue(ctx, this_obj, JSValue::from_u32(idx), val, flags)
}

pub unsafe fn JS_DefinePropertyValueInt64(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
    flags: u32,
) -> i32 {
    JS_DefinePropertyValueValue(ctx, this_obj, JSValue::from_i64(idx), val, flags)
}

pub unsafe fn JS_DefinePropertyDesc(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    desc: JSValueConst,
    flags: u32,
) -> Result<bool, i32> {
    let mut d: JSPropertyDescriptor = std::mem::zeroed();
    js_obj_to_desc(ctx, &mut d, desc)?;

    let ret = JS_DefineProperty(ctx, obj, prop, d.value, d.getter, d.setter, d.flags | flags);
    js_free_desc(ctx, &mut d);

    Ok(ret != 0)
}

pub unsafe fn JS_HasProperty(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
) -> Result<bool, i32> {
    if JS_VALUE_GET_TAG(obj) != JSTag::Object as i32 {
        return Ok(false);
    }

    let mut p = JS_VALUE_GET_OBJ(obj);

    loop {
        if (*p).detailed.is_exotic() {
            let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
            if !em.is_null() && (*em).has_property.is_some() {
                // has_property can free the prototype
                let obj1 = JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));
                let ret = ((*em).has_property.unwrap())(ctx, obj1, prop);
                JS_FreeValue(ctx, obj1);

                return match ret {
                    -1 => Err(-1),
                    n => Ok(n != 0),
                };
            }
        }

        // JS_GetOwnPropertyInternal can free the prototype
        JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));
        let ret = JS_GetOwnPropertyInternal(ctx, std::ptr::null_mut(), p, prop);
        JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));

        match ret {
            -1 => {
                return Err(-1);
            }
            0 => {
                if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&(*p).class_id()) {
                    match JS_AtomIsNumericIndex(ctx, prop) {
                        -1 => {
                            return Err(-1);
                        }
                        0 => {}
                        _ => {
                            return Ok(false);
                        }
                    }
                }

                p = (*p).shape().unwrap().proto;
                if p.is_null() {
                    break;
                }
            }
            _ => {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Note: `val` is consumed
pub unsafe fn JS_SetPropertyValue(
    ctx: &mut JSContext,
    this_obj: JSValue,
    prop: JSValue,
    val: JSValue,
    flags: u32,
) -> i32 {
    if likely(
        JS_VALUE_GET_TAG(this_obj) == JSTag::Object as i32
            && JS_VALUE_GET_TAG(prop) == JSTag::Int as i32,
    ) {
        let mut d: f64 = 0.0;
        let mut v: i32 = 0;

        // 快速路径用于数组访问
        let p = JS_VALUE_GET_OBJ(this_obj);
        let idx = JS_VALUE_GET_INT(prop);

        let mut slow_path = false;
        let mut ta_out_of_bound = false;

        match (*p).class_id() {
            JS_CLASS_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // 快速路径用于向数组添加元素
                    if idx != (*p).u.array.count as i32
                        || !(*p).detailed.fast_array()
                        || !(*p).detailed.extensible()
                    {
                        slow_path = true;
                    } else {
                        // 检查原型链是否有数字属性
                        let mut p1 = (*p).shape().unwrap().proto;
                        while !p1.is_null() && !slow_path {
                            let sh1 = (*p1).shape().unwrap();
                            if (*p1).class_id() == JS_CLASS_ARRAY {
                                if !(*p1).detailed.fast_array() {
                                    slow_path = true;
                                }
                            } else if (*p1).class_id() == JS_CLASS_OBJECT {
                                if sh1.has_small_array_index {
                                    slow_path = true;
                                }
                            } else {
                                slow_path = true;
                            }
                            p1 = sh1.proto;
                        }

                        if !slow_path {
                            // 添加元素
                            return add_fast_array_element(ctx, p, val, flags);
                        }
                    }
                }

                if !slow_path {
                    set_value(ctx, (*p).u.array.u.values.offset(idx as isize), val);
                }
            }
            JS_CLASS_ARGUMENTS => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    slow_path = true;
                } else {
                    set_value(ctx, (*p).u.array.u.values.offset(idx as _), val);
                }
            }
            JS_CLASS_UINT8C_ARRAY => {
                if JS_ToUint8ClampFree(ctx, &mut v, val) != 0 {
                    return -1;
                }
                if unlikely(idx >= (*p).u.array.count as i32) {
                    ta_out_of_bound = true;
                } else {
                    *(*p).u.array.u.uint8_ptr.offset(idx as _) = v as u8;
                }
            }
            JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY => {
                v = if let Ok(n) = val.to_i32_free(ctx) {
                    n
                } else {
                    return -1;
                };
                if idx >= (*p).u.array.count as i32 {
                    ta_out_of_bound = true;
                } else {
                    *(*p).u.array.u.uint8_ptr.offset(idx as _) = v as u8;
                }
            }
            JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY => {
                v = if let Ok(n) = val.to_i32_free(ctx) {
                    n
                } else {
                    return -1;
                };
                if idx >= (*p).u.array.count as i32 {
                    ta_out_of_bound = true;
                } else {
                    *(*p).u.array.u.uint16_ptr.offset(idx as _) = v as u16;
                }
            }
            JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY => {
                v = if let Ok(n) = val.to_i32_free(ctx) {
                    n
                } else {
                    return -1;
                };
                if idx >= (*p).u.array.count as i32 {
                    ta_out_of_bound = true;
                } else {
                    *(*p).u.array.u.uint32_ptr.offset(idx as _) = v as u32;
                }
            }
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                unimplemented!()
            }
            JS_CLASS_FLOAT32_ARRAY => {
                if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
                    return -1;
                }
                if unlikely(idx >= (*p).u.array.count as i32) {
                    ta_out_of_bound = true;
                } else {
                    *(*p).u.array.u.float_ptr.offset(idx as _) = d as f32;
                }
            }
            JS_CLASS_FLOAT64_ARRAY => {
                if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
                    return -1;
                }
                if unlikely(idx >= (*p).u.array.count as i32) {
                    ta_out_of_bound = true;
                } else {
                    *(*p).u.array.u.double_ptr.offset(idx as _) = d;
                }
            }
            _ => {
                slow_path = true;
            }
        }

        if ta_out_of_bound {
            return 1;
        }

        if !slow_path {
            return 1;
        }
    }

    // slow_path 逻辑
    let atom = JS_ValueToAtom(ctx, prop);
    JS_FreeValue(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        JS_FreeValue(ctx, val);
        return -1;
    }
    let ret = JS_SetPropertyInternal(ctx, this_obj, atom, val, this_obj, flags);
    JS_FreeAtom(ctx, atom);
    ret
}

#[inline(always)]
pub unsafe fn JS_SetProperty(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
) -> i32 {
    JS_SetPropertyInternal(
        ctx.as_mut().unwrap(),
        this_obj,
        prop,
        val,
        this_obj,
        JS_PROP_THROW,
    )
}

pub unsafe fn JS_SetPropertyUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: u32,
    val: JSValue,
) -> i32 {
    JS_SetPropertyValue(
        ctx.as_mut().unwrap(),
        this_obj,
        JSValue::from_u32(idx),
        val,
        JS_PROP_THROW,
    )
}

pub unsafe fn JS_SetPropertyInt64(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
) -> i32 {
    if (idx as u64) <= i32::MAX as u64 {
        // fast path for fast arrays
        return JS_SetPropertyValue(
            ctx.as_mut().unwrap(),
            this_obj,
            JSValue::from_i32(idx as i32),
            val,
            JS_PROP_THROW,
        );
    }
    let prop = JS_NewAtomInt64(ctx, idx);
    if prop == JS_ATOM_NULL {
        JS_FreeValue(ctx, val);
        return -1;
    }

    let res = JS_SetProperty(ctx, this_obj, prop, val);
    JS_FreeAtom(ctx, prop);
    res
}

pub unsafe fn JS_SetPropertyStr(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: &CStr,
    val: JSValue,
) -> i32 {
    let atom = JS_NewAtom(ctx, prop);
    let ret = JS_SetPropertyInternal(ctx, this_obj, atom, val, this_obj, JS_PROP_THROW);
    JS_FreeAtom(ctx, atom);
    ret
}

/* return -1 in case of exception or TRUE or FALSE. Warning: 'val' is
freed by the function. 'flags' is a bitmask of JS_PROP_NO_ADD,
JS_PROP_THROW or JS_PROP_THROW_STRICT. If JS_PROP_NO_ADD is set,
the new property is not added and an error is raised. 'this_obj' is
the receiver. If obj != this_obj, then obj must be an object
(Reflect.set case). */
pub unsafe fn JS_SetPropertyInternal(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
    this_obj: JSValueConst,
    flags: u32,
) -> i32 {
    let err_read_only_prop = |ctx: *mut JSContext, val: JSValue| -> i32 {
        JS_FreeValue(ctx, val);
        return JS_ThrowTypeErrorReadOnly(ctx, flags, prop);
    };

    let err_typed_array_oob = |p1: *mut JSObject| -> i32 {
        /* must convert the argument even if out of bound access */
        if matches!(
            (*p1).class_id(),
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY
        ) {
            unimplemented!()
            // let mut v:i64;
            // if JS_ToBigInt64Free(ctx, &v, val)!=0{
            //     return -1;
            // }
        } else {
            let val = JS_ToNumberFree(ctx, val);
            JS_FreeValue(ctx, val);
            if JS_IsException(val) {
                return -1;
            }
        }
        return 1;
    };

    // returns Some(-1) for error, Some(1) for updated, Some(0) not updated. None for prop not found.
    // see label retry2
    let find_prop = |ctx: *mut JSContext,
                     p1: *mut JSObject,
                     this_obj: JSValue,
                     val: JSValue,
                     flags: u32|
     -> Option<i32> {
        let mut pr: *mut JSProperty = std::ptr::null_mut();

        loop {
            let prs = find_own_property(&mut pr, p1, prop);
            if !prs.is_null() {
                if (*prs).flags() & JS_PROP_TMASK == JS_PROP_GETSET {
                    return Some(call_setter(
                        ctx,
                        (*pr).u.getset.setter,
                        this_obj,
                        val,
                        flags,
                    ));
                } else if (*prs).flags() & JS_PROP_TMASK == JS_PROP_AUTOINIT {
                    /* Instantiate property and retry (potentially useless) */
                    if JS_AutoInitProperty(ctx, p1, prop, pr, prs) != 0 {
                        return Some(-1);
                    }
                } else if (*prs).flags() & JS_PROP_WRITABLE == 0 {
                    return Some(err_read_only_prop(ctx, val)); // if read only prop, error raised
                }
            } else {
                break; // not found
            }
        }

        None
    };

    // returns Some(-1) for error, Some(1) for updated, Some(0) not updated. None for prop not found.
    let find_prop_along_prototype = |ctx: *mut JSContext,
                                     target: *mut JSObject,
                                     mut proto: *mut JSObject,
                                     prop: JSAtom,
                                     flags: u32|
     -> Option<i32> {
        loop {
            if (*proto).detailed.is_exotic() {
                if (*proto).detailed.fast_array() {
                    if prop.is_int_type() {
                        let idx = __JS_AtomToUInt32(prop);
                        if idx < (*proto).u.array.count {
                            if unlikely(target == proto) {
                                return Some(JS_SetPropertyValue(
                                    ctx.as_mut().unwrap(),
                                    this_obj,
                                    JSValue::from_i32(idx as _),
                                    val,
                                    flags,
                                ));
                            } else {
                                break;
                            }
                        } else if (*proto).class_id() >= JS_CLASS_UINT8C_ARRAY
                            && (*proto).class_id() <= JS_CLASS_FLOAT64_ARRAY
                        {
                            return Some(err_typed_array_oob(proto));
                        }
                    } else if (*proto).class_id() >= JS_CLASS_UINT8C_ARRAY
                        && (*proto).class_id() <= JS_CLASS_FLOAT64_ARRAY
                    {
                        let ret = JS_AtomIsNumericIndex(ctx, prop);
                        if ret != 0 {
                            if ret < 0 {
                                JS_FreeValue(ctx, val);
                                return Some(-1);
                            }
                            return Some(err_typed_array_oob(proto));
                        }
                    }
                } else {
                    let em = (*ctx).rt().class_array[(*proto).class_id() as usize].exotic;
                    if !em.is_null() {
                        if let Some(cb) = (*em).set_property {
                            /* set_property can free the prototype */
                            let obj1 = JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, proto.cast()));
                            let ret = cb(ctx, obj1, prop, val, this_obj, flags as _);
                            JS_FreeValue(ctx, obj1);
                            JS_FreeValue(ctx, val);
                            return Some(ret);
                        }
                        if let Some(cb) = (*em).get_own_property {
                            /* get_own_property can free the prototype */
                            let obj1 = JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, proto.cast()));
                            let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                            let mut ret = cb(ctx, &mut desc, obj1, prop);
                            JS_FreeValue(ctx, obj1);
                            if ret < 0 {
                                JS_FreeValue(ctx, val);
                                return Some(ret);
                            }
                            if ret != 0 {
                                if desc.flags & JS_PROP_GETSET != 0 {
                                    let setter: *mut JSObject = if JS_IsUndefined(desc.setter) {
                                        std::ptr::null_mut()
                                    } else {
                                        JS_VALUE_GET_OBJ(desc.setter)
                                    };
                                    ret = call_setter(ctx, setter, this_obj, val, flags);
                                    JS_FreeValue(ctx, desc.getter);
                                    JS_FreeValue(ctx, desc.setter);
                                    return Some(ret);
                                } else {
                                    JS_FreeValue(ctx, desc.value);
                                    if desc.flags & JS_PROP_WRITABLE == 0 {
                                        return Some(err_read_only_prop(ctx, val));
                                    }

                                    if likely(target == proto) {
                                        let ret = JS_DefineProperty(
                                            ctx,
                                            this_obj,
                                            prop,
                                            val,
                                            JSValue::UNDEFINED,
                                            JSValue::UNDEFINED,
                                            JS_PROP_HAS_VALUE,
                                        );
                                        JS_FreeValue(ctx, val);
                                        return Some(ret);
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // upward for protortype
            proto = (*proto).shape().unwrap().proto;

            if proto.is_null() {
                break;
            }

            if let Some(r) = find_prop(ctx, proto, this_obj, val, flags) {
                return Some(r);
            }
        }

        None // not found
    };

    let p: *mut JSObject;
    let p1: *mut JSObject;
    let tag = JS_VALUE_GET_NORM_TAG(this_obj);

    if unlikely(tag != JSTag::Object) {
        if JS_VALUE_GET_TAG(obj) == JSTag::Object as i32 {
            p = std::ptr::null_mut();
            p1 = JS_VALUE_GET_OBJ(obj);
        } else {
            match tag {
                JSTag::Null => {
                    JS_FreeValue(ctx, val);
                    JS_ThrowTypeErrorAtom(ctx, c"cannot set property '%s' of null", prop);
                    return -1;
                }
                JSTag::Undefined => {
                    JS_FreeValue(ctx, val);
                    JS_ThrowTypeErrorAtom(ctx, c"cannot set property '%s' of undefined", prop);
                    return -1;
                }
                _ => {
                    /* even on a primitive type we can have setters on the prototype */
                    p = std::ptr::null_mut();
                    p1 = JS_VALUE_GET_OBJ(JS_GetPrototypePrimitive(ctx, obj));
                }
            }
        }
        if !p1.is_null() {
            if let Some(r) = find_prop(ctx, p1, this_obj, val, flags) {
                return r;
            }
        }
    } else {
        p = JS_VALUE_GET_OBJ(this_obj);
        p1 = JS_VALUE_GET_OBJ(obj);
        debug_assert!(!p1.is_null());
        debug_assert!(!p.is_null());

        if unlikely(p != p1) {
            if let Some(r) = find_prop(ctx, p1, this_obj, val, flags) {
                return r;
            }
        } else {
            /* fast path if obj == this_obj */
            let mut retry_flag = true;
            while retry_flag {
                retry_flag = false;

                let mut pr: *mut JSProperty = std::ptr::null_mut();
                let prs = find_own_property(&mut pr, p1, prop);
                if !prs.is_null() {
                    if likely(
                        (*prs).flags() & (JS_PROP_TMASK | JS_PROP_WRITABLE | JS_PROP_LENGTH)
                            == JS_PROP_WRITABLE,
                    ) {
                        /* fast case */
                        set_value(ctx, &mut (*pr).u.value, val);
                        return 1;
                    } else if (*prs).flags() & JS_PROP_LENGTH != 0 {
                        assert!((*p).class_id() == JS_CLASS_ARRAY);
                        assert_eq!(prop, JS_ATOM_length);
                        return set_array_length(ctx, p, val, flags);
                    } else if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_GETSET {
                        return call_setter(ctx, (*pr).u.getset.setter, this_obj, val, flags);
                    } else if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
                        if (*p).class_id() == JS_CLASS_MODULE_NS {
                            // goto read_only_prop; // C2RS: 转换为break到标签
                        }
                        set_value(ctx, (*(*pr).u.var_ref).pvalue, val);
                        return 1;
                    } else if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_AUTOINIT {
                        if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                            JS_FreeValue(ctx, val);
                            return -1;
                        }
                        retry_flag = true; // 模拟goto retry
                    }
                }
            }
        }
    }

    if !p1.is_null() {
        if let Some(r) = find_prop_along_prototype(ctx, p, p1, prop, flags) {
            return r;
        }
    }

    //
    // Property not found, add it to the object
    //

    if unlikely(flags & JS_PROP_NO_ADD != 0) {
        JS_FreeValue(ctx, val);
        JS_ThrowReferenceErrorNotDefined(ctx, prop);
        return -1;
    }
    if unlikely(p.is_null()) {
        JS_FreeValue(ctx, val);
        return ctx
            .as_mut()
            .unwrap()
            .JS_ThrowTypeErrorOrFalse(flags as _, c"not an object");
    }
    if unlikely(!(*p).detailed.extensible()) {
        JS_FreeValue(ctx, val);
        return ctx
            .as_mut()
            .unwrap()
            .JS_ThrowTypeErrorOrFalse(flags as _, c"object is not extensible");
    }

    macro_rules! generic_create_prop {
        ($p:expr ) => {
            let ret = JS_CreateProperty(
                ctx,
                $p,
                prop,
                val,
                JSValue::UNDEFINED,
                JSValue::UNDEFINED,
                flags
                    | JS_PROP_HAS_VALUE
                    | JS_PROP_HAS_ENUMERABLE
                    | JS_PROP_HAS_WRITABLE
                    | JS_PROP_HAS_CONFIGURABLE
                    | JS_PROP_C_W_E,
            );
            JS_FreeValue(ctx, val);

            return match ret {
                Ok(b) => b as i32,
                Err(_) => -1,
            }
        };
    }

    // let generic_create_prop =
    //     |ctx: *mut JSContext, p: *mut JSObject, prop: JSAtom, val: JSValue, flags: u32| -> i32 {
    //         let ret = JS_CreateProperty(
    //             ctx,
    //             p,
    //             prop,
    //             val,
    //             JSValue::UNDEFINED,
    //             JSValue::UNDEFINED,
    //             flags
    //                 | JS_PROP_HAS_VALUE
    //                 | JS_PROP_HAS_ENUMERABLE
    //                 | JS_PROP_HAS_WRITABLE
    //                 | JS_PROP_HAS_CONFIGURABLE
    //                 | JS_PROP_C_W_E,
    //         );
    //         JS_FreeValue(ctx, val);
    //         return ret;
    //     };

    if likely(p == JS_VALUE_GET_OBJ(obj)) {
        if (*p).detailed.is_exotic() {
            if (*p).class_id() == JS_CLASS_ARRAY && (*p).detailed.fast_array() && prop.is_int_type()
            {
                let idx = __JS_AtomToUInt32(prop);
                if idx == (*p).u.array.count {
                    /* fast case */
                    return add_fast_array_element(ctx, p, val, flags);
                } else {
                    generic_create_prop!(p);
                }
            } else {
                generic_create_prop!(p);
            }
        } else {
            match p
                .as_mut()
                .unwrap()
                .add_property(ctx.as_mut().unwrap(), prop, JS_PROP_C_W_E)
            {
                Ok(pr) => {
                    (*pr).u.value = val;
                    return 1;
                }
                Err(_) => {
                    JS_FreeValue(ctx, val);
                    return -1;
                }
            }
        }
    } else {
        /* generic case: modify the property in this_obj if it already exists */
        let mut desc: JSPropertyDescriptor = std::mem::zeroed();
        let mut ret = JS_GetOwnPropertyInternal(ctx, &mut desc, p, prop);
        if ret < 0 {
            JS_FreeValue(ctx, val);
            return ret;
        }
        if ret != 0 {
            if desc.flags & JS_PROP_GETSET != 0 {
                JS_FreeValue(ctx, desc.getter);
                JS_FreeValue(ctx, desc.setter);
                JS_FreeValue(ctx, val);
                return ctx
                    .as_mut()
                    .unwrap()
                    .JS_ThrowTypeErrorOrFalse(flags as _, c"setter is forbidden");
            } else {
                JS_FreeValue(ctx, desc.value);
                if desc.flags & JS_PROP_WRITABLE == 0 || (*p).class_id() == JS_CLASS_MODULE_NS {
                    return err_read_only_prop(ctx, val);
                }
            }
            ret = JS_DefineProperty(
                ctx,
                this_obj,
                prop,
                val,
                JSValue::UNDEFINED,
                JSValue::UNDEFINED,
                JS_PROP_HAS_VALUE,
            );
            JS_FreeValue(ctx, val);
            return ret;
        } else {
            generic_create_prop!(p);
        }
    }
}

/* 返回 -1、FALSE 或 TRUE。如果不可配置或无效对象则返回 FALSE。出现异常时返回 -1。
flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT */
pub unsafe fn JS_DeleteProperty(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    flags: u32,
) -> Result<bool, i32> {
    let obj1 = JS_ToObject(ctx, obj);
    if JS_IsException(obj1) {
        return Err(-1);
    }

    let p = JS_VALUE_GET_OBJ(obj1);
    let res = p.as_mut().unwrap().delete_property(ctx, prop);
    JS_FreeValue(ctx, obj1);
    if res < 0 {
        return Err(-1);
    } else if res != 0 {
        return Ok(true);
    } else if flags & JS_PROP_THROW != 0
        || ((flags & JS_PROP_THROW_STRICT) != 0 && is_strict_mode(ctx))
    {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"could not delete property");

        Err(-1)
    } else {
        Ok(false)
    }
}

pub unsafe fn JS_DeletePropertyInt64(
    ctx: *mut JSContext,
    obj: JSValueConst,
    idx: i64,
    flags: u32,
) -> Result<bool, i32> {
    if (idx as u64) <= JS_ATOM_MAX_INT as u64 {
        // fast path for fast arrays
        return JS_DeleteProperty(ctx, obj, JSAtom::from_u32(idx as u32), flags);
    }

    let prop = JS_NewAtomInt64(ctx, idx);
    if prop == JS_ATOM_NULL {
        return Err(-1);
    }

    let res = JS_DeleteProperty(ctx, obj, prop, flags);
    JS_FreeAtom(ctx, prop);
    res
}

/// Descriptor config object to JSPropertyDescriptor
pub unsafe fn js_obj_to_desc(
    ctx: *mut JSContext,
    d: *mut JSPropertyDescriptor,
    desc: JSValueConst,
) -> Result<(), i32> {
    if !JS_IsObject(desc) {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let mut val: JSValue = JSValue::UNDEFINED;
    let mut getter: JSValue = JSValue::UNDEFINED;
    let mut setter: JSValue = JSValue::UNDEFINED;
    let mut flags: u32 = 0;

    macro_rules! fail {
        () => {
            JS_FreeValue(ctx, val);
            JS_FreeValue(ctx, getter);
            JS_FreeValue(ctx, setter);
            return Err(-1);
        };
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_configurable).unwrap_or(true) {
        let prop = JS_GetProperty(ctx, desc, JS_ATOM_configurable);
        if JS_IsException(prop) {
            fail!();
        }
        flags |= JS_PROP_HAS_CONFIGURABLE;
        if JS_ToBoolFree(ctx, prop).unwrap_or(true) {
            flags |= JS_PROP_CONFIGURABLE;
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_writable).unwrap_or(true) {
        let prop = JS_GetProperty(ctx, desc, JS_ATOM_writable);
        if JS_IsException(prop) {
            fail!();
        }
        flags |= JS_PROP_HAS_WRITABLE;
        if JS_ToBoolFree(ctx, prop).unwrap_or(true) {
            flags |= JS_PROP_WRITABLE;
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_enumerable).unwrap_or(true) {
        let prop = JS_GetProperty(ctx, desc, JS_ATOM_enumerable);
        if JS_IsException(prop) {
            fail!();
        }
        flags |= JS_PROP_HAS_ENUMERABLE;
        if JS_ToBoolFree(ctx, prop).unwrap_or(true) {
            flags |= JS_PROP_ENUMERABLE;
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_value).unwrap_or(true) {
        flags |= JS_PROP_HAS_VALUE;
        val = JS_GetProperty(ctx, desc, JS_ATOM_value);
        if JS_IsException(val) {
            fail!();
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_get).unwrap_or(true) {
        flags |= JS_PROP_HAS_GET;
        getter = JS_GetProperty(ctx, desc, JS_ATOM_get);
        if JS_IsException(getter) || !(JS_IsUndefined(getter) || JS_IsFunction(ctx, getter)) {
            ctx.as_mut().unwrap().JS_ThrowTypeError(c"invalid getter");
            fail!();
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_set).unwrap_or(true) {
        flags |= JS_PROP_HAS_SET;
        setter = JS_GetProperty(ctx, desc, JS_ATOM_set);
        if JS_IsException(setter) || !(JS_IsUndefined(setter) || JS_IsFunction(ctx, setter)) {
            ctx.as_mut().unwrap().JS_ThrowTypeError(c"invalid setter");
            fail!();
        }
    }

    if (flags & (JS_PROP_HAS_SET | JS_PROP_HAS_GET)) != 0
        && (flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE)) != 0
    {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"cannot have setter/getter and value or writable");
        fail!();
    }

    (*d).flags = flags;
    (*d).value = val;
    (*d).getter = getter;
    (*d).setter = setter;

    Ok(())
}

impl JSObject {
    /// Grow prop values list to expected new length, extended elements will be initialized to value `undefined`.
    /// If new_length is less than or equal to current length, nothing to do.
    pub(super) unsafe fn grow_prop_value_slots(&mut self, new_length: usize) -> Result<(), i32> {
        if new_length > self.prop_values.len() {
            let grow = new_length - self.prop_values.len();
            if self.prop_values.try_reserve_exact(grow).is_err() {
                log::error!("enlarge object prop values slot failed");
                return Err(-1);
            }

            // init extended elements to `undefined` jsvalue
            self.prop_values.extend(
                std::iter::repeat(JSProperty {
                    u: JSPropertyUnion {
                        value: JSValue::UNDEFINED,
                    },
                })
                .take(grow),
            );
        }

        Ok(())
    }

    pub unsafe fn js_update_property_flags(
        &mut self,
        ctx: *mut JSContext,
        pprs: *mut *mut JSShapeProperty,
        flags: u32,
    ) -> i32 {
        if flags != (*(*pprs)).flags() {
            if self.js_shape_prepare_update(ctx, pprs) != 0 {
                return -1;
            }
            (*(*pprs)).set_flags(flags);
        }
        return 0;
    }

    /* Note: the property value is initialized to undefined. Returns Err if memory error. */
    pub unsafe fn add_property(
        &mut self,
        ctx: &mut JSContext,
        prop: JSAtom,
        prop_flags: u32,
    ) -> Result<*mut JSProperty, i32> {
        let curr_sh = self.shape.as_mut().unwrap();

        if curr_sh.is_hashed {
            /* try to find an existing shape */
            match find_hashed_shape_prop(ctx.rt_mut(), curr_sh, prop, prop_flags) {
                Some(new_sh) => {
                    /* matching shape found: use it */
                    let new_len = (*new_sh).props.len();
                    if new_len != curr_sh.props.len() {
                        debug_assert!(new_len > curr_sh.props.len());
                        self.grow_prop_value_slots(new_len)?;
                    }

                    self.shape = js_dup_shape(new_sh);
                    js_free_shape(ctx.rt_mut(), curr_sh);

                    return Ok(self.prop_values.get_mut(new_len - 1).unwrap());
                }
                None => {
                    /* no matching found, clone current for update */
                    let new_sh = js_clone_shape(ctx, curr_sh)?;

                    // hash the new shape
                    (*new_sh).is_hashed = true;
                    js_shape_hash_link(ctx.rt_mut(), new_sh);

                    js_free_shape(ctx.rt_mut(), curr_sh);
                    self.shape = new_sh;
                }
            };
        }

        // for unhashed or cloned shape: add shape prop
        debug_assert_eq!(self.shape().unwrap().header.ref_count, 1);

        if self
            .shape
            .as_mut()
            .unwrap()
            .add_shape_property(ctx, Some(self), prop, prop_flags)
            != 0
        {
            return Err(-1);
        }

        let new_len = self.shape().unwrap().props.len();
        Ok(self.prop_values.get_mut(new_len - 1).unwrap())
    }

    pub unsafe fn delete_property(&mut self, ctx: *mut JSContext, atom: JSAtom) -> i32 {
        let sh = self.shape_mut().unwrap();
        let mut idx_and_flags: Option<(usize, u32)> = None;

        if let Some((idx, _, prs)) = sh.props.get_full_mut(&atom) {
            let flags = prs.flags();
            if flags & JS_PROP_CONFIGURABLE == 0 {
                return 0;
            }

            let mut prs = prs as *mut JSShapeProperty;
            if self.js_shape_prepare_update(ctx, std::ptr::addr_of_mut!(prs)) != 0 {
                return -1;
            }

            // self.shape could be changed in js_shape_prepare_update()

            idx_and_flags = Some((idx, flags));
        }

        if let Some((idx, flags)) = idx_and_flags {
            // remove and free property value
            let mut val = self.prop_values.remove(idx);
            free_property((*ctx).rt_mut(), &mut val, flags);

            // remove shape property of current self.shape
            let prs = self.shape_mut().unwrap().props.shift_remove(&atom).unwrap();
            JS_FreeAtom(ctx, prs.atom);

            return 1;
        }

        loop {
            if self.detailed.is_exotic() {
                if self.detailed.fast_array() {
                    let mut idx: u32 = 0;
                    if JS_AtomIsArrayIndex(ctx, &mut idx, atom) && idx < self.u.array.count {
                        if matches!(self.class_id(), JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS) {
                            if idx == self.u.array.count - 1 {
                                JS_FreeValue(ctx, *self.u.array.u.values.offset(idx as isize));
                                self.u.array.count = idx;
                                return 1;
                            }
                            if convert_fast_array_to_array(ctx, self) != 0 {
                                return -1;
                            }

                            continue; // C2RS: goto redo simulated with continue
                        } else {
                            return 0;
                        }
                    }
                } else {
                    let class_id = self.class_id();
                    let em = (*ctx).rt().class_array[class_id as usize].exotic;
                    if !em.is_null() {
                        if let Some(cb) = (*em).delete_property {
                            return cb(
                                ctx,
                                JS_MKPTR!(JSTag::Object, self as *mut JSObject as _),
                                atom,
                            );
                        }
                    }
                }
            }

            break;
        }

        return 1;
    }
}

// /* Note: the property value is not initialized. Returns Err if memory error. */
// pub unsafe fn add_property(
//     ctx: &mut JSContext,
//     p: &mut JSObject,
//     prop: JSAtom,
//     prop_flags: u32,
// ) -> Result<*mut JSProperty, i32> {
//     let sh = p.shape.as_mut().unwrap();

//     if sh.is_hashed {
//         /* try to find an existing shape */
//         match find_hashed_shape_prop(ctx.rt.as_mut().unwrap(), sh, prop, prop_flags) {
//             Some(new_sh) => {
//                 /* matching shape found: use it */
//                 /*  the property array may need to be resized */
//                 if (*new_sh).prop_size != sh.prop_size {
//                     let new_prop = js_realloc(
//                         ctx,
//                         p.prop.cast(),
//                         std::mem::size_of::<JSProperty>() * (*new_sh).prop_size as usize,
//                     );
//                     if new_prop.is_null() {
//                         return Err(-1);
//                     }
//                     p.prop = new_prop.cast();
//                 }
//                 p.shape = js_dup_shape(new_sh);
//                 js_free_shape(ctx.rt, p.shape.as_mut().unwrap());

//                 return Ok(p.prop.offset((*new_sh).prop_count as isize - 1));
//             }
//             None => {
//                 /* if the shape is shared, clone it */
//                 let new_sh = sh.clone(ctx)?; //js_clone_shape(ctx, sh)?;
//                 let raw_sh = Box::into_raw(new_sh);
//                 /* hash the cloned shape */
//                 (*raw_sh).is_hashed = true;
//                 js_shape_hash_link(ctx.rt.as_mut().unwrap(), raw_sh);
//                 js_free_shape(ctx.rt, p.shape.as_mut().unwrap());
//                 p.shape = raw_sh;
//             }
//         };
//     }

//     debug_assert_eq!((*p.shape).header.ref_count, 1);

//     if add_shape_property(ctx, &mut p.shape, p, prop, prop_flags) != 0 {
//         Err(-1)
//     } else {
//         Ok(p.prop.add((*p.shape).prop_count as usize - 1))
//     }
// }

// pub unsafe fn delete_property(ctx: *mut JSContext, p: *mut JSObject, atom: JSAtom) -> i32 {
//     let mut sh = (*p).shape;
//     let mut prop = get_shape_prop(sh);

//     let h1 = (atom as isize) & (*sh).prop_hash_mask as isize;
//     let mut h = *prop_hash_end(sh).offset(-h1 - 1);
//     let mut lpr = std::ptr::null_mut::<JSShapeProperty>();

//     loop {
//         while h != 0 {
//             let mut pr = prop.offset(h as isize - 1);

//             if likely((*pr).atom == atom) {
//                 if (*pr).flags() & JS_PROP_CONFIGURABLE == 0 {
//                     return 0;
//                 }

//                 let lpr_idx = if !lpr.is_null() {
//                     lpr.offset_from(get_shape_prop(sh))
//                 } else {
//                     0
//                 };

//                 if js_shape_prepare_update(ctx, p, &mut pr) != 0 {
//                     return -1;
//                 }
//                 sh = (*p).shape;
//                 prop = get_shape_prop(sh);

//                 if !lpr.is_null() {
//                     lpr = prop.offset(lpr_idx);
//                     (*lpr).set_hash_next((*pr).hash_next());
//                 } else {
//                     *prop_hash_end(sh).offset(-h1 - 1) = (*pr).hash_next();
//                 }

//                 (*sh).deleted_prop_count += 1;

//                 let pr1 = (*p).prop.offset(h as isize - 1);
//                 free_property((*ctx).rt, pr1, (*pr).flags());
//                 JS_FreeAtom(ctx, (*pr).atom);
//                 (*pr).set_flags(0);
//                 (*pr).atom = JS_ATOM_NULL;
//                 (*pr1).u.value = JSValue::UNDEFINED;

//                 if (*sh).deleted_prop_count >= 8
//                     && (*sh).deleted_prop_count >= (*sh).prop_count as i32 / 2
//                 {
//                     compact_properties(ctx, p);
//                 }
//                 return 1;
//             }
//             lpr = pr;
//             h = (*pr).hash_next();
//         }

//         if (*p).detailed.is_exotic() {
//             if (*p).detailed.fast_array() {
//                 let mut idx: u32 = 0;
//                 if JS_AtomIsArrayIndex(ctx, &mut idx, atom) && idx < (*p).u.array.count {
//                     if (*p).class_id() == JS_CLASS_ARRAY
//                         || (*p).class_id() == JS_CLASS_ARGUMENTS
//                     {
//                         if idx == (*p).u.array.count - 1 {
//                             JS_FreeValue(ctx, *(*p).u.array.u.values.offset(idx as isize));
//                             (*p).u.array.count = idx;
//                             return 1;
//                         }
//                         if convert_fast_array_to_array(ctx, p) != 0 {
//                             return -1;
//                         }

//                         continue; // C2RS: goto redo simulated with continue
//                     } else {
//                         return 0;
//                     }
//                 }
//             } else {
//                 let class_id = (*p).class_id();
//                 let em = (*(*(*ctx).rt).class_array.offset(class_id as _)).exotic;
//                 if !em.is_null() {
//                     if let Some(cb) = (*em).delete_property {
//                         return cb(ctx, JS_MKPTR!(JS_TAG_OBJECT, p.cast()), atom);
//                     }
//                 }
//             }
//         }

//         return 1;
//     }
// }

// // remove the deleted properties.
// pub unsafe fn compact_properties(ctx: *mut JSContext, p: *mut JSObject) -> i32 {
//     let mut sh = (*p).shape;
//     debug_assert!(!(*sh).is_hashed);
//     debug_assert!(sh.is_aligned());

//     let new_size = std::cmp::max(
//         JS_PROP_INITIAL_SIZE,
//         (*sh).prop_count as u32 - (*sh).deleted_prop_count as u32,
//     );
//     assert!(new_size <= (*sh).prop_size as u32);

//     let mut new_hash_size = (*sh).prop_hash_mask + 1;
//     while (new_hash_size / 2) >= new_size {
//         new_hash_size = new_hash_size / 2;
//     }
//     let new_hash_mask = new_hash_size - 1;

//     // resize the hash table and the properties
//     let old_sh = sh;

//     // sh_alloc memory layou: prop_hash array + JSShape + JSProperty array
//     let sh_alloc = js_malloc(ctx, get_shape_size(new_hash_size as _, new_size as usize));
//     if sh_alloc.is_null() {
//         return -1;
//     }

//     sh = get_shape_from_alloc(sh_alloc.cast(), new_hash_size as _);
//     list_del(&mut (*old_sh).header.link);
//     std::ptr::copy_nonoverlapping(old_sh, sh, std::mem::size_of::<JSShape>());
//     list_add_tail(&mut (*sh).header.link, &mut (*(*ctx).rt).gc_obj_list);

//     // Initialize new hash table
//     let hash_start = prop_hash_end(sh).offset(-(new_hash_size as isize));
//     std::ptr::write_bytes(
//         hash_start as *mut u8,
//         0,
//         std::mem::size_of::<u32>() * new_hash_size as usize,
//     );

//     let mut old_pr = (*old_sh).prop.as_mut_ptr();
//     let mut pr = (*sh).prop.as_mut_ptr();
//     let prop = (*p).prop;
//     let mut j = 0;

//     for i in 0..(*sh).prop_count {
//         if (*old_pr).atom != JS_ATOM_NULL {
//             (*pr).atom = (*old_pr).atom;
//             (*pr).set_flags((*old_pr).flags());
//             let h = ((*old_pr).atom as usize) & (new_hash_mask as usize);
//             let hash_entry = prop_hash_end(sh).offset(-(h as isize + 1));
//             (*pr).set_hash_next(*hash_entry);
//             *hash_entry = j + 1;
//             *prop.offset(j as isize) = *prop.offset(i as isize);
//             j += 1;
//             pr = pr.offset(1);
//         }
//         old_pr = old_pr.offset(1);
//     }
//     assert_eq!(
//         j,
//         ((*sh).prop_count as u32 - (*sh).deleted_prop_count as u32)
//     );

//     (*sh).prop_hash_mask = new_hash_mask;
//     (*sh).prop_size = new_size as i32;
//     (*sh).deleted_prop_count = 0;
//     (*sh).prop_count = j as i32;

//     (*p).shape = sh;
//     js_free(ctx, get_alloc_from_shape(old_sh).cast());

//     // reduce the size of the object properties
//     let new_prop = js_realloc(
//         ctx,
//         (*p).prop as *mut _,
//         std::mem::size_of::<JSProperty>() * new_size as usize,
//     );
//     if !new_prop.is_null() {
//         (*p).prop = new_prop.cast();
//     }

//     0
// }

pub unsafe fn free_property(rt: *mut JSRuntime, pr: *mut JSProperty, prop_flags: u32) {
    if unlikely(prop_flags & JS_PROP_TMASK != 0) {
        match prop_flags & JS_PROP_TMASK {
            JS_PROP_GETSET => {
                if !(*pr).u.getset.getter.is_null() {
                    JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()));
                }
                if !(*pr).u.getset.setter.is_null() {
                    JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()));
                }
            }
            JS_PROP_VARREF => {
                if let Some(vr) = Box::from_raw((*pr).u.var_ref).free(rt) {
                    std::mem::forget(vr);
                }
            }
            JS_PROP_AUTOINIT => {
                js_autoinit_free(rt, pr);
            }
            _ => {}
        }
    } else {
        JS_FreeValueRT(rt, (*pr).u.value);
    }
}

#[inline(always)]
pub unsafe fn find_own_property1(p: *mut JSObject, atom: JSAtom) -> *mut JSShapeProperty {
    (*p).shape_mut()
        .and_then(|sh| sh.props.get_mut(&atom))
        .map_or(std::ptr::null_mut(), |p| p as *mut JSShapeProperty)
}

#[inline(always)]
pub unsafe fn find_own_property(
    ppr: *mut *mut JSProperty,
    p: *mut JSObject,
    atom: JSAtom,
) -> *mut JSShapeProperty {
    if let Some((idx, _, prs)) = (*p).shape_mut().unwrap().props.get_full_mut(&atom) {
        *ppr = (*p).prop_values.get_mut(idx).unwrap();
        prs
    } else {
        *ppr = std::ptr::null_mut();
        std::ptr::null_mut()
    }
}

/* compute the property flags. For each flag: (JS_PROP_HAS_x forces
   it, otherwise def_flags is used)
   Note: makes assumption about the bit pattern of the flags
*/
#[inline(always)]
pub fn get_prop_flags(flags: u32, def_flags: u32) -> u32 {
    let mask = (flags >> JS_PROP_HAS_SHIFT) & JS_PROP_C_W_E;
    (flags & mask) | (def_flags & !mask)
}

// return FALSE if not OK
pub unsafe fn check_define_prop_flags(prop_flags: u32, flags: u32) -> bool {
    let has_accessor;
    let is_getset;

    if (prop_flags & JS_PROP_CONFIGURABLE) == 0 {
        if (flags & (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE))
            == (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE)
        {
            return false;
        }
        if (flags & JS_PROP_HAS_ENUMERABLE) != 0
            && (flags & JS_PROP_ENUMERABLE) != (prop_flags & JS_PROP_ENUMERABLE)
        {
            return false;
        }
    }
    if (flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)) != 0
    {
        if (prop_flags & JS_PROP_CONFIGURABLE) == 0 {
            has_accessor = (flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET)) != 0;
            is_getset = (prop_flags & JS_PROP_TMASK) == JS_PROP_GETSET;
            if has_accessor != is_getset {
                return false;
            }
            if !has_accessor && !is_getset && (prop_flags & JS_PROP_WRITABLE) == 0 {
                /* not writable: cannot set the writable bit */
                if (flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE))
                    == (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
                {
                    return false;
                }
            }
        }
    }
    true
}

#[inline(always)]
pub unsafe fn JS_GetProperty(ctx: *mut JSContext, this_obj: JSValueConst, prop: JSAtom) -> JSValue {
    JS_GetPropertyInternal(ctx, this_obj, prop, this_obj, false)
}

/* warning: 'prs' is reallocated after it */
pub unsafe fn JS_AutoInitProperty(
    ctx: *mut JSContext,
    p: *mut JSObject,
    prop: JSAtom,
    pr: *mut JSProperty,
    mut prs: *mut JSShapeProperty,
) -> i32 {
    if p.as_mut().unwrap().js_shape_prepare_update(ctx, &mut prs) != 0 {
        return -1;
    }

    let realm = js_autoinit_get_realm(pr);
    let func = js_autoinit_func_table[js_autoinit_get_id(pr) as usize];

    /* 'func' shall not modify the object properties 'pr' */
    let val = func(realm, p, prop, (*pr).u.init.opaque);

    js_autoinit_free((*ctx).rt_mut(), pr);

    // clear AUTO INIT flag
    let f = (*prs).flags() & (!JS_PROP_TMASK);
    (*prs).set_flags(f);

    if JS_IsException(val) {
        (*pr).u.value = JSValue::UNDEFINED;
        -1
    } else {
        (*pr).u.value = val;
        0
    }
}

pub unsafe fn JS_DefineAutoInitProperty(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    id: JSAutoInitIDEnum,
    opaque: *mut std::ffi::c_void,
    flags: u32,
) -> Result<bool, i32> {
    if JS_VALUE_GET_TAG(this_obj) != JSTag::Object as i32 {
        return Ok(false);
    }

    let p = JS_VALUE_GET_OBJ(this_obj).as_mut().unwrap();

    let mut pr = std::ptr::null_mut::<JSProperty>();
    if !find_own_property(std::ptr::addr_of_mut!(pr), p, prop).is_null() {
        // property already exists
        unreachable!();
    }

    // Specialized CreateProperty
    let pr = p
        .add_property(
            ctx.as_mut().unwrap(),
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_AUTOINIT,
        )?
        .as_mut()
        .unwrap();

    pr.u.init.realm_and_id = JS_DupContext(ctx) as usize;
    debug_assert!(((*pr).u.init.realm_and_id & 3) == 0);

    pr.u.init.realm_and_id |= id as usize; // id: 0,1,2
    debug_assert!(matches!((*pr).u.init.realm_and_id & 3, 0 | 1 | 2));

    pr.u.init.opaque = opaque;

    Ok(true)
}

/// returned value has its ref_count increased by 1.
pub unsafe fn JS_GetPropertyInternal(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    this_obj: JSValueConst,
    throw_ref_error: bool,
) -> JSValue {
    let mut p = match JS_VALUE_GET_TAG(obj) {
        JS_TAG_OBJECT => JS_VALUE_GET_OBJ(obj),
        JS_TAG_NULL => {
            return JS_ThrowTypeErrorAtom(ctx, c"cannot read property '%s' of null", prop);
        }
        JS_TAG_UNDEFINED => {
            return JS_ThrowTypeErrorAtom(ctx, c"cannot read property '%s' of undefined", prop);
        }
        JS_TAG_EXCEPTION => {
            return JSValue::EXCEPTION;
        }
        JS_TAG_STRING => {
            let p1 = JS_VALUE_GET_STRING(obj);
            if prop.is_int_type() {
                let idx = __JS_AtomToUInt32(prop);
                if idx < (*p1).len() as u32 {
                    let ch = (*p1).content().chars().nth(idx as usize).unwrap();
                    return (*ctx).create_string_from_char(ch);
                }
            } else if prop == JS_ATOM_length {
                return JSValue::from_i32((*p1).len() as i32);
            }

            // cannot raise an exception
            JS_VALUE_GET_OBJ(JS_GetPrototypePrimitive(ctx, obj))
        }
        _ => {
            // cannot raise an exception
            JS_VALUE_GET_OBJ(JS_GetPrototypePrimitive(ctx, obj))
        }
    };

    if p.is_null() {
        return JSValue::UNDEFINED;
    }

    loop {
        let mut pr: *mut JSProperty = std::ptr::null_mut();
        let prs = find_own_property(&mut pr, p, prop);

        if !prs.is_null() {
            match (*prs).flags() & JS_PROP_TMASK {
                JS_PROP_GETSET => {
                    return if unlikely((*pr).u.getset.getter.is_null()) {
                        JSValue::UNDEFINED
                    } else {
                        let func: JSValue = JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast());
                        /* Note: the field could be removed in the getter */
                        JS_CallFree(ctx, func.dup(), this_obj, 0, std::ptr::null_mut())
                    };
                }
                JS_PROP_VARREF => {
                    let val = *(*pr).u.var_ref.as_ref().unwrap().pvalue;
                    return if unlikely(JS_IsUninitialized(val)) {
                        JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom)
                    } else {
                        val.dup()
                    };
                }
                JS_PROP_AUTOINIT => {
                    // Instantiate property and retry
                    if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                        return JSValue::EXCEPTION;
                    } else {
                        continue;
                    }
                }
                0 => {
                    return (*pr).u.value.dup();
                }
                _ => unreachable!(),
            }
        }

        if unlikely((*p).detailed.is_exotic()) {
            // exotic behaviors
            if (*p).detailed.fast_array() {
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < (*p).u.array.count {
                        /* we avoid duplicating the code */
                        return JS_GetPropertyUint32(ctx, JS_MKPTR!(JSTag::Object, p.cast()), idx);
                    } else if (*p).class_id() >= JS_CLASS_UINT8C_ARRAY
                        && (*p).class_id() <= JS_CLASS_FLOAT64_ARRAY
                    {
                        return JSValue::UNDEFINED;
                    }
                } else if (*p).class_id() >= JS_CLASS_UINT8C_ARRAY
                    && (*p).class_id() <= JS_CLASS_FLOAT64_ARRAY
                {
                    let ret = JS_AtomIsNumericIndex(ctx, prop);
                    if ret != 0 {
                        if ret < 0 {
                            return JSValue::EXCEPTION;
                        }
                        return JSValue::UNDEFINED;
                    }
                }
            } else {
                let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
                if !em.is_null() {
                    if (*em).get_property.is_some() {
                        /* XXX: should pass throw_ref_error */
                        /* Note: if 'p' is a prototype, it can be
                        freed in the called function */
                        let obj1 = JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));
                        let retval = (*em).get_property.unwrap()(ctx, obj1, prop, this_obj);
                        JS_FreeValue(ctx, obj1);
                        return retval;
                    }
                    if (*em).get_own_property.is_some() {
                        let mut desc: JSPropertyDescriptor = std::mem::zeroed();

                        /* Note: if 'p' is a prototype, it can be
                        freed in the called function */
                        let obj1 = JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));
                        let ret = (*em).get_own_property.unwrap()(ctx, &mut desc, obj1, prop);
                        JS_FreeValue(ctx, obj1);
                        if ret < 0 {
                            return JSValue::EXCEPTION;
                        }
                        if ret != 0 {
                            if desc.flags & JS_PROP_GETSET != 0 {
                                JS_FreeValue(ctx, desc.setter);
                                return JS_CallFree(
                                    ctx,
                                    desc.getter,
                                    this_obj,
                                    0,
                                    std::ptr::null_mut(),
                                );
                            } else {
                                return desc.value;
                            }
                        }
                    }
                }
            }
        }

        p = (*(*p).shape).proto;
        if p.is_null() {
            break;
        }
    }

    if unlikely(throw_ref_error) {
        JS_ThrowReferenceErrorNotDefined(ctx, prop)
    } else {
        JSValue::UNDEFINED
    }
}

/// NOTE: returned property value has increased ref_count by 1
pub unsafe fn JS_GetPropertyValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSValue,
) -> JSValue {
    let atom;
    let ret;

    if JS_VALUE_GET_TAG(this_obj) == JSTag::Object as i32
        && JS_VALUE_GET_TAG(prop) == JSTag::Int as i32
    {
        /* fast path for array access */
        let p = JS_VALUE_GET_OBJ(this_obj);
        let idx = JS_VALUE_GET_INT(prop);
        match (*p).class_id() {
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                if idx >= (*p).u.array.count as i32 {
                    // goto slow_path converted to control flow
                } else {
                    return JS_DupValue(ctx, *(*p).u.array.u.values.offset(idx as isize));
                }
            }
            JS_CLASS_INT8_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // goto slow_path converted to control flow
                } else {
                    return JSValue::from_i32(*(*p).u.array.u.int8_ptr.offset(idx as isize) as i32);
                }
            }
            JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    return JSValue::from_i32(*(*p).u.array.u.uint8_ptr.offset(idx as isize) as i32);
                }
            }
            JS_CLASS_INT16_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    return JSValue::from_i32(*(*p).u.array.u.int16_ptr.offset(idx as isize) as i32);
                }
            }
            JS_CLASS_UINT16_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    return JSValue::from_i32(
                        *(*p).u.array.u.uint16_ptr.offset(idx as isize) as i32
                    );
                }
            }
            JS_CLASS_INT32_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    return JSValue::from_i32(*(*p).u.array.u.int32_ptr.offset(idx as isize));
                }
            }
            JS_CLASS_UINT32_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    return JSValue::from_u32(*(*p).u.array.u.uint32_ptr.offset(idx as isize));
                }
            }
            JS_CLASS_BIG_INT64_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    //return JS_NewBigInt64(ctx, *(*p).u.array.u.int64_ptr.offset(idx as isize));
                    unimplemented!()
                }
            }
            JS_CLASS_BIG_UINT64_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    //return JS_NewBigUint64(ctx, *(*p).u.array.u.uint64_ptr.offset(idx as isize));
                    unimplemented!()
                }
            }
            JS_CLASS_FLOAT32_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    return JSValue::from_f64_as_is(
                        *(*p).u.array.u.float_ptr.offset(idx as isize) as f64
                    );
                }
            }
            JS_CLASS_FLOAT64_ARRAY => {
                if unlikely(idx >= (*p).u.array.count as i32) {
                    // C2RS: goto slow_path converted to control flow
                } else {
                    return JSValue::from_f64_as_is(
                        *(*p).u.array.u.double_ptr.offset(idx as isize),
                    );
                }
            }
            _ => {
                // C2RS: goto slow_path converted to control flow
            }
        }
    }

    // slow_path:
    atom = JS_ValueToAtom(ctx, prop);
    JS_FreeValue(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        return JSValue::EXCEPTION;
    }
    ret = JS_GetProperty(ctx, this_obj, atom);
    JS_FreeAtom(ctx, atom);
    ret
}

pub unsafe fn JS_GetPropertyUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: u32,
) -> JSValue {
    JS_GetPropertyValue(ctx, this_obj, JSValue::from_u32(idx))
}

pub unsafe fn JS_GetPropertyInt64(ctx: *mut JSContext, obj: JSValueConst, idx: i64) -> JSValue {
    if (idx as u64) <= i32::MAX as u64 {
        // fast path for fast arrays
        return JS_GetPropertyValue(ctx, obj, JSValue::from_i32(idx as i32));
    }
    let prop = JS_NewAtomInt64(ctx, idx);
    if prop == JS_ATOM_NULL {
        return JSValue::EXCEPTION;
    }

    let val = JS_GetProperty(ctx, obj, prop);
    JS_FreeAtom(ctx, prop);
    val
}

/// Check if an object has a generalized numeric property `idx`. Returns:
/// Err for exception,
/// Ok(Some(_)) if property exists,
/// Ok(None) if proprty does not exist.
pub unsafe fn JS_TryGetPropertyInt64(
    ctx: *mut JSContext,
    obj: JSValueConst,
    idx: i64,
) -> Result<Option<JSValue>, i32> {
    if likely((idx as u64) <= JS_ATOM_MAX_INT as u64) {
        // fast path
        if JS_HasProperty(ctx, obj, JSAtom::from_u32(idx as u32))? {
            let val = JS_GetPropertyValue(ctx, obj, JSValue::from_i32(idx as i32));
            if JS_IsException(val) {
                Err(-1)
            } else {
                Ok(Some(val))
            }
        } else {
            Ok(None)
        }
    } else {
        let prop = JS_NewAtomInt64(ctx, idx);
        let ret = if likely(prop != JS_ATOM_NULL) {
            JS_HasProperty(ctx, obj, prop).and_then(|b| {
                if b {
                    let val = JS_GetPropertyValue(ctx, obj, JSValue::from_i32(idx as i32));
                    if unlikely(JS_IsException(val)) {
                        Err(-1)
                    } else {
                        Ok(Some(val))
                    }
                } else {
                    Ok(None) // not exists
                }
            })
        } else {
            Err(-1) // can't create prop atom
        };
        JS_FreeAtom(ctx, prop);
        ret
    }
}

/// `val` is duplicated inside
pub unsafe fn JS_CreateProperty(
    ctx: *mut JSContext,
    p: *mut JSObject,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    macro_rules! generic_array {
        ($p:expr, $idx:expr) => {
            /* update the length field */
            let plen = (*$p).prop_values.first_mut().unwrap();
            let mut len = JS_ToUint32(ctx, (*plen).u.value)?;
            if ($idx + 1) > len {
                let prop_flags0 = (*(*$p).shape).props.first().unwrap().1.flags();
                if unlikely(!(prop_flags0 & JS_PROP_WRITABLE) != 0) {
                    return if JS_ThrowTypeErrorReadOnly(ctx, flags, JS_ATOM_length) == 0 {
                        Ok(false)
                    } else {
                        Err(-1)
                    };
                }

                /* XXX: should update the length after defining
                the property */
                len = $idx + 1;
                set_value(ctx, &mut (*plen).u.value, JSValue::from_u32(len));
            }
        };
    }

    macro_rules! convert_to_array {
        ($p:expr, $idx:expr) => {
            if convert_fast_array_to_array(ctx, $p) != 0 {
                return Err(-1);
            }
            generic_array!($p, $idx);
        };
    }

    /* add a new property or modify an existing exotic one */
    if (*p).detailed.is_exotic() {
        match (*p).class_id() {
            JS_CLASS_ARRAY => {
                let mut idx: u32 = 0;

                if (*p).detailed.fast_array() {
                    if prop.is_int_type() {
                        idx = __JS_AtomToUInt32(prop);
                        if idx == (*p).u.array.count {
                            if !(*p).detailed.extensible() {
                                return if (*ctx).JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"object is not extensible",
                                ) == 0
                                {
                                    Ok(false)
                                } else {
                                    Err(-1)
                                };
                            }

                            if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                convert_to_array!(p, idx);
                            }
                            let prop_flags = get_prop_flags(flags, 0);
                            if prop_flags != JS_PROP_C_W_E {
                                convert_to_array!(p, idx);
                            }
                            let r = add_fast_array_element(ctx, p, val.dup(), flags);
                            return if r < 0 { Err(-1) } else { Ok(r != 0) };
                        } else {
                            convert_to_array!(p, idx);
                        }
                    } else if JS_AtomIsArrayIndex(ctx, &mut idx, prop) {
                        /* convert the fast array to normal array */
                        convert_to_array!(p, idx);
                    }
                } else if JS_AtomIsArrayIndex(ctx, &mut idx, prop) {
                    generic_array!(p, idx);
                }
            }
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                let ret = JS_AtomIsNumericIndex(ctx, prop);
                if ret != 0 {
                    if ret < 0 {
                        return Err(-1);
                    }
                    return if ctx.as_mut().unwrap().JS_ThrowTypeErrorOrFalse(
                        flags as _,
                        c"cannot create numeric index in typed array",
                    ) == 0
                    {
                        Ok(false)
                    } else {
                        Err(-1)
                    };
                }
            }
            cid => {
                if (flags & JS_PROP_NO_EXOTIC) == 0 {
                    let em = (*ctx).rt().class_array[cid as usize].exotic;
                    if !em.is_null() {
                        if let Some(cb) = (*em).define_own_property {
                            let r = cb(
                                ctx,
                                JS_MKPTR!(JSTag::Object, p.cast()),
                                prop,
                                val,
                                getter,
                                setter,
                                flags as _,
                            );
                            return if r < 0 { Err(-1) } else { Ok(r != 0) };
                        }

                        if !JS_IsExtensible(ctx, JS_MKPTR!(JSTag::Object, p.cast()))? {
                            return if ctx
                                .as_mut()
                                .unwrap()
                                .JS_ThrowTypeErrorOrFalse(flags as _, c"object is not extensible")
                                == 0
                            {
                                Ok(false)
                            } else {
                                Err(-1)
                            };
                        }
                    }
                }
            }
        }
    }

    if !(*p).detailed.extensible() {
        return if (*ctx).JS_ThrowTypeErrorOrFalse(flags as _, c"object is not extensible") == 0 {
            Ok(false)
        } else {
            Err(-1)
        };
    }

    let prop_flags = if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
        (flags & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
    } else {
        flags & JS_PROP_C_W_E
    };
    let pr = (*p).add_property(ctx.as_mut().unwrap(), prop, prop_flags)?;

    if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
        (*pr).u.getset.getter = std::ptr::null_mut();
        if (flags & JS_PROP_HAS_GET) != 0 && JS_IsFunction(ctx, getter) {
            (*pr).u.getset.getter = JS_VALUE_GET_OBJ(JS_DupValue(ctx, getter));
        }
        (*pr).u.getset.setter = std::ptr::null_mut();
        if (flags & JS_PROP_HAS_SET) != 0 && JS_IsFunction(ctx, setter) {
            (*pr).u.getset.setter = JS_VALUE_GET_OBJ(JS_DupValue(ctx, setter));
        }
    } else {
        if (flags & JS_PROP_HAS_VALUE) != 0 {
            (*pr).u.value = val.dup();
        } else {
            (*pr).u.value = JSValue::UNDEFINED;
        }
    }

    Ok(true)
}

pub unsafe fn JS_DefinePropertyGetSet(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> i32 {
    let ret = JS_DefineProperty(
        ctx,
        this_obj,
        prop,
        JSValue::UNDEFINED,
        getter,
        setter,
        flags
            | JS_PROP_HAS_GET
            | JS_PROP_HAS_SET
            | JS_PROP_HAS_CONFIGURABLE
            | JS_PROP_HAS_ENUMERABLE,
    );
    JS_FreeValue(ctx, getter);
    JS_FreeValue(ctx, setter);

    return ret;
}

/* return Err() if exception (Proxy object only) or TRUE/FALSE */
pub unsafe fn JS_IsExtensible(ctx: *mut JSContext, obj: JSValueConst) -> Result<bool, i32> {
    if unlikely(JS_VALUE_GET_TAG(obj) != JSTag::Object as i32) {
        return Ok(false);
    }

    let p = JS_VALUE_GET_OBJ(obj);
    if unlikely((*p).class_id() == JS_CLASS_PROXY) {
        js_proxy_isExtensible(ctx, obj)
    } else {
        Ok((*p).detailed.extensible())
    }
}

/* Return -1 if exception,
FALSE if the property does not exist, TRUE if it exists. If TRUE is
returned, the property descriptor 'desc' is filled present. */
pub unsafe fn JS_GetOwnPropertyInternal(
    ctx: *mut JSContext,
    desc: *mut JSPropertyDescriptor,
    p: *mut JSObject,
    prop: JSAtom,
) -> i32 {
    let mut pr: *mut JSProperty = std::ptr::null_mut();
    let mut prs: *mut JSShapeProperty;

    loop {
        prs = find_own_property(&mut pr as *mut *mut JSProperty, p, prop);
        if !prs.is_null() {
            if !desc.is_null() {
                (*desc).flags = (*prs).flags() & JS_PROP_C_W_E;
                (*desc).getter = JSValue::UNDEFINED;
                (*desc).setter = JSValue::UNDEFINED;
                (*desc).value = JSValue::UNDEFINED;
                if unlikely((*prs).flags() & JS_PROP_TMASK != 0) {
                    match (*prs).flags() & JS_PROP_TMASK {
                        JS_PROP_GETSET => {
                            (*desc).flags |= JS_PROP_GETSET;
                            if !(*pr).u.getset.getter.is_null() {
                                (*desc).getter =
                                    JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()).dup();
                            }
                            if !(*pr).u.getset.setter.is_null() {
                                (*desc).setter =
                                    JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()).dup();
                            }
                        }
                        JS_PROP_VARREF => {
                            let val = *(*(*pr).u.var_ref).pvalue;
                            if unlikely(JS_IsUninitialized(val)) {
                                JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom);
                                return -1;
                            }
                            (*desc).value = val.dup();
                        }
                        JS_PROP_AUTOINIT => {
                            /* Instantiate property and retry */
                            if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                                return -1;
                            }
                            continue; // C2RS: Replaced goto retry with loop+continue
                        }
                        _ => {}
                    }
                } else {
                    (*desc).value = (*pr).u.value.dup();
                }
            } else {
                /* for consistency, send the exception even if desc is NULL */
                if unlikely((*prs).flags() & JS_PROP_TMASK == JS_PROP_VARREF) {
                    if unlikely(JS_IsUninitialized(*(*(*pr).u.var_ref).pvalue)) {
                        JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom);
                        return -1;
                    }
                } else if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_AUTOINIT {
                    /* nothing to do: delay instantiation until actual value and/or attributes are read */
                }
            }
            return 1;
        }

        if (*p).detailed.is_exotic() {
            if (*p).detailed.fast_array() {
                /* specific case for fast arrays */
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < (*p).u.array.count {
                        if !desc.is_null() {
                            (*desc).flags =
                                JS_PROP_WRITABLE | JS_PROP_ENUMERABLE | JS_PROP_CONFIGURABLE;
                            (*desc).getter = JSValue::UNDEFINED;
                            (*desc).setter = JSValue::UNDEFINED;
                            (*desc).value =
                                JS_GetPropertyUint32(ctx, JS_MKPTR!(JSTag::Object, p.cast()), idx);
                        }
                        return 1;
                    }
                }
            } else {
                let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
                if !em.is_null() {
                    if let Some(cb) = (*em).get_own_property {
                        return cb(ctx, desc, JS_MKPTR!(JSTag::Object, p.cast()), prop);
                    }
                }
            }
        }

        break;
    }

    0
}

/// returned value has its ref_count incremented by 1
pub unsafe fn JS_GetOwnProperty(
    ctx: *mut JSContext,
    desc: *mut JSPropertyDescriptor,
    obj: JSValueConst,
    prop: JSAtom,
) -> i32 {
    if JS_VALUE_GET_TAG(obj) != JSTag::Object as i32 {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return -1;
    }
    return JS_GetOwnPropertyInternal(ctx, desc, JS_VALUE_GET_OBJ(obj), prop);
}

pub unsafe fn JS_CopyDataProperties(
    ctx: *mut JSContext,
    target: JSValueConst,
    source: JSValueConst,
    excluded: JSValueConst,
    setprop: bool,
) -> Result<(), i32> {
    if JS_VALUE_GET_TAG(source) != JSTag::Object as i32 {
        return Ok(());
    }

    let mut tab_atom: *mut JSPropertyEnum = std::ptr::null_mut();
    let mut tab_atom_count: u32 = 0;

    let mut desc: JSPropertyDescriptor = std::mem::zeroed();

    let pexcl = if JS_VALUE_GET_TAG(excluded) == JSTag::Object as i32 {
        JS_VALUE_GET_OBJ(excluded)
    } else {
        std::ptr::null_mut()
    };

    let p = JS_VALUE_GET_OBJ(source);

    let mut gpn_flags = JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_ENUM_ONLY;
    if (*p).detailed.is_exotic() {
        let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
        // cannot use JS_GPN_ENUM_ONLY with e.g. proxies because it
        // introduces a visible change
        if !em.is_null() && (*em).get_own_property_names.is_some() {
            gpn_flags &= !JS_GPN_ENUM_ONLY;
        }
    }

    JS_GetOwnPropertyNamesInternal(ctx, &mut tab_atom, &mut tab_atom_count, p, gpn_flags)?;

    for i in 0..tab_atom_count {
        if !pexcl.is_null() {
            let r = JS_GetOwnPropertyInternal(
                ctx,
                std::ptr::null_mut(),
                pexcl,
                (*tab_atom.offset(i as isize)).atom,
            );
            if r != 0 {
                // ERR or TRUE
                if r < 0 {
                    // C2RS: goto exception replaced with early return logic
                    js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
                    return Err(-1);
                }
                continue;
            }
        }

        if gpn_flags & JS_GPN_ENUM_ONLY == 0 {
            // test if the property is enumerable
            let ret =
                JS_GetOwnPropertyInternal(ctx, &mut desc, p, (*tab_atom.offset(i as isize)).atom);
            if ret < 0 {
                // C2RS: goto exception replaced with early return logic
                js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
                return Err(-1);
            }
            if ret == 0 {
                continue;
            }

            let is_enumerable = (desc.flags & JS_PROP_ENUMERABLE) != 0;
            js_free_desc(ctx, &mut desc);
            if !is_enumerable {
                continue;
            }
        }

        let val = JS_GetProperty(ctx, source, (*tab_atom.offset(i as isize)).atom);
        if JS_IsException(val) {
            // C2RS: goto exception replaced with early return logic
            js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
            return Err(-1);
        }

        let ret = if setprop {
            JS_SetProperty(ctx, target, (*tab_atom.offset(i as isize)).atom, val)
        } else {
            JS_DefinePropertyValue(
                ctx,
                target,
                (*tab_atom.offset(i as isize)).atom,
                val,
                JS_PROP_C_W_E,
            )
        };
        if ret < 0 {
            // C2RS: goto exception replaced with early return logic
            js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
            return Err(-1);
        }
    }

    js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
    Ok(())
}

/// Get object `length` property as u32
pub unsafe fn js_get_length32(ctx: *mut JSContext, obj: JSValueConst) -> Result<u32, i32> {
    let len_val = JS_GetProperty(ctx, obj, JS_ATOM_length);
    if JS_IsException(len_val) {
        Err(-1)
    } else {
        JS_ToUint32Free(ctx, len_val)
    }
}

/// Get object `length` property as i64
pub unsafe fn js_get_length64(ctx: *mut JSContext, obj: JSValueConst) -> Result<i64, i32> {
    let len_val = JS_GetProperty(ctx, obj, JS_ATOM_length);
    if JS_IsException(len_val) {
        Err(-1)
    } else {
        JS_ToLengthFree(ctx, len_val)
    }
}

/* Note: the property value is not initialized. Return NULL if memory
error. */
#[deprecated(note = "use JSObject::add_property() instead")]
pub unsafe fn add_property(
    ctx: *mut JSContext,
    p: *mut JSObject,
    prop: JSAtom,
    prop_flags: u32,
) -> *mut JSProperty {
    (*p).add_property(ctx.as_mut().unwrap(), prop, prop_flags)
        .unwrap_or(std::ptr::null_mut())
}
