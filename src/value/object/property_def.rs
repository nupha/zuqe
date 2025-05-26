use crate::{
    array::{convert_fast_array_to_array, JS_ToArrayLengthFree},
    atom::{
        JSAtom, JS_AtomIsNumericIndex1, JS_FreeAtom, JS_ValueToAtom, __JS_AtomToUInt32, consts::*,
        JS_ATOM_NULL,
    },
    context::JSContext,
    value::JS_ToBoolFree,
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_MODULE_NS,
    JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        set_value, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_NumberIsNegativeOrMinusZero,
        JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    check_define_prop_flags, get_prop_flags, js_get_function_name, js_object_has_name,
    JSAutoInitIDEnum, JSProperty, JSPropertyDescriptor, JSShapeProperty, JS_AutoInitProperty,
    JS_PROP_AUTOINIT, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_ENUMERABLE, JS_PROP_GETSET,
    JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK,
    JS_PROP_VARREF, JS_PROP_WRITABLE,
};

impl JSValue {
    /* allowed flags:
       JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JS_PROP_ENUMERABLE
       JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
       JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_HAS_ENUMERABLE,
       JS_PROP_THROW, JS_PROP_NO_EXOTIC.
       If JS_PROP_THROW is set, return an exception instead of FALSE.
       if JS_PROP_NO_EXOTIC is set, do not call the exotic define_own_property callback.
    */
    /// Both `prop`, `val`, `getter`, `setter` are not consumed.
    pub unsafe fn define_property(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        mut val: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if JS_VALUE_GET_TAG(self) != JSTag::Object as i32 {
            ctx.JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }

        let p = JS_VALUE_GET_OBJ(self);

        loop {
            if let Some((mut prs, mut pr)) = (*p).find_own_property_mut(prop) {
                if prs.flags() & JS_PROP_LENGTH != 0 && flags & JS_PROP_HAS_VALUE != 0 {
                    // is `length` property
                    // the range of the Array length property is always tested before
                    let array_length = JS_ToArrayLengthFree(ctx, val.dup(), false)?;
                    val = JSValue::from(array_length);

                    // prs may have been modified, read it again
                    (prs, pr) = (*p).find_own_property_mut(prop).unwrap();
                }

                if !check_define_prop_flags(prs.flags(), flags) {
                    // property already exists
                    return if ctx.JS_ThrowTypeErrorOrFalse(flags, c"property is not configurable") {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }

                if prs.flags() & JS_PROP_TMASK == JS_PROP_AUTOINIT {
                    // Instantiate property and retry
                    if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                        return Err(-1);
                    }
                    continue; // retry
                }

                if flags
                    & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)
                    != 0
                {
                    if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                        // getset property
                        self.do_build_getset_property(ctx, prs, pr, getter, setter, flags)?;

                        // let new_getter = if getter.is_callable(ctx) {
                        //     getter.object_unchecked()
                        // } else {
                        //     std::ptr::null_mut()
                        // };
                        // let new_setter = if setter.is_callable(ctx) {
                        //     JS_VALUE_GET_OBJ(setter)
                        // } else {
                        //     std::ptr::null_mut()
                        // };

                        // if prs.flags() & JS_PROP_TMASK != JS_PROP_GETSET {
                        //     if (*p).shape_prepare_update(ctx, &mut (prs as *mut JSShapeProperty))
                        //         != 0
                        //     {
                        //         return Err(-1);
                        //     }

                        //     // convert property to getset type
                        //     if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF {
                        //         pr.unset_var_ref(ctx);
                        //     } else {
                        //         pr.unset_value(ctx);
                        //     }
                        //     pr.u.getset.getter = std::ptr::null_mut();
                        //     pr.u.getset.setter = std::ptr::null_mut();
                        //     prs.set_flags(
                        //         (prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE))
                        //             | JS_PROP_GETSET,
                        //     );
                        // } else if JS_PROP_TMASK & JS_PROP_CONFIGURABLE == 0 {
                        //     if (flags & JS_PROP_HAS_GET != 0 && new_getter != pr.u.getset.getter)
                        //         || (flags & JS_PROP_HAS_SET != 0
                        //             && new_setter != pr.u.getset.setter)
                        //     {
                        //         return if ctx.JS_ThrowTypeErrorOrFalse(
                        //             flags,
                        //             c"property is not configurable",
                        //         ) {
                        //             Err(-1)
                        //         } else {
                        //             Ok(false)
                        //         };
                        //     }
                        // }

                        // if flags & JS_PROP_HAS_GET != 0 {
                        //     if !pr.u.getset.getter.is_null() {
                        //         JS_FreeValue(
                        //             ctx,
                        //             JS_MKPTR!(JSTag::Object, pr.u.getset.getter.cast()),
                        //         );
                        //     }
                        //     if !new_getter.is_null() {
                        //         getter.dup();
                        //     }
                        //     pr.u.getset.getter = new_getter;
                        // }
                        // if flags & JS_PROP_HAS_SET != 0 {
                        //     if !pr.u.getset.setter.is_null() {
                        //         JS_FreeValue(
                        //             ctx,
                        //             JS_MKPTR!(JSTag::Object, pr.u.getset.setter.cast()),
                        //         );
                        //     }
                        //     if !new_setter.is_null() {
                        //         setter.dup();
                        //     }
                        //     pr.u.getset.setter = new_setter;
                        // }
                    } else {
                        // normal property
                        let res = self.do_build_normal_property(ctx, prs, pr, val, flags)?;

                        if flags & JS_PROP_LENGTH != 0 {
                            // don't change flag for length property
                            // return immediately
                            return Ok(res);
                        }

                        // if prs.flags() & JS_PROP_TMASK == JS_PROP_GETSET {
                        //     // getset property

                        //     // convert property to getset type
                        //     if (*p).shape_prepare_update(ctx, &mut (prs as *mut JSShapeProperty))
                        //         != 0
                        //     {
                        //         return Err(-1);
                        //     }
                        //     pr.unset_getset(ctx);
                        //     prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                        //     pr.u.value = JSValue::UNDEFINED;
                        // } else if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF {
                        //     pr.unset_var_ref(ctx);
                        // } else {
                        //     if prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                        //         && (flags & JS_PROP_HAS_VALUE) != 0
                        //     {
                        //         return if !val.is_same(pr.u.value) {
                        //             if ctx.JS_ThrowTypeErrorOrFalse(
                        //                 flags,
                        //                 c"property is not configurable",
                        //             ) {
                        //                 Err(-1)
                        //             } else {
                        //                 Ok(false)
                        //             }
                        //         } else {
                        //             Ok(true)
                        //         };
                        //     }
                        // }

                        // if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF {
                        //     if flags & JS_PROP_HAS_VALUE != 0 {
                        //         if (*p).class_id() == JS_CLASS_MODULE_NS {
                        //             /* JS_PROP_WRITABLE is always true for variable
                        //             references, but they are write protected in module name
                        //             spaces. */
                        //             if !val.is_same(*pr.u.var_ref.as_ref().borrow().pvalue) {
                        //                 return if ctx.JS_ThrowTypeErrorOrFalse(
                        //                     flags,
                        //                     c"property is not configurable",
                        //                 ) {
                        //                     Err(-1)
                        //                 } else {
                        //                     Ok(false)
                        //                 };
                        //             }
                        //         } else {
                        //             // update the referenced value
                        //             set_value(
                        //                 ctx,
                        //                 pr.u.var_ref
                        //                     .as_ref()
                        //                     .borrow_mut()
                        //                     .as_mut()
                        //                     .get_unchecked_mut()
                        //                     .pvalue,
                        //                 val.dup(),
                        //             );
                        //         }
                        //     }

                        //     /* if writable is set to false, no longer a reference (for mapped arguments) */
                        //     if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
                        //         == JS_PROP_HAS_WRITABLE
                        //     {
                        //         if (*p).class_id() == JS_CLASS_MODULE_NS {
                        //             return if ctx.JS_ThrowTypeErrorOrFalse(
                        //                 flags,
                        //                 c"module namespace properties have writable = false",
                        //             ) {
                        //                 Err(-1)
                        //             } else {
                        //                 Ok(false)
                        //             };
                        //         }

                        //         if (*p)
                        //             .shape_prepare_update(ctx, &mut (prs as *mut JSShapeProperty))
                        //             != 0
                        //         {
                        //             return Err(-1);
                        //         }

                        //         // convert property to value type
                        //         pr.unset_var_ref(ctx);
                        //         let v = (*pr.u.var_ref.as_ref().borrow().pvalue).dup();
                        //         pr.u.value = v;
                        //         prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                        //     }
                        // } else if prs.flags() & JS_PROP_LENGTH != 0 {
                        //     let res = if flags & JS_PROP_HAS_VALUE != 0 {
                        //         // Note: no JS code is executable because 'val' is guaranted to be a Uint32
                        //         (*p).set_array_length(ctx, val.dup(), flags)
                        //     } else {
                        //         Ok(true)
                        //     };

                        //     /* still need to reset the writable flag if
                        //     needed.  The JS_PROP_LENGTH is kept because the
                        //     Uint32 test is still done if the length
                        //     property is read-only. */
                        //     if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
                        //         == JS_PROP_HAS_WRITABLE
                        //     {
                        //         (_, prs) = (*p)
                        //             .shape_mut()
                        //             .and_then(|sh| sh.props.first_mut())
                        //             .unwrap();

                        //         if (*p).js_update_property_flags(
                        //             ctx,
                        //             &mut (prs as *mut JSShapeProperty),
                        //             prs.flags() & !JS_PROP_WRITABLE,
                        //         ) != 0
                        //         {
                        //             return Err(-1);
                        //         }
                        //     }

                        //     return res;
                        // } else {
                        //     if flags & JS_PROP_HAS_VALUE != 0 {
                        //         JS_FreeValue(ctx, pr.u.value);
                        //         pr.u.value = val.dup();
                        //     }
                        //     if flags & JS_PROP_HAS_WRITABLE != 0 {
                        //         if (*p).js_update_property_flags(
                        //             ctx,
                        //             &mut (prs as *mut JSShapeProperty),
                        //             (prs.flags() & !JS_PROP_WRITABLE) | (flags & JS_PROP_WRITABLE),
                        //         ) != 0
                        //         {
                        //             return Err(-1);
                        //         }
                        //     }
                        // }
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
                    &mut (prs as *mut JSShapeProperty),
                    (prs.flags() & !mask) | (flags & mask),
                ) != 0
                {
                    return Err(-1);
                }

                return Ok(true);
            }

            // for prs is None:
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
                                        Err(-1)
                                    } else {
                                        Ok(false)
                                    };
                                }
                            } else {
                                JS_FreeValue(ctx, num);
                                return Err(-1);
                            };

                            let ret = JS_NumberIsNegativeOrMinusZero(ctx, num);
                            JS_FreeValue(ctx, num);
                            if ret {
                                return if ctx.JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"negative index in typed array",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }

                            if !prop.is_int_type() {
                                return if ctx.JS_ThrowTypeErrorOrFalse(
                                    flags,
                                    c"out-of-bound index in typed array",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
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
                                Err(-1)
                            } else {
                                Ok(false)
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
                                Err(-1)
                            } else {
                                Ok(false)
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
        } // loop

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

    /// make a property as normal type
    unsafe fn do_build_normal_property(
        self,
        ctx: &mut JSContext,
        prs: &mut JSShapeProperty,
        pr: &mut JSProperty,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let p = self.object_unchecked();

        match prs.flags() & JS_PROP_TMASK {
            JS_PROP_GETSET => {
                // convert property to value type
                if p.shape_prepare_update(ctx, &mut (prs as *mut JSShapeProperty)) != 0 {
                    return Err(-1);
                }
                pr.unset_getset(ctx);
                prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                pr.u.value = JSValue::UNDEFINED;
            }
            JS_PROP_VARREF => {
                // always writable
            }
            JS_PROP_NORMAL => {
                if prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                    && (flags & JS_PROP_HAS_VALUE) != 0
                {
                    return if !val.is_same(pr.u.value) {
                        if ctx.JS_ThrowTypeErrorOrFalse(flags, c"property is not configurable") {
                            Err(-1)
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(true)
                    };
                }
            }
            _ => {
                unreachable!();
            }
        }

        if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF {
            if flags & JS_PROP_HAS_VALUE != 0 {
                if (*p).class_id() == JS_CLASS_MODULE_NS {
                    /* JS_PROP_WRITABLE is always true for variable
                    references, but they are write protected in module name
                    spaces. */
                    if !val.is_same(*pr.u.var_ref.as_ref().pvalue) {
                        return if ctx
                            .JS_ThrowTypeErrorOrFalse(flags, c"property is not configurable")
                        {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }
                } else {
                    // update the referenced value
                    set_value(
                        ctx,
                        (*pr.u.var_ref).as_mut().get_unchecked_mut().pvalue,
                        val.dup(),
                    );
                }
            }

            // if writable is set to false, no longer a reference (for mapped arguments)
            if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                if p.class_id() == JS_CLASS_MODULE_NS {
                    return if ctx.JS_ThrowTypeErrorOrFalse(
                        flags,
                        c"module namespace properties have writable = false",
                    ) {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }

                if p.shape_prepare_update(ctx, &mut (prs as *mut JSShapeProperty)) != 0 {
                    return Err(-1);
                }

                // convert property to value type
                pr.unset_var_ref(ctx);
                let v = (*pr.u.var_ref.as_ref().pvalue).dup();
                pr.u.value = v;
                prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
            }
        } else {
            if prs.flags() & JS_PROP_LENGTH != 0 {
                // special property: length
                debug_assert_eq!(
                    p.shape().and_then(|sh| {
                        sh.props.iter().enumerate().find_map(|(i, (a, d))| {
                            if *a == JS_ATOM_length && std::ptr::eq(d, prs) {
                                Some(i)
                            } else {
                                None
                            }
                        })
                    }),
                    Some(0),
                    "length property defination must be indexed 0"
                );

                let res = if flags & JS_PROP_HAS_VALUE != 0 {
                    // Note: no JS code is executable because 'val' is guaranted to be a Uint32
                    p.set_array_length(ctx, val.dup(), flags)
                } else {
                    Ok(true)
                };

                /* still need to reset the writable flag if
                needed.  The JS_PROP_LENGTH is kept because the
                Uint32 test is still done if the length
                property is read-only. */
                if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                    if p.js_update_property_flags(
                        ctx,
                        &mut (prs as *mut JSShapeProperty),
                        prs.flags() & !JS_PROP_WRITABLE,
                    ) != 0
                    {
                        return Err(-1);
                    }
                }

                return res;
            } else {
                // not length property
                if flags & JS_PROP_HAS_VALUE != 0 {
                    JS_FreeValue(ctx, pr.u.value);
                    pr.u.value = val.dup();
                }
                if flags & JS_PROP_HAS_WRITABLE != 0 {
                    if p.js_update_property_flags(
                        ctx,
                        &mut (prs as *mut JSShapeProperty),
                        (prs.flags() & !JS_PROP_WRITABLE) | (flags & JS_PROP_WRITABLE),
                    ) != 0
                    {
                        return Err(-1);
                    }
                }
            }
        }

        Ok(true)
    }

    /// make a property as getset type
    unsafe fn do_build_getset_property(
        self,
        ctx: &mut JSContext,
        prs: &mut JSShapeProperty,
        pr: &mut JSProperty,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let new_getter = if getter.is_callable(ctx) {
            getter.object_unchecked()
        } else {
            std::ptr::null_mut()
        };
        let new_setter = if setter.is_callable(ctx) {
            JS_VALUE_GET_OBJ(setter)
        } else {
            std::ptr::null_mut()
        };

        let p = self.object_unchecked();

        if prs.flags() & JS_PROP_TMASK != JS_PROP_GETSET {
            // property descriptor is not getset type:
            // change property description and clear old property value.
            if (*p).shape_prepare_update(ctx, &mut (prs as *mut JSShapeProperty)) != 0 {
                return Err(-1);
            }

            if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF {
                pr.unset_var_ref(ctx);
            } else {
                pr.unset_value(ctx);
            }
            pr.u.getset.getter = std::ptr::null_mut();
            pr.u.getset.setter = std::ptr::null_mut();
            prs.set_flags(
                (prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET,
            );
        } else {
            // alreay is getset type, check if configurable
            if prs.flags() & JS_PROP_CONFIGURABLE == 0 {
                if (flags & JS_PROP_HAS_GET != 0 && new_getter != pr.u.getset.getter)
                    || (flags & JS_PROP_HAS_SET != 0 && new_setter != pr.u.getset.setter)
                {
                    return if ctx.JS_ThrowTypeErrorOrFalse(flags, c"property is not configurable") {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }
            }
        }

        if flags & JS_PROP_HAS_GET != 0 {
            if !pr.u.getset.getter.is_null() {
                JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, pr.u.getset.getter.cast()));
            }
            if !new_getter.is_null() {
                getter.dup();
            }
            pr.u.getset.getter = new_getter;
        }
        if flags & JS_PROP_HAS_SET != 0 {
            if !pr.u.getset.setter.is_null() {
                JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, pr.u.getset.setter.cast()));
            }
            if !new_setter.is_null() {
                setter.dup();
            }
            pr.u.getset.setter = new_setter;
        }

        Ok(true)
    }

    /// shortcut to add or redefine a new property value.
    /// Note: `prop` is not consumed, `val` is consumed.
    pub fn define_property_value(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        #[cfg(debug_assertions)]
        let _val_rc_cnt = val.ref_count();

        let ret = unsafe {
            self.define_property(
                ctx,
                prop,
                val,
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
                c >= _val_rc_cnt.unwrap(),
                "before:{}, after:{}",
                _val_rc_cnt.unwrap(),
                c
            );
        }

        // consume `val`
        unsafe {
            JS_FreeValue(ctx, val);
        }

        ret
    }

    /// Note: `val` is consumed.
    pub fn define_property_value_str(
        self,
        ctx: &mut JSContext,
        prop: &str,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = ctx.create_atom(prop)?;
        let ret = self.define_property_value(ctx, atom, val, flags);
        unsafe {
            JS_FreeAtom(ctx, atom);
        }
        ret
    }

    /// Note: both `prop` and `val` are consumed.
    pub fn define_property_value_value(
        self,
        ctx: &mut JSContext,
        prop: JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = unsafe {
            let a = JS_ValueToAtom(ctx, prop);
            JS_FreeValue(ctx, prop);
            a
        };

        if atom == JS_ATOM_NULL {
            unsafe {
                JS_FreeValue(ctx, val);
            }
            return Err(-1);
        }

        let ret = self.define_property_value(ctx, atom, val, flags);
        unsafe {
            JS_FreeAtom(ctx, atom);
        }
        ret
    }

    /// Note: `val` is consumed.
    #[inline(always)]
    pub fn define_property_value_u32(
        self,
        ctx: &mut JSContext,
        idx: u32,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value(ctx, JSAtom::from_u32(idx), val, flags)
    }

    /// Note: `val` is consumed.
    #[inline(always)]
    pub fn define_property_value_i64(
        self,
        ctx: &mut JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        // self.define_property_value_value(ctx, JSValue::from_i64(idx), val, flags)
        let atom = ctx.rt_mut().add_atom_i64(idx);
        let ret = self.define_property_value(ctx, atom, val, flags);
        unsafe {
            JS_FreeAtom(ctx, atom);
        }
        ret
    }

    /// define getter, setter property.
    /// Note: both `getter` and `setter` are consumed.
    pub fn define_property_getset(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        unsafe {
            let ret = self.define_property(
                ctx,
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
            ret
        }
    }

    pub fn define_auto_init_property(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        id: JSAutoInitIDEnum,
        opaque: *mut std::ffi::c_void,
        flags: u32,
    ) -> Result<bool, i32> {
        if JS_VALUE_GET_TAG(self) != JSTag::Object as i32 {
            return Ok(false);
        }

        let p = unsafe { self.object_unchecked() };

        debug_assert!(
            p.find_own_property1(prop).is_none(),
            "auto init property already exists"
        );

        // Specialized CreateProperty
        let pr = unsafe {
            p.add_property(ctx, prop, (flags & JS_PROP_C_W_E) | JS_PROP_AUTOINIT)?
                .as_mut()
                .unwrap()
        };

        pr.u.init.realm_and_id = (ctx.dup() as *mut JSContext) as usize;
        debug_assert_eq!(unsafe { pr.u.init.realm_and_id & 3 }, 0);

        unsafe {
            pr.u.init.realm_and_id |= id as usize;
            debug_assert!(matches!(pr.u.init.realm_and_id & 3, 0..=2));
        }

        pr.u.init.opaque = opaque;

        Ok(true)
    }

    /// `val` is consumed.
    #[inline(always)]
    pub fn create_data_property_u32(
        self,
        ctx: &mut JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value_i64(
            ctx,
            idx,
            val,
            flags | JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
        )
    }

    /// define property from descriptor object configuration.
    /// `desc` is not consumed.
    pub fn define_property_desc(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        desc: Self,
        flags: u32,
    ) -> Result<bool, i32> {
        let mut d = JSPropertyDescriptor {
            flags: 0,
            value: JSValue::UNDEFINED,
            getter: JSValue::UNDEFINED,
            setter: JSValue::UNDEFINED,
        };

        unsafe {
            js_obj_to_desc(ctx, &mut d, desc)?;

            let ret = self.define_property(ctx, prop, d.value, d.getter, d.setter, d.flags | flags);
            js_free_desc(ctx, &mut d);
            ret
        }
    }
}

