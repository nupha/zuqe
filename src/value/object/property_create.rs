use crate::{
    array::{add_fast_array_element, convert_fast_array_to_array},
    atom::{JSAtom, JS_AtomIsArrayIndex, JS_AtomIsNumericIndex, __JS_AtomToUInt32, consts::*},
    context::JSContext,
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{set_value, JSTag, JSValue, JS_ToUint32, JS_VALUE_GET_OBJ},
    get_prop_flags,
    obj::{JSObject, JS_IsFunction},
    JS_IsExtensible, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_ENUMERABLE, JS_PROP_GETSET,
    JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE,
};

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
                // prop #0 of Array is length
                let prop0_flags = (*(*$p).shape).props.first().unwrap().1.flags();
                if prop0_flags & JS_PROP_WRITABLE == 0 {
                    return if (*ctx).JS_ThrowTypeErrorReadOnly(flags, JS_ATOM_length) {
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
                        if idx == (*p).array_length_unchecked() as u32 {
                            if !(*p).detailed.extensible() {
                                return if (*ctx).JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"object is not extensible",
                                ) {
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
                    return if (*ctx).JS_ThrowTypeErrorOrFalse(
                        flags,
                        c"cannot create numeric index in typed array",
                    ) {
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
                            return if (*ctx)
                                .JS_ThrowTypeErrorOrFalse(flags as _, c"object is not extensible")
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
        return if (*ctx).JS_ThrowTypeErrorOrFalse(flags, c"object is not extensible") {
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
            (*pr).u.getset.getter = JS_VALUE_GET_OBJ(getter.dup());
        }
        (*pr).u.getset.setter = std::ptr::null_mut();
        if (flags & JS_PROP_HAS_SET) != 0 && JS_IsFunction(ctx, setter) {
            (*pr).u.getset.setter = JS_VALUE_GET_OBJ(setter.dup());
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
