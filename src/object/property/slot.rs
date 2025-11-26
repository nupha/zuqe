use crate::{
    JS_CLASS_MODULE_NS, atom::Atom, context::ContextImpl, object::ObjectRef, utils::unlikely,
    value::JSValue,
};

use super::{
    JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK,
    JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty,
};

impl ObjectRef {
    /// update value to property slot `idx`, handle 'writable' flag setting.
    pub(super) fn update_prop_slot_value(
        &mut self,
        ctx: &ContextImpl,
        prop: Atom,
        prs_flags: u32,
        idx: usize,
        val: JSValue,
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
                    return if !val.is_same(self.prop_values[idx].value(ctx, JSValue::UNDEFINED)?) {
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
                self.modify_shape_property_flags(ctx, prop, |f| {
                    let f2 = f & !(JS_PROP_TMASK | JS_PROP_WRITABLE);
                    debug_assert_eq!(flags & JS_PROP_TMASK, JS_PROP_NORMAL);
                    f2
                })?;

                // self.shape_prepare_update(ctx)?;
                // let sh = self.shape().unwrap();
                // let prs = unsafe { sh.as_mut().prop_defs.get_mut(&prop.id()).unwrap() };
                // prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                // debug_assert_eq!(prs.flags() & JS_PROP_TMASK, JS_PROP_NORMAL);

                // set value slot
                self.prop_values[idx] = JSProperty::default();
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
            let class_id = self.class_id();

            if let JSProperty::VarRef(mut vref) = self.prop_values[idx] {
                if flags & JS_PROP_HAS_VALUE != 0 {
                    if unlikely(class_id == JS_CLASS_MODULE_NS) {
                        /* JS_PROP_WRITABLE is always true for variable
                        references, but they are write protected in module name
                        spaces. */
                        if !val.is_same(vref.value()) {
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
                    if unlikely(class_id == JS_CLASS_MODULE_NS) {
                        return if ctx.throw_type_error_or_false(
                            flags,
                            "module namespace properties have writable = false",
                        ) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    } else {
                        self.modify_shape_property_flags(ctx, prop, |f| {
                            let f2 = f & !(JS_PROP_TMASK | JS_PROP_WRITABLE);
                            debug_assert_eq!(f2 & JS_PROP_TMASK, JS_PROP_NORMAL);
                            f2
                        })?;

                        self.prop_values[idx] = JSProperty::Normal(vref.value());
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
                    self.as_Array_mut_unchecked().set_length(
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
                self.modify_shape_property_flags(ctx, prop, |f| f & !JS_PROP_WRITABLE)?;
            }

            res
        } else {
            // normal property
            if flags & JS_PROP_HAS_VALUE != 0 {
                self.prop_values[idx] = JSProperty::Normal(val);
            }

            // update `writable` flag
            if flags & JS_PROP_HAS_WRITABLE != 0 {
                self.modify_shape_property_flags(ctx, prop, |f| {
                    f & !JS_PROP_WRITABLE | (flags & JS_PROP_WRITABLE)
                })?;
            }

            Ok(true)
        }
    }

    /// update property as getset type at specified slot `idx`
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_prop_slot_getset(
        &mut self,
        ctx: &ContextImpl,
        prop: Atom,
        prs_flags: u32,
        idx: usize,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let new_getter = if flags & JS_PROP_HAS_GET != 0 && getter.is_function() {
            getter.as_object().copied()
        } else {
            None
        };
        let new_setter = if flags & JS_PROP_HAS_SET != 0 && setter.is_function() {
            setter.as_object().copied()
        } else {
            None
        };

        let mut old_getter: Option<ObjectRef> = None;
        let mut old_setter: Option<ObjectRef> = None;

        if prs_flags & JS_PROP_TMASK != JS_PROP_GETSET {
            // property is not getset type:
            // change shape property type and clear old property value.
            self.modify_shape_property_flags(ctx, prop, |f| {
                (f & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
            })?;

            // self.shape_prepare_update(ctx)?;
            // let sh = self.shape().unwrap();
            // let prs = unsafe { sh.as_mut().prop_defs.get_mut(&prop.id()).unwrap() };
            // prs.set_flags(
            //     (prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET,
            // );
        } else {
            // alreay is getset type
            if let JSProperty::GetSet { getter, setter } =
                self.prop_values.get(idx).as_ref().unwrap()
            {
                if unlikely(
                    prs_flags & JS_PROP_CONFIGURABLE == 0
                        && ((flags & JS_PROP_HAS_GET != 0 && new_getter != *getter)
                            || (flags & JS_PROP_HAS_SET != 0 && new_setter != *setter)),
                ) {
                    return if ctx.throw_type_error_or_false(flags, "property is not configurable") {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                } else {
                    if flags & JS_PROP_HAS_GET == 0 {
                        old_getter = *getter;
                    }
                    if flags & JS_PROP_HAS_SET == 0 {
                        old_setter = *setter;
                    }
                }
            } else {
                unreachable!()
            }
        }

        // modify property value
        self.prop_values[idx] = JSProperty::GetSet {
            getter: new_getter.or(old_getter),
            setter: new_setter.or(old_setter),
        };

        Ok(true)
    }
}
