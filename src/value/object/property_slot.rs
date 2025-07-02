use crate::{
    JS_CLASS_ARRAY, JS_CLASS_MODULE_NS,
    atom::{JSAtom, consts::JS_ATOM_length},
    context::JSContext,
    value::object::{JS_PROP_AUTOINIT, JSObject, property_set::call_setter},
};

use super::{
    super::JSValue, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET,
    JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NORMAL,
    JS_PROP_TMASK, JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty,
};

impl JSObject {
    /// update property value as normal at specified slot `idx`
    pub(super) fn update_prop_slot_normal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prs_flags: u32,
        idx: usize,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        match prs_flags & JS_PROP_TMASK {
            JS_PROP_NORMAL => {
                if prs_flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                    && (flags & JS_PROP_HAS_VALUE) != 0
                {
                    return if !val.is_same(
                        &self.0.prop_values.borrow_mut()[idx].value(ctx, &JSValue::UNDEFINED),
                    ) {
                        if ctx.JS_ThrowTypeErrorOrFalse(flags, "property is not configurable") {
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
                // convert property to value type
                self.shape_prepare_update(ctx)?;
                self.0.prop_values.borrow_mut()[idx] = JSProperty::Normal(JSValue::UNDEFINED);
                self.shape().unwrap().with_mut(|s| {
                    let prs = s.prop_defs.get_mut(&prop.0).unwrap();
                    prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                });
            }
            JS_PROP_VARREF => {
                if let JSProperty::VarRef(vref) = self.0.prop_values.borrow_mut()[idx].clone() {
                    if flags & JS_PROP_HAS_VALUE != 0 {
                        if self.class_id() == JS_CLASS_MODULE_NS {
                            /* JS_PROP_WRITABLE is always true for variable
                            references, but they are write protected in module name
                            spaces. */
                            if !val.is_same(&vref.value()) {
                                return if ctx
                                    .JS_ThrowTypeErrorOrFalse(flags, "property is not configurable")
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
                            return if ctx.JS_ThrowTypeErrorOrFalse(
                                flags,
                                "module namespace properties have writable = false",
                            ) {
                                Err(-1)
                            } else {
                                Ok(false)
                            };
                        }

                        self.shape_prepare_update(ctx)?;
                        self.0.prop_values.borrow_mut()[idx] =
                            JSProperty::Normal(vref.value().clone());
                        self.shape().unwrap().with_mut(|s| {
                            let prs = s.prop_defs.get_mut(&prop.0).unwrap();
                            prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                        });
                    }
                } else {
                    unreachable!()
                }
                return Ok(true);
            }

            _ => {
                unreachable!();
            }
        }

        //
        // fill value
        //
        debug_assert!(matches!(
            prs_flags & JS_PROP_TMASK,
            JS_PROP_NORMAL | JS_PROP_GETSET
        ));

        if prs_flags & JS_PROP_LENGTH != 0 {
            // special property: length
            debug_assert_eq!(idx, 0, "length property must be index at 0");

            let res = if flags & JS_PROP_HAS_VALUE != 0 {
                // Note: no JS code is executable because 'val' is guaranted to be a Uint32
                self.set_array_length(ctx, val, flags)
            } else {
                Ok(true)
            };

            // still need to reset the writable flag if needed.
            // The JS_PROP_LENGTH is kept because the Uint32 test is still done if the length
            // property is read-only.
            if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                self.js_update_property_flags(ctx, prop, prs_flags & !JS_PROP_WRITABLE)?;
            }

            return res;
        } else {
            if flags & JS_PROP_HAS_VALUE != 0 {
                self.0.prop_values.borrow_mut()[idx] = JSProperty::Normal(val);
            }
            if flags & JS_PROP_HAS_WRITABLE != 0 {
                self.js_update_property_flags(
                    ctx,
                    prop,
                    (prs_flags & !JS_PROP_WRITABLE) | (flags & JS_PROP_WRITABLE),
                )?;
            }
        }

        Ok(true)
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
            getter.object()
        } else {
            None
        };
        let new_setter = if flags & JS_PROP_HAS_SET != 0 && setter.is_callable() {
            setter.object()
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
                    return if ctx.JS_ThrowTypeErrorOrFalse(flags, "property is not configurable") {
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

    /// overwrite property value at specified slot idx.
    /// if the property is auto init type, do instantiate instead.
    pub(super) fn put_prop_slot(
        &self,
        ctx: &JSContext,
        prs_flags: u32,
        prop: JSAtom,
        slot_idx: usize,
        value: JSValue,
        this_obj: &JSObject,
        flags: u32,
    ) -> Result<bool, i32> {
        if prs_flags & JS_PROP_LENGTH != 0 {
            // special prop: length
            // to set .length property of an array object
            debug_assert_eq!(self.class_id(), JS_CLASS_ARRAY);
            debug_assert_eq!(prop, JS_ATOM_length);
            debug_assert_eq!(slot_idx, 0, "length property must be indexed at 0");
            return self.set_array_length(ctx, value, flags);
        }

        if prs_flags & JS_PROP_AUTOINIT == JS_PROP_AUTOINIT {
            // lazy init property
            let mut pr = std::mem::take(&mut self.0.prop_values.borrow_mut()[slot_idx]);
            pr.instantiate(self, prop, this_obj)?;
            debug_assert!(matches!(pr, JSProperty::Normal(_)));
            debug_assert_eq!(
                self.find_own_property(prop).unwrap().1 & JS_PROP_TMASK,
                JS_PROP_NORMAL
            );
            self.0.prop_values.borrow_mut()[slot_idx] = pr;
            Ok(true)
        } else {
            // normal, getter, setter
            let pr = self.0.prop_values.borrow()[slot_idx].clone();
            match pr {
                JSProperty::Normal(_) => {
                    if prs_flags & JS_PROP_WRITABLE == 0 {
                        return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }

                    self.0.prop_values.borrow_mut()[slot_idx] = JSProperty::Normal(value);
                    Ok(true)
                }
                JSProperty::GetSet { setter, .. } => call_setter(
                    ctx,
                    setter.as_ref(),
                    &JSValue::Object(self.clone()),
                    value,
                    flags,
                ),
                JSProperty::VarRef(vref) => {
                    // JS_PROP_WRITABLE is always true for variable references,
                    // but they are write protected in module name spaces.
                    if self.class_id() == JS_CLASS_MODULE_NS {
                        if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                            Err(-1)
                        } else {
                            Ok(false)
                        }
                    } else {
                        vref.set_value(value);
                        Ok(true)
                    }
                }
                JSProperty::AutoInit { .. } => {
                    unreachable!()
                }
            }
        }
    }
}
