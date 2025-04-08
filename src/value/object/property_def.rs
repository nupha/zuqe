use std::ffi::CStr;

use crate::{
    array::{convert_fast_array_to_array, set_array_length, JS_ToArrayLengthFree},
    atom::{
        JSAtom, JS_AtomIsNumericIndex1, JS_FreeAtom, JS_NewAtom, JS_ValueToAtom, __JS_AtomToUInt32,
        consts::JS_ATOM_name, JS_ATOM_NULL,
    },
    context::JSContext,
    types::unlikely,
    value::object::find_own_property,
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_MODULE_NS,
    JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        set_value, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_NumberIsNegativeOrMinusZero,
        JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    check_define_prop_flags, get_prop_flags, js_free_desc, js_get_function_name, js_obj_to_desc,
    js_object_has_name, JSProperty, JSPropertyDescriptor, JS_AutoInitProperty, JS_IsFunction,
    JS_PROP_AUTOINIT, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_ENUMERABLE, JS_PROP_GETSET,
    JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_TMASK, JS_PROP_VARREF,
    JS_PROP_WRITABLE,
};

impl JSValue {
    /* allowed flags:
       JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JS_PROP_ENUMERABLE
       JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
       JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_HAS_ENUMERABLE,
       JS_PROP_THROW, JS_PROP_NO_EXOTIC.
       If JS_PROP_THROW is set, return an exception instead of FALSE.
       if JS_PROP_NO_EXOTIC is set, do not call the exotic
       define_own_property callback.
    */
    /// Note: property `val` is duplicated inside. `prop` keeps ref count unchanged after calling.