/// Descriptor config object to JSPropertyDescriptor.
/// `desc` keep ref count unchanged after calling.
pub unsafe fn js_obj_to_desc(
    ctx: &mut JSContext,
    d: &mut JSPropertyDescriptor,
    desc: JSValue,
) -> Result<(), i32> {
    if !desc.is_object() {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
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

    if desc.has_property(ctx, JS_ATOM_configurable)? {
        let conf = desc.get_property(ctx, JS_ATOM_configurable);
        if conf.is_exception() {
            fail!();
        }
        flags |= JS_PROP_HAS_CONFIGURABLE;
        if JS_ToBoolFree(ctx, conf).unwrap_or(true) {
            flags |= JS_PROP_CONFIGURABLE;
        }
    }

    if desc.has_property(ctx, JS_ATOM_writable)? {
        let conf = desc.get_property(ctx, JS_ATOM_writable);
        if conf.is_exception() {
            fail!();
        }
        flags |= JS_PROP_HAS_WRITABLE;
        if JS_ToBoolFree(ctx, conf).unwrap_or(true) {
            flags |= JS_PROP_WRITABLE;
        }
    }

    if desc.has_property(ctx, JS_ATOM_enumerable)? {
        let conf = desc.get_property(ctx, JS_ATOM_enumerable);
        if conf.is_exception() {
            fail!();
        }
        flags |= JS_PROP_HAS_ENUMERABLE;
        if JS_ToBoolFree(ctx, conf).unwrap_or(true) {
            flags |= JS_PROP_ENUMERABLE;
        }
    }

    if desc.has_property(ctx, JS_ATOM_value)? {
        flags |= JS_PROP_HAS_VALUE;
        val = desc.get_property(ctx, JS_ATOM_value);
        if val.is_exception() {
            fail!();
        }
    }

    if desc.has_property(ctx, JS_ATOM_get)? {
        flags |= JS_PROP_HAS_GET;
        getter = desc.get_property(ctx, JS_ATOM_get);
        if getter.is_exception() || !(getter.is_undefined() || getter.is_callable(ctx)) {
            ctx.JS_ThrowTypeError(c"invalid getter");
            fail!();
        }
    }

    if desc.has_property(ctx, JS_ATOM_set)? {
        flags |= JS_PROP_HAS_SET;
        setter = desc.get_property(ctx, JS_ATOM_set);
        if setter.is_exception() || !(setter.is_undefined() || setter.is_callable(ctx)) {
            ctx.JS_ThrowTypeError(c"invalid setter");
            fail!();
        }
    }

    if flags & (JS_PROP_HAS_SET | JS_PROP_HAS_GET) != 0
        && flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE) != 0
    {
        ctx.JS_ThrowTypeError(c"cannot have setter/getter and value or writable");
        fail!();
    }

    d.flags = flags;
    d.value = val;
    d.getter = getter;
    d.setter = setter;

    Ok(())
}

pub unsafe fn js_free_desc(ctx: *mut JSContext, desc: *mut JSPropertyDescriptor) {
    JS_FreeValue(ctx, (*desc).getter);
    JS_FreeValue(ctx, (*desc).setter);
    JS_FreeValue(ctx, (*desc).value);
}

pub unsafe fn JS_DefineObjectName(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSAtom,
    flags: u32,
) -> i32 {
    if name != JS_ATOM_NULL
        && obj.is_object()
        && !js_object_has_name(obj)
        && obj
            .define_property_value(
                ctx.as_mut().unwrap(),
                JS_ATOM_name,
                name.to_value(ctx.as_mut().unwrap(), true),
                flags,
            )
            .is_err()
    {
        -1
    } else {
        0
    }
}

pub unsafe fn JS_DefineObjectNameComputed(
    ctx: *mut JSContext,
    obj: JSValueConst,
    str: JSValueConst,
    flags: u32,
) -> i32 {
    if obj.is_object() && !js_object_has_name(obj) {
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
