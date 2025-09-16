use crate::{
    JS_CLASS_ARRAY, JS_CLASS_MODULE_NS,
    atom::{JSAtom, consts::JS_ATOM_length},
    context::JSContext,
    object::{JS_PROP_LAZYINIT, JSObject},
    value::JSValue,
};

use super::{
    JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK,
    JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty,
};

impl JSObject {
    /// update normal property value at slot `idx`
    pub(super) fn update_prop_slot_normal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prs_flags: u32,
        idx: usize,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        // check configurable
        match prs_flags & JS_PROP_TMASK {
            JS_PROP_NORMAL => {
                if prs_flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                    && flags & JS_PROP_HAS_VALUE != 0
                {
                    return if !val
                        .is_same(&self.0.prop_values.borrow()[idx].value(ctx, &JSValue::UNDEFINED)?)
                    {
                        if ctx.throw_type_error_or_false(flags, "property is not configurable") {
                            Err(-1)
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(true)
                    };
                }
            }
            JS_PROP_GETSET => {
                // convert getset property to normal type
                self.shape_prepare_update(ctx)?;
                self.0.prop_values.borrow_mut()[idx] = JSProperty::default();
                self.shape().unwrap().with_mut(|s| {
                    // Note: s must be an exclusive shape, so we can modify it.
                    let prs = s.prop_defs.get_mut(&prop.0).unwrap();
                    prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                    debug_assert_eq!(prs.flags() & JS_PROP_TMASK, JS_PROP_NORMAL);
                });
            }
            JS_PROP_VARREF => {
                // JS_PROP_VARREF is always writable.
            }
            _ => {
                unreachable!()
            }
        }

        // update value
        if prs_flags & JS_PROP_TMASK == JS_PROP_VARREF {
            let slot = &mut self.0.prop_values.borrow_mut()[idx];
            if let JSProperty::VarRef(vref) = slot {
                if flags & JS_PROP_HAS_VALUE != 0 {
                    if self.class_id() == JS_CLASS_MODULE_NS {
                        /* JS_PROP_WRITABLE is always true for variable
                        references, but they are write protected in module name
                        spaces. */
                        if !val.is_same(&vref.value()) {
                            return if ctx
                                .throw_type_error_or_false(flags, "property is not configurable")
                            {
                                Err(-1)
                            } else {
                                Ok(false)
                            };
                        }
                    } else {
                        // update the referenced value
                        vref.set_value(val);
                    }
                }

                // if writable is set to false, no longer a reference (for mapped arguments)
                if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                    if self.class_id() == JS_CLASS_MODULE_NS {
                        return if ctx.throw_type_error_or_false(
                            flags,
                            "module namespace properties have writable = false",
                        ) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }

                    self.shape_prepare_update(ctx)?;
                    self.shape().unwrap().with_mut(|s| {
                        // Note: s must be an exclusive shape, so we can modify it.
                        let prs = s.prop_defs.get_mut(&prop.0).unwrap();
                        prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                        debug_assert_eq!(prs.flags() & JS_PROP_TMASK, JS_PROP_NORMAL);
                    });
                    *slot = JSProperty::Normal(vref.value().clone());
                }
                Ok(true)
            } else {
                unreachable!();
            }
        } else if prs_flags & JS_PROP_LENGTH != 0 {
            if flags & JS_PROP_HAS_VALUE != 0 {
                unsafe {
                    self.as_Array_unchecked().set_length(
                        ctx,
                        val.to_array_length(ctx, false)?,
                        ctx.should_throw_error(flags),
                    )
                }
            } else {
                Ok(true)
            }
        } else {
            if flags & JS_PROP_HAS_VALUE != 0 {
                self.0.prop_values.borrow_mut()[idx] = JSProperty::Normal(val);
            }
            Ok(true)
        }
    }

    /// update property as getset type at specified slot `idx`
    pub(super) fn update_prop_slot_getset(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prs_flags: u32,
        idx: usize,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let new_getter = if flags & JS_PROP_HAS_GET != 0 && getter.is_callable() {
            getter.as_object()
        } else {
            None
        };
        let new_setter = if flags & JS_PROP_HAS_SET != 0 && setter.is_callable() {
            setter.as_object()
        } else {
            None
        };

        let mut old_getter: Option<JSObject> = None;
        let mut old_setter: Option<JSObject> = None;

        if prs_flags & JS_PROP_TMASK != JS_PROP_GETSET {
            // property is not getset type:
            // change shape property type and clear old property value.
            self.shape_prepare_update(ctx)?;

            self.shape().unwrap().with_mut(|s| {
                let prs = s.prop_defs.get_mut(&prop.0).unwrap();
                prs.set_flags(
                    (prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET,
                );
            });
        } else {
            // alreay is getset type
            if let JSProperty::GetSet { getter, setter } =
                self.0.prop_values.borrow().get(idx).unwrap()
            {
                if prs_flags & JS_PROP_CONFIGURABLE == 0
                    && ((flags & JS_PROP_HAS_GET != 0 && new_getter != getter.as_ref())
                        || (flags & JS_PROP_HAS_SET != 0 && new_setter != setter.as_ref()))
                {
                    return if ctx.throw_type_error_or_false(flags, "property is not configurable") {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }

                if flags & JS_PROP_HAS_GET == 0 {
                    old_getter = getter.clone();
                }
                if flags & JS_PROP_HAS_SET == 0 {
                    old_setter = setter.clone();
                }
            } else {
                unreachable!()
            }
        }

        // modify property value
        self.0.prop_values.borrow_mut()[idx] = JSProperty::GetSet {
            getter: new_getter.cloned().or(old_getter),
            setter: new_setter.cloned().or(old_setter),
        };

        Ok(true)
    }

    /// overwrite property value at specified slot idx to a normal value.
    #[inline(always)]
    pub(super) fn put_prop_slot(
        &self,
        ctx: &JSContext,
        prs_flags: u32,
        prop: JSAtom,
        index: usize,
        value: JSValue,
        this_obj: &JSObject, // only used for getter, setter calling
        flags: u32,
    ) -> Result<bool, i32> {
        // fast path: normal
        match prs_flags & (JS_PROP_TMASK | JS_PROP_WRITABLE | JS_PROP_LENGTH) {
            JS_PROP_WRITABLE => {
                // non-length, normal, writable
                self.0.prop_values.borrow_mut()[index] = JSProperty::Normal(value);
                return Ok(true);
            }
            0 => {
                // non-length, normal, not-writable
                return if ctx.should_throw_error(flags) {
                    ctx.throw_type_error_read_only(prop);
                    Err(-1)
                } else {
                    Ok(false)
                };
            }
            _ => {}
        }

        //
        // slow path: length, setter, lazy
        //
        if prs_flags & JS_PROP_LENGTH != 0 {
            // special prop length: to set .length property of an array-like object
            debug_assert_eq!(self.class_id(), JS_CLASS_ARRAY);
            debug_assert_eq!(prop, JS_ATOM_length);
            debug_assert_eq!(index, 0);

            unsafe {
                self.as_Array_unchecked().set_length(
                    ctx,
                    value.to_array_length(ctx, false)?,
                    ctx.should_throw_error(flags),
                )
            }
        } else if prs_flags & JS_PROP_LAZYINIT == JS_PROP_LAZYINIT {
            // lazy init replaced with normal value
            std::mem::replace(
                &mut self.0.prop_values.borrow_mut()[index],
                JSProperty::Normal(value),
            );
            self.shape().unwrap().with_mut(|s| {
                let (_, prs) = s.prop_defs.get_index_mut(index).unwrap();
                prs.set_flags(prs.flags() & !JS_PROP_TMASK);
            });
            Ok(true)
        } else {
            // setter
            let pr = self.0.prop_values.borrow()[index].clone();
            match pr {
                JSProperty::VarRef(vref) => {
                    // JS_PROP_WRITABLE is always true for variable references,
                    // but they are write protected in module name spaces.
                    if self.class_id() == JS_CLASS_MODULE_NS {
                        if ctx.should_throw_error(flags) {
                            ctx.throw_type_error_read_only(prop);
                            Err(-1)
                        } else {
                            Ok(false)
                        }
                    } else {
                        vref.set_value(value);
                        Ok(true)
                    }
                }
                JSProperty::Normal(_) => {
                    #[cfg(debug_assertions)]
                    unreachable!("can't get here due to fast path");
                    #[cfg(not(debug_assertions))]
                    Ok(true)

                    // if prs_flags & JS_PROP_WRITABLE == 0 {
                    //     if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                    //         Err(-1)
                    //     } else {
                    //         Ok(false)
                    //     }
                    // } else {
                    //     self.0.prop_values.borrow_mut()[slot_idx] = JSProperty::Normal(value);
                    //     Ok(true)
                    // }
                }
                JSProperty::GetSet { setter, .. } => {
                    unreachable!();
                    // call_setter(
                    //     ctx,
                    //     setter.as_ref(),
                    //     &JSValue::Object(this_obj.clone()),
                    //     value,
                    //     flags,
                    // )
                }
                JSProperty::LazyInit { .. } => {
                    unreachable!();
                }
            }
        }
    }
}