    pub unsafe fn define_property(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        mut val: JSValueConst,
        getter: JSValueConst,
        setter: JSValueConst,
        flags: u32,
    ) -> Result<bool, i32> {
        if JS_VALUE_GET_TAG(self) != JSTag::Object as i32 {
            ctx.JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }

        let p = JS_VALUE_GET_OBJ(self);

        loop {
            let mut pr = std::ptr::null_mut::<JSProperty>();
            let mut prs = find_own_property(std::ptr::addr_of_mut!(pr), p, prop);
            if !prs.is_null() {
                /* the range of the Array length property is always tested before */
                if ((*prs).flags() & JS_PROP_LENGTH) != 0 && (flags & JS_PROP_HAS_VALUE) != 0 {
                    let array_length = JS_ToArrayLengthFree(ctx, val.dup(), false)?;
                    // this code relies on the fact that Uint32 are never allocated
                    val = JSValue::from_u32(array_length);
                    // prs may have been modified
                    prs = find_own_property(&mut pr, p, prop);
                    debug_assert!(!prs.is_null());
                }

                // property already exists
                if !check_define_prop_flags((*prs).flags(), flags) {
                    return if ctx.JS_ThrowTypeErrorOrFalse(flags, c"property is not configurable") {
                        Ok(false)
                    } else {
                        Err(-1)
                    };
                }

                if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_AUTOINIT {
                    // Instantiate property and retry
                    if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                        return Err(-1);
                    }
                    continue; // redo_prop_update
                }

                if flags
                    & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)
                    != 0
                {
                    if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                        let new_getter = if getter.is_function(ctx) {
                            JS_VALUE_GET_OBJ(getter)
                        } else {
                            std::ptr::null_mut()
                        };

                        let new_setter = if setter.is_function(ctx) {
                            JS_VALUE_GET_OBJ(setter)
                        } else {
                            std::ptr::null_mut()
                        };

                        if ((*prs).flags() & JS_PROP_TMASK) != JS_PROP_GETSET {
                            if (*p).js_shape_prepare_update(ctx, &mut prs) != 0 {
                                return Err(-1);
                            }

                            // convert to getset
                            if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
                                if let Some(vr) = Box::from_raw((*pr).u.var_ref).free(ctx.rt_mut())
                                {
                                    Box::leak(vr);
                                }
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
                                if flags & JS_PROP_HAS_GET != 0
                                    && new_getter != (*pr).u.getset.getter
                                {
                                    return if ctx.JS_ThrowTypeErrorOrFalse(
                                        flags,
                                        c"property is not configurable",
                                    ) {
                                        Ok(false)
                                    } else {
                                        Err(-1)
                                    };
                                }
                                if flags & JS_PROP_HAS_SET != 0
                                    && new_setter != (*pr).u.getset.setter
                                {
                                    return if ctx.JS_ThrowTypeErrorOrFalse(
                                        flags,
                                        c"property is not configurable",
                                    ) {
                                        Ok(false)
                                    } else {
                                        Err(-1)
                                    };
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
                                getter.dup();
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
                                setter.dup();
                            }
                            (*pr).u.getset.setter = new_setter;
                        }
                    } else {
                        if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_GETSET {
                            /* convert to data descriptor */
                            if (*p).js_shape_prepare_update(ctx, &mut prs) != 0 {
                                return Err(-1);
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
                            // Note: JS_PROP_VARREF is always writable
                        } else {
                            if ((*prs).flags() & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE)) == 0
                                && (flags & JS_PROP_HAS_VALUE) != 0
                            {
                                return if !val.is_same((*pr).u.value) {
                                    if (*ctx).JS_ThrowTypeErrorOrFalse(
                                        flags,
                                        c"property is not configurable",
                                    ) {
                                        Ok(false)
                                    } else {
                                        Err(-1)
                                    }
                                } else {
                                    Ok(true)
                                };
                            }
                        }

                        if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
                            if flags & JS_PROP_HAS_VALUE != 0 {
                                if (*p).class_id() == JS_CLASS_MODULE_NS {
                                    /* JS_PROP_WRITABLE is always true for variable
                                    references, but they are write protected in module name
                                    spaces. */
                                    if !val.is_same(*(*(*pr).u.var_ref).pvalue) {
                                        return if ctx.JS_ThrowTypeErrorOrFalse(
                                            flags,
                                            c"property is not configurable",
                                        ) {
                                            Ok(false)
                                        } else {
                                            Err(-1)
                                        };
                                    }
                                } else {
                                    /* update the reference */
                                    set_value(ctx, (*(*pr).u.var_ref).pvalue, val.dup());
                                }
                            }
                            /* if writable is set to false, no longer a
                            reference (for mapped arguments) */
                            if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
                                == JS_PROP_HAS_WRITABLE
                            {
                                if (*p).class_id() == JS_CLASS_MODULE_NS {
                                    return if ctx.JS_ThrowTypeErrorOrFalse(
                                        flags,
                                        c"module namespace properties have writable = false",
                                    ) {
                                        Ok(false)
                                    } else {
                                        Err(-1)
                                    };
                                }
                                if (*p).js_shape_prepare_update(ctx, &mut prs) != 0 {
                                    return Err(-1);
                                }
                                let val1 = (*(*(*pr).u.var_ref).pvalue).dup();
                                if let Some(vr) = Box::from_raw((*pr).u.var_ref).free(ctx.rt_mut())
                                {
                                    Box::leak(vr);
                                    //  unreachable!();
                                }
                                (*pr).u.value = val1;
                                (*prs).set_flags(
                                    (*prs).flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE),
                                );
                            }
                        } else if ((*prs).flags() & JS_PROP_LENGTH) != 0 {
                            let res = if flags & JS_PROP_HAS_VALUE != 0 {
                                /* Note: no JS code is executable because 'val' is guaranted to be a Uint32 */
                                set_array_length(ctx, p, val.dup(), flags)
                            } else {
                                1
                            };
                            /* still need to reset the writable flag if
                            needed.  The JS_PROP_LENGTH is kept because the
                            Uint32 test is still done if the length
                            property is read-only. */
                            if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
                                == JS_PROP_HAS_WRITABLE
                            {
                                (_, prs) = (*p)
                                    .shape_mut()
                                    .and_then(|sh| sh.props.first_mut())
                                    .unwrap();

                                if (*p).js_update_property_flags(
                                    ctx,
                                    std::ptr::addr_of_mut!(prs),
                                    (*prs).flags() & !JS_PROP_WRITABLE,
                                ) != 0
                                {
                                    return Err(-1);
                                }
                            }
                            return match res {
                                0 => Ok(false),
                                1 => Ok(true),
                                -1 => Err(-1),
                                _ => unreachable!(),
                            };
                        } else {
                            if flags & JS_PROP_HAS_VALUE != 0 {
                                JS_FreeValue(ctx, (*pr).u.value);
                                (*pr).u.value = val.dup();
                            }
                            if flags & JS_PROP_HAS_WRITABLE != 0 {
                                if (*p).js_update_property_flags(
                                    ctx,
                                    std::ptr::addr_of_mut!(prs),
                                    ((*prs).flags() & !JS_PROP_WRITABLE)
                                        | (flags as u32 & JS_PROP_WRITABLE),
                                ) != 0
                                {
                                    return Err(-1);
                                }
                            }
                        }
                    }
                }

                let mut mask = 0;
                if flags & JS_PROP_HAS_CONFIGURABLE != 0 {
                    mask |= JS_PROP_CONFIGURABLE;
                }
                if flags & JS_PROP_HAS_ENUMERABLE != 0 {
                    mask |= JS_PROP_ENUMERABLE;
                }
                if (*p).js_update_property_flags(
                    ctx,
                    std::ptr::addr_of_mut!(prs),
                    ((*prs).flags() & !mask) | (flags & mask),
                ) != 0
                {
                    return Err(-1);
                }

                return Ok(true);
            }

            // modification of fast array elements
            if (*p).detailed.fast_array() {
                match (*p).class_id() {
                    JS_CLASS_ARRAY => {
                        let lst = (*p).fast_array_values_mut_unchecked();
                        if prop.is_int_type() {
                            let idx = __JS_AtomToUInt32(prop) as usize;
                            if idx < lst.len() {
                                let prop_flags = get_prop_flags(flags, JS_PROP_C_W_E);
                                if prop_flags != JS_PROP_C_W_E {
                                    if convert_fast_array_to_array(ctx, p) != 0 {
                                        return Err(-1);
                                    } else {
                                        continue; // redo_prop_update
                                    }
                                }
                                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                    if convert_fast_array_to_array(ctx, p) != 0 {
                                        return Err(-1);
                                    } else {
                                        continue; // redo_prop_update
                                    }
                                }
                                if flags & JS_PROP_HAS_VALUE != 0 {
                                    set_value(ctx, lst.get_mut(idx as usize).unwrap(), val.dup());
                                }
                                return Ok(true);
                            }
                        }
                    }
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        if !prop.is_int_type() {
                            /* slow path with to handle all numeric indexes */
                            let num = JS_AtomIsNumericIndex1(ctx, prop);
                            if num.is_exception() {
                                return Err(-1);
                            }
                            if num.is_undefined() {
                                break; // goto typed_array_done
                            }

                            if let Ok(b) = num.is_integer(ctx) {
                                if !b {
                                    JS_FreeValue(ctx, num);
                                    return if ctx.JS_ThrowTypeErrorOrFalse(
                                        flags,
                                        c"non integer index in typed array",
                                    ) {
                                        Ok(false)
                                    } else {
                                        Err(-1)
                                    };
                                }
                            } else {
                                JS_FreeValue(ctx, num);
                                return Err(-1);
                            };

                            let ret = JS_NumberIsNegativeOrMinusZero(ctx, num);
                            JS_FreeValue(ctx, num);
                            if ret {
                                return if (*ctx).JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"negative index in typed array",
                                ) {
                                    Ok(false)
                                } else {
                                    Err(-1)
                                };
                            }

                            if !prop.is_int_type() {
                                return if ctx.JS_ThrowTypeErrorOrFalse(
                                    flags,
                                    c"out-of-bound index in typed array",
                                ) {
                                    Ok(false)
                                } else {
                                    Err(-1)
                                };
                            }
                        }

                        let idx = __JS_AtomToUInt32(prop);
                        /* if the typed array is detached, p->u.array.count = 0 */
                        if idx >= (*p).array_length_unchecked() as u32 {
                            return if ctx.JS_ThrowTypeErrorOrFalse(
                                flags,
                                c"out-of-bound index in typed array",
                            ) {
                                Ok(false)
                            } else {
                                Err(-1)
                            };
                        }

                        let prop_flags = get_prop_flags(
                            flags,
                            JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                        );
                        if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0
                            || prop_flags
                                != (JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
                        {
                            return if ctx
                                .JS_ThrowTypeErrorOrFalse(flags as _, c"invalid descriptor flags")
                            {
                                Ok(false)
                            } else {
                                Err(-1)
                            };
                        }
                        if flags & JS_PROP_HAS_VALUE != 0 {
                            return self.set_property_value(
                                ctx,
                                JSValue::from_i32(idx as i32),
                                val.dup(),
                                flags,
                            );
                        }

                        return Ok(true);
                    }

                    #[cfg(debug_assertions)]
                    JS_CLASS_ARGUMENTS => {}
                    _ => {
                        #[cfg(debug_assertions)]
                        unreachable!();
                    }
                }
            }

            break;
        }

        #[cfg(debug_assertions)]
        let _val_rc = val.ref_count();

        let r = (*p).create_property(ctx, prop, val, getter, setter, flags);

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

        r
    }

    /// shortcut to add or redefine a new property value.
    /// Note: property value is consumed.
    pub fn define_property_value(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        #[cfg(debug_assertions)]
        let _val_rc = val.ref_count();

        let ret = unsafe {
            self.define_property(
                ctx,
                prop,
                val, // will be duplicated inside
                JSValue::UNDEFINED,
                JSValue::UNDEFINED,
                flags
                    | JS_PROP_HAS_VALUE
                    | JS_PROP_HAS_CONFIGURABLE
                    | JS_PROP_HAS_WRITABLE
                    | JS_PROP_HAS_ENUMERABLE,
            )
        };

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
        unsafe {
            JS_FreeValue(ctx, val);
        }

        ret
    }
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
#[deprecated(note = "use JSValue::define_property() instead")]
pub unsafe fn JS_DefineProperty(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: u32,
) -> i32 {
    this_obj
        .define_property(ctx.as_mut().unwrap(), prop, val, getter, setter, flags)
        .map_or(-1, |b| b as i32)

    // let mut mask: u32;
    // let res: i32;

    // if JS_VALUE_GET_TAG(this_obj) != JSTag::Object as i32 {
    //     ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
    //     return -1;
    // }
    // let p = JS_VALUE_GET_OBJ(this_obj);

    // loop {
    //     let mut pr = std::ptr::null_mut::<JSProperty>();
    //     let mut prs = find_own_property(std::ptr::addr_of_mut!(pr), p, prop);

    //     if !prs.is_null() {
    //         /* the range of the Array length property is always tested before */
    //         if ((*prs).flags() & JS_PROP_LENGTH) != 0 && (flags & JS_PROP_HAS_VALUE) != 0 {
    //             let array_length = if let Ok(n) = JS_ToArrayLengthFree(ctx, val.dup(), false) {
    //                 n
    //             } else {
    //                 return -1;
    //             };

    //             /* this code relies on the fact that Uint32 are never allocated */
    //             val = JSValue::from_u32(array_length);
    //             /* prs may have been modified */
    //             prs = find_own_property(&mut pr, p, prop);
    //             assert!(!prs.is_null());
    //         }
    //         /* property already exists */
    //         if !check_define_prop_flags((*prs).flags(), flags) {
    //             return if (*ctx).JS_ThrowTypeErrorOrFalse(flags, c"property is not configurable") {
    //                 0
    //             } else {
    //                 -1
    //             };
    //         }

    //         if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_AUTOINIT {
    //             /* Instantiate property and retry */
    //             if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
    //                 return -1;
    //             }
    //             continue; // redo_prop_update
    //         }

    //         if flags
    //             & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)
    //             != 0
    //         {
    //             if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
    //                 let new_getter = if JS_IsFunction(ctx, getter) {
    //                     JS_VALUE_GET_OBJ(getter)
    //                 } else {
    //                     std::ptr::null_mut()
    //                 };

    //                 let new_setter = if JS_IsFunction(ctx, setter) {
    //                     JS_VALUE_GET_OBJ(setter)
    //                 } else {
    //                     std::ptr::null_mut()
    //                 };

    //                 if ((*prs).flags() & JS_PROP_TMASK) != JS_PROP_GETSET {
    //                     if p.as_mut().unwrap().js_shape_prepare_update(ctx, &mut prs) != 0 {
    //                         return -1;
    //                     }
    //                     /* convert to getset */
    //                     if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
    //                         if let Some(vr) = Box::from_raw((*pr).u.var_ref).free((*ctx).rt_mut()) {
    //                             std::mem::forget(vr);
    //                         }
    //                         //  free_var_ref((*ctx).rt, (*pr).u.var_ref);
    //                     } else {
    //                         JS_FreeValue(ctx, (*pr).u.value);
    //                     }
    //                     (*prs).set_flags(
    //                         ((*prs).flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE))
    //                             | JS_PROP_GETSET,
    //                     );
    //                     (*pr).u.getset.getter = std::ptr::null_mut();
    //                     (*pr).u.getset.setter = std::ptr::null_mut();
    //                 } else {
    //                     if ((*prs).flags() & JS_PROP_CONFIGURABLE) == 0 {
    //                         if flags & JS_PROP_HAS_GET != 0 && new_getter != (*pr).u.getset.getter {
    //                             return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                                 flags,
    //                                 c"property is not configurable",
    //                             ) {
    //                                 0
    //                             } else {
    //                                 -1
    //                             };
    //                         }
    //                         if flags & JS_PROP_HAS_SET != 0 && new_setter != (*pr).u.getset.setter {
    //                             return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                                 flags,
    //                                 c"property is not configurable",
    //                             ) {
    //                                 0
    //                             } else {
    //                                 -1
    //                             };
    //                         }
    //                     }
    //                 }
    //                 if flags & JS_PROP_HAS_GET != 0 {
    //                     if !(*pr).u.getset.getter.is_null() {
    //                         JS_FreeValue(
    //                             ctx,
    //                             JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()),
    //                         );
    //                     }
    //                     if !new_getter.is_null() {
    //                         getter.dup();
    //                     }
    //                     (*pr).u.getset.getter = new_getter;
    //                 }
    //                 if flags & JS_PROP_HAS_SET != 0 {
    //                     if !(*pr).u.getset.setter.is_null() {
    //                         JS_FreeValue(
    //                             ctx,
    //                             JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()),
    //                         );
    //                     }
    //                     if !new_setter.is_null() {
    //                         setter.dup();
    //                     }
    //                     (*pr).u.getset.setter = new_setter;
    //                 }
    //             } else {
    //                 if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_GETSET {
    //                     /* convert to data descriptor */
    //                     if (*p).js_shape_prepare_update(ctx, &mut prs) != 0 {
    //                         return -1;
    //                     }
    //                     if !(*pr).u.getset.getter.is_null() {
    //                         JS_FreeValue(
    //                             ctx,
    //                             JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()),
    //                         );
    //                     }
    //                     if !(*pr).u.getset.setter.is_null() {
    //                         JS_FreeValue(
    //                             ctx,
    //                             JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()),
    //                         );
    //                     }
    //                     (*prs).set_flags((*prs).flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
    //                     (*pr).u.value = JSValue::UNDEFINED;
    //                 } else if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
    //                     /* Note: JS_PROP_VARREF is always writable */
    //                 } else {
    //                     if ((*prs).flags() & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE)) == 0
    //                         && (flags & JS_PROP_HAS_VALUE) != 0
    //                     {
    //                         if !val.is_same((*pr).u.value) {
    //                             return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                                 flags,
    //                                 c"property is not configurable",
    //                             ) {
    //                                 0
    //                             } else {
    //                                 -1
    //                             };
    //                         } else {
    //                             return 1;
    //                         }
    //                     }
    //                 }
    //                 if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF {
    //                     if flags & JS_PROP_HAS_VALUE != 0 {
    //                         if (*p).class_id() == JS_CLASS_MODULE_NS {
    //                             /* JS_PROP_WRITABLE is always true for variable
    //                             references, but they are write protected in module name
    //                             spaces. */
    //                             if !val.is_same(*(*(*pr).u.var_ref).pvalue) {
    //                                 return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                                     flags,
    //                                     c"property is not configurable",
    //                                 ) {
    //                                     0
    //                                 } else {
    //                                     -1
    //                                 };
    //                             }
    //                         } else {
    //                             /* update the reference */
    //                             set_value(ctx, (*(*pr).u.var_ref).pvalue, val.dup());
    //                         }
    //                     }
    //                     /* if writable is set to false, no longer a
    //                     reference (for mapped arguments) */
    //                     if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE
    //                     {
    //                         if (*p).class_id() == JS_CLASS_MODULE_NS {
    //                             return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                                 flags,
    //                                 c"module namespace properties have writable = false",
    //                             ) {
    //                                 0
    //                             } else {
    //                                 -1
    //                             };
    //                         }
    //                         if p.as_mut().unwrap().js_shape_prepare_update(ctx, &mut prs) != 0 {
    //                             return -1;
    //                         }
    //                         let val1 = (*(*(*pr).u.var_ref).pvalue).dup();
    //                         if let Some(vr) = Box::from_raw((*pr).u.var_ref).free((*ctx).rt_mut()) {
    //                             Box::leak(vr);
    //                             //  unreachable!();
    //                         }
    //                         (*pr).u.value = val1;
    //                         (*prs).set_flags((*prs).flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
    //                     }
    //                 } else if ((*prs).flags() & JS_PROP_LENGTH) != 0 {
    //                     if flags & JS_PROP_HAS_VALUE != 0 {
    //                         /* Note: no JS code is executable because
    //                         'val' is guaranted to be a Uint32 */
    //                         res = set_array_length(ctx, p, val.dup(), flags);
    //                     } else {
    //                         res = 1;
    //                     }
    //                     /* still need to reset the writable flag if
    //                     needed.  The JS_PROP_LENGTH is kept because the
    //                     Uint32 test is still done if the length
    //                     property is read-only. */
    //                     if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE
    //                     {
    //                         (_, prs) = (*p)
    //                             .shape_mut()
    //                             .and_then(|sh| sh.props.first_mut())
    //                             .unwrap();
    //                         //  prs =get_shape_prop((*p).shape);
    //                         if p.as_mut().unwrap().js_update_property_flags(
    //                             ctx,
    //                             std::ptr::addr_of_mut!(prs),
    //                             (*prs).flags() & !JS_PROP_WRITABLE,
    //                         ) != 0
    //                         {
    //                             return -1;
    //                         }
    //                     }
    //                     return res;
    //                 } else {
    //                     if flags & JS_PROP_HAS_VALUE != 0 {
    //                         JS_FreeValue(ctx, (*pr).u.value);
    //                         (*pr).u.value = val.dup();
    //                     }
    //                     if flags & JS_PROP_HAS_WRITABLE != 0 {
    //                         if p.as_mut().unwrap().js_update_property_flags(
    //                             ctx,
    //                             std::ptr::addr_of_mut!(prs),
    //                             ((*prs).flags() & !JS_PROP_WRITABLE)
    //                                 | (flags as u32 & JS_PROP_WRITABLE),
    //                         ) != 0
    //                         {
    //                             return -1;
    //                         }
    //                     }
    //                 }
    //             }
    //         }

    //         mask = 0;
    //         if flags & JS_PROP_HAS_CONFIGURABLE != 0 {
    //             mask |= JS_PROP_CONFIGURABLE;
    //         }
    //         if flags & JS_PROP_HAS_ENUMERABLE != 0 {
    //             mask |= JS_PROP_ENUMERABLE;
    //         }
    //         if p.as_mut().unwrap().js_update_property_flags(
    //             ctx,
    //             std::ptr::addr_of_mut!(prs),
    //             ((*prs).flags() & !mask) | (flags & mask),
    //         ) != 0
    //         {
    //             return -1;
    //         }

    //         return 1;
    //     }

    //     // modification of fast array elements
    //     if (*p).detailed.fast_array() {
    //         match (*p).class_id() {
    //             JS_CLASS_ARRAY => {
    //                 let lst = (*p).fast_array_values_mut_unchecked();
    //                 if prop.is_int_type() {
    //                     let idx = __JS_AtomToUInt32(prop) as usize;
    //                     if idx < lst.len() {
    //                         let prop_flags = get_prop_flags(flags, JS_PROP_C_W_E);
    //                         if prop_flags != JS_PROP_C_W_E {
    //                             if convert_fast_array_to_array(ctx, p) != 0 {
    //                                 return -1;
    //                             } else {
    //                                 continue; // redo_prop_update
    //                             }
    //                         }
    //                         if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
    //                             if convert_fast_array_to_array(ctx, p) != 0 {
    //                                 return -1;
    //                             } else {
    //                                 continue; // redo_prop_update
    //                             }
    //                         }
    //                         if flags & JS_PROP_HAS_VALUE != 0 {
    //                             set_value(ctx, lst.get_mut(idx as usize).unwrap(), val.dup());
    //                         }
    //                         return 1;
    //                     }
    //                 }
    //             }
    //             JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
    //                 if !prop.is_int_type() {
    //                     /* slow path with to handle all numeric indexes */
    //                     let num = JS_AtomIsNumericIndex1(ctx, prop);
    //                     if num.is_exception() {
    //                         return -1;
    //                     }
    //                     if num.is_undefined() {
    //                         break; // goto typed_array_done
    //                     }

    //                     if let Ok(b) = num.is_integer(ctx.as_mut().unwrap()) {
    //                         if !b {
    //                             JS_FreeValue(ctx, num);
    //                             return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                                 flags,
    //                                 c"non integer index in typed array",
    //                             ) {
    //                                 0
    //                             } else {
    //                                 -1
    //                             };
    //                         }
    //                     } else {
    //                         JS_FreeValue(ctx, num);
    //                         return -1;
    //                     };

    //                     let ret = JS_NumberIsNegativeOrMinusZero(ctx, num);
    //                     JS_FreeValue(ctx, num);
    //                     if ret {
    //                         return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                             flags as _,
    //                             c"negative index in typed array",
    //                         ) {
    //                             0
    //                         } else {
    //                             -1
    //                         };
    //                     }
    //                     if !prop.is_int_type() {
    //                         return if (*ctx).JS_ThrowTypeErrorOrFalse(
    //                             flags,
    //                             c"out-of-bound index in typed array",
    //                         ) {
    //                             0
    //                         } else {
    //                             -1
    //                         };
    //                     }
    //                 }

    //                 let idx = __JS_AtomToUInt32(prop);
    //                 /* if the typed array is detached, p->u.array.count = 0 */
    //                 if idx >= (*p).array_length_unchecked() as u32 {
    //                     return if (*ctx)
    //                         .JS_ThrowTypeErrorOrFalse(flags, c"out-of-bound index in typed array")
    //                     {
    //                         0
    //                     } else {
    //                         -1
    //                     };
    //                 }
    //                 let prop_flags = get_prop_flags(
    //                     flags,
    //                     JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    //                 );
    //                 if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0
    //                     || prop_flags
    //                         != (JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
    //                 {
    //                     return if (*ctx)
    //                         .JS_ThrowTypeErrorOrFalse(flags as _, c"invalid descriptor flags")
    //                     {
    //                         0
    //                     } else {
    //                         -1
    //                     };
    //                 }
    //                 if flags & JS_PROP_HAS_VALUE != 0 {
    //                     return this_obj
    //                         .set_property_value(
    //                             ctx.as_mut().unwrap(),
    //                             JSValue::from_i32(idx as i32),
    //                             val.dup(),
    //                             flags,
    //                         )
    //                         .map_or(-1, |b| b as i32);
    //                 }

    //                 return 1;
    //             }

    //             #[cfg(debug_assertions)]
    //             JS_CLASS_ARGUMENTS => {}
    //             _ => {
    //                 #[cfg(debug_assertions)]
    //                 unreachable!();
    //             }
    //         }
    //     }

    //     break;
    // }

    // #[cfg(debug_assertions)]
    // let _val_rc = val.ref_count();

    // let r = (*p).create_property(ctx.as_mut().unwrap(), prop, val, getter, setter, flags);

    // #[cfg(debug_assertions)]
    // if flags & JS_PROP_HAS_VALUE != 0 {
    //     if let Some(c) = val.ref_count() {
    //         debug_assert!(
    //             c > _val_rc.unwrap(),
    //             "before:{}, after:{}",
    //             _val_rc.unwrap(),
    //             c
    //         );
    //     }
    // }

    // r.map_or(-1, |b| b as i32)
}

