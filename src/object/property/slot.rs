use crate::{
    JS_CLASS_MODULE_NS, atom::Atom, branch_hints::unlikely, context::JSContext, object::JSObject,
    value::JSValue,
};

use super::{
    JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK,
    JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty,
};

impl<'a> JSObject<'a> {
    /// update value to property slot `idx`, handle 'writable' flag setting.
    pub(super) fn update_prop_slot_value(
        &self,
        ctx: &JSContext<'a>,
        prop: &Atom,
        prs_flags: u32,
        idx: usize,
        val: JSValue<'a>,
        flags: u32,
    ) -> Result<bool, i32> {
        //
        // 1. check configurable
        //
        match prs_flags & JS_PROP_TMASK {
            JS_PROP_NORMAL => {
                if unlikely(
                    prs_flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                        && flags & JS_PROP_HAS_VALUE != 0,
                ) {
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
                    let prs = s.prop_defs.get_mut(&prop.id()).unwrap();
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

        //
        // 2. update value, process 'writable' flag
        //
        if prs_flags & JS_PROP_TMASK == JS_PROP_VARREF {
            // varref property
            let slot = &mut self.0.prop_values.borrow_mut()[idx];

            if let JSProperty::VarRef(vref) = slot {
                if flags & JS_PROP_HAS_VALUE != 0 {
                    if unlikely(self.class_id() == JS_CLASS_MODULE_NS) {
                        /* JS_PROP_WRITABLE is always true for variable
                        references, but they are write protected in module name
                        spaces. */
                        if !val.is_same(&vref.value()) {
                            return if ctx.can_throw(flags) {
                                ctx.throw_type_error("property is not configurable");
                                Err(-1)
                            } else {
                                Ok(false)
                            };
                        }
                    } else {
                        vref.set_value(val);
                    }
                }

                if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                    // has {writable:false}
                    // 'writable' is set to false, no longer a reference (for mapped arguments)
                    if unlikely(self.class_id() == JS_CLASS_MODULE_NS) {
                        return if ctx.throw_type_error_or_false(
                            flags,
                            "module namespace properties have writable = false",
                        ) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    } else {
                        self.shape_prepare_update(ctx)?;
                        self.shape().unwrap().with_mut(|s| {
                            // Note: s must be an exclusive shape, so we can modify it.
                            let prs = s.prop_defs.get_mut(&prop.id()).unwrap();
                            prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                            debug_assert_eq!(prs.flags() & JS_PROP_TMASK, JS_PROP_NORMAL);
                        });
                        *slot = JSProperty::Normal(vref.value().clone());
                    }
                }

                Ok(true)
            } else {
                unreachable!();
            }
        } else if prs_flags & JS_PROP_LENGTH != 0 {
            // the length property
            let res = if flags & JS_PROP_HAS_VALUE != 0 {
                unsafe {
                    self.as_Array_unchecked().set_length(
                        ctx,
                        val.to_array_length(ctx, false)?,
                        ctx.can_throw(flags),
                    )
                }
            } else {
                Ok(true)
            };

            if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                // has {writable:false}
                let (_, prs_flags2) = self.find_shape_property(prop).unwrap();
                self.update_shape_property_flags(ctx, prop, prs_flags2 & !JS_PROP_WRITABLE)?;
            }

            res
        } else {
            // normal property
            if flags & JS_PROP_HAS_VALUE != 0 {
                self.0.prop_values.borrow_mut()[idx] = JSProperty::Normal(val);
            }

            // update `writable` flag
            if flags & JS_PROP_HAS_WRITABLE != 0 {
                let (_, prs_flags2) = self.find_shape_property(prop).unwrap();
                self.update_shape_property_flags(
                    ctx,
                    prop,
                    prs_flags2 & !JS_PROP_WRITABLE | (flags & JS_PROP_WRITABLE),
                )?;
            }

            Ok(true)
        }
    }

    /// update property as getset type at specified slot `idx`
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_prop_slot_getset(
        &self,
        ctx: &JSContext<'a>,
        prop: &Atom,
        prs_flags: u32,
        idx: usize,
        getter: JSValue<'a>,
        setter: JSValue<'a>,
        flags: u32,
    ) -> Result<bool, i32> {
        let new_getter = if flags & JS_PROP_HAS_GET != 0 && getter.is_function() {
            getter.as_object()
        } else {
            None
        };
        let new_setter = if flags & JS_PROP_HAS_SET != 0 && setter.is_function() {
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
                let prs = s.prop_defs.get_mut(&prop.id()).unwrap();
                prs.set_flags(
                    (prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET,
                );
            });
        } else {
            // alreay is getset type
            if let JSProperty::GetSet { getter, setter } =
                self.0.prop_values.borrow().get(idx).unwrap()
            {
                if unlikely(
                    prs_flags & JS_PROP_CONFIGURABLE == 0
                        && ((flags & JS_PROP_HAS_GET != 0 && new_getter != getter.as_ref())
                            || (flags & JS_PROP_HAS_SET != 0 && new_setter != setter.as_ref())),
                ) {
                    return if ctx.throw_type_error_or_false(flags, "property is not configurable") {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                } else {
                    if flags & JS_PROP_HAS_GET == 0 {
                        old_getter = getter.clone();
                    }
                    if flags & JS_PROP_HAS_SET == 0 {
                        old_setter = setter.clone();
                    }
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
}