/// shortcut to add or redefine a new property value.
/// Note: property value is consumed.
#[deprecated(note = "use JSValue::defined_property_value() instead")]
pub unsafe fn JS_DefinePropertyValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value(ctx.as_mut().unwrap(), prop, val, flags)
        .map_or(-1, |b| b as i32)
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

pub unsafe fn JS_DefineObjectName(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSAtom,
    flags: u32,
) -> i32 {
    if name != JS_ATOM_NULL
        && obj.is_object()
        && !js_object_has_name(ctx, obj)
        && obj
            .define_property_value(
                ctx.as_mut().unwrap(),
                JS_ATOM_name,
                name.to_value(ctx.as_mut().unwrap(), true),
                flags,
            )
            .is_err()
    {
        return -1;
    }
    return 0;
}

pub unsafe fn JS_DefineObjectNameComputed(
    ctx: *mut JSContext,
    obj: JSValueConst,
    str: JSValueConst,
    flags: u32,
) -> i32 {
    if obj.is_object() && !js_object_has_name(ctx, obj) {
        let prop = JS_ValueToAtom(ctx, str);
        if prop == JS_ATOM_NULL {
            return -1;
        }

        let name_str = js_get_function_name(ctx, prop);
        JS_FreeAtom(ctx, prop);
        if name_str.is_exception() {
            return -1;
        }
        if obj
            .define_property_value(ctx.as_mut().unwrap(), JS_ATOM_name, name_str, flags)
            .is_err()
        {
            return -1;
        }
    }

    0
}
