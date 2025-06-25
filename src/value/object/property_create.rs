use crate::{
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    array::convert_fast_array_to_array,
    atom::{__JS_AtomToUInt32, JS_AtomIsNumericIndex, JSAtom, consts::*},
    context::JSContext,
    value::object::{JSObjectInner, JSShape},
};

use super::{
    super::JSValue, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET,
    JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE,
    JSObject, JSProperty, get_prop_flags,
};

impl JSObjectInner {
    /// Grows the property values array to accommodate more properties.
    /// Takes a new length and extends the array with undefined values if needed.
    /// Returns error if memory allocation fails.
    pub(super) fn grow_prop_value_slots(&self, total_length: usize) -> Result<(), i32> {
        let mut slots = self.prop_values.borrow_mut();

        let len = slots.len();
        if total_length > len {
            let delta = total_length - len;
            slots.try_reserve_exact(delta).map_err(|_| -1)?;

            // init extended values
            for _ in 0..delta {
                slots.push(JSProperty::default());
            }
        }

        Ok(())
    }

    /// Add an own property to shape and enlarge prop value slots.
    /// This function implies that the new prop should be add to shape (owned property),
    /// and it does not exist yet.
    /// Returns Err if memory error.
    /// `prop` is not consumed.
    fn do_add_own_property<R>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prop_flags: u32,
        then: Option<impl FnOnce(&mut JSProperty) -> R>,
    ) -> Result<Option<R>, i32> {
        let mut shape = self.shape.borrow_mut().take().unwrap();
        let mut shape0: Option<JSShape> = None;

        if shape.is_hashed() {
            // try to find an existing shape
            match ctx.rt().find_shape_with_prop(&shape, prop, prop_flags) {
                Some(found) => {
                    let new_len = found.len();
                    let len = shape.len();
                    debug_assert!(new_len > len);

                    if self.grow_prop_value_slots(new_len).is_err() {
                        ctx.JS_ThrowOutOfMemory();
                        self.shape.borrow_mut().replace(shape); // restore old shape
                        return Err(-1);
                    }

                    // use found shape
                    self.shape.borrow_mut().replace(found);

                    return if let Some(f) = then {
                        Ok(Some(f(self
                            .prop_values
                            .borrow_mut()
                            .get_mut(new_len - 1)
                            .unwrap())))
                    } else {
                        Ok(None)
                    };
                }
                None => {
                    if shape.strong_count() != 1 {
                        // current shape is shared, clone and use the new
                        let new_shape = shape.clone_to_new(ctx)?;
                        shape0.replace(shape);
                        shape = new_shape;
                    }
                }
            };
        }

        // Note: shape must be exclusive now, so that is can be modified.
        // add shape property, enlarge object prop value slot.
        let len = if let Ok(n) = shape.add_shape_property(ctx, prop, prop_flags, Some(self)) {
            n
        } else {
            // fail to modify shape: restore old shape for object
            if let Some(sh0) = shape0 {
                self.shape.borrow_mut().replace(sh0);
            } else {
                // shape was unregistered
                ctx.rt_mut().register_shape(&shape);
                self.shape.borrow_mut().replace(shape);
            }
            return Err(-1);
        };

        // let object use modified shape
        self.shape.borrow_mut().replace(shape.clone());

        if let Some(f) = then {
            Ok(Some(f(&mut self.prop_values.borrow_mut()[len - 1])))
        } else {
            Ok(None)
        }
    }

    /// Add an own property to shape and enlarge prop value slots.
    /// This function implies that the new prop should be add to shape (owned property),
    /// and it does not exist yet.
    /// Returns Err if memory error.
    /// `prop` is not consumed.
    #[inline(always)]
    pub(super) fn add_own_property_then<R>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prop_flags: u32,
        then: impl FnOnce(&mut JSProperty) -> R,
    ) -> Result<R, i32> {
        self.do_add_own_property(ctx, prop, prop_flags, Some(then))
            .map(|r| r.unwrap())
    }
}

impl JSObject {
    /// Add a property to shape and enlarge prop value slots,
    /// the property value is initialized to undefined.
    /// Returns Err if memory error.
    /// `prop` is not consumed.
    #[inline]
    pub(crate) fn add_property_then<R>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prop_flags: u32,
        then: impl FnOnce(&mut JSProperty) -> R,
    ) -> Result<R, i32> {
        self.0.add_own_property_then(ctx, prop, prop_flags, then)
    }

    // /// Add a property to shape and enlarge prop value slots,
    // /// the property value is initialized to undefined.
    // /// Usually used to add a get/set or lazy prop.
    // ///
    // /// Returns Err if memory error.
    // /// `prop` is not consumed.
    // #[inline]
    // pub fn add_property(&self, ctx: &JSContext, prop: JSAtom, prop_flags: u32) -> Result<(), i32> {
    //     self.0
    //         .do_add_property(
    //             ctx,
    //             prop,
    //             prop_flags,
    //             Option::<Box<dyn FnOnce(&mut JSProperty)>>::None,
    //         )
    //         .map(|r| r.unwrap())
    // }

    /// Similar to define_property(), usually for new instance property initialization.
    /// `prop` is not consumed.
    pub fn create_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        macro_rules! update_array_length {
            ($p:expr, $idx:expr) => {
                let len = $p.0.prop_values.borrow()[0]
                    .value(ctx, &JSValue::UNDEFINED)
                    .coerce_to_i32_free(ctx)? as usize;

                if ($idx as usize + 1) > len {
                    // prop #0 of Array is length
                    let prop0_flags = $p
                        .shape()
                        .map(|sh| sh.0.borrow().prop_defs.first().unwrap().1.flags())
                        .unwrap();

                    if prop0_flags & JS_PROP_WRITABLE == 0 {
                        return if ctx.JS_ThrowTypeErrorReadOnly(flags, JS_ATOM_length) {
                            Ok(false)
                        } else {
                            Err(-1)
                        };
                    }

                    $p.0.prop_values.borrow_mut()[0] =
                        JSProperty::Normal(JSValue::from($idx as u32 + 1))
                }
            };
        }

        macro_rules! convert_to_array {
            ($p:expr, $idx:expr) => {
                if unsafe { convert_fast_array_to_array(ctx, $p) } != 0 {
                    return Err(-1);
                }
                update_array_length!($p, $idx);
            };
        }

        // add a new property or modify an existing exotic one
        let obj_flags = self.flags();
        if obj_flags.is_exotic() {
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    let idx: u32;

                    if obj_flags.fast_array() {
                        if prop.is_int_type() {
                            idx = __JS_AtomToUInt32(prop);
                            if idx == self.array_length_unchecked() as u32 {
                                if !self.flags().extensible() {
                                    return if ctx.JS_ThrowTypeErrorOrFalse(
                                        flags as _,
                                        "object is not extensible",
                                    ) {
                                        Err(-1)
                                    } else {
                                        Ok(false)
                                    };
                                }

                                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                    convert_to_array!(self, idx);
                                }
                                let prop_flags = get_prop_flags(flags, 0);
                                if prop_flags != JS_PROP_C_W_E {
                                    convert_to_array!(self, idx);
                                }
                                return self.add_fast_array_element(ctx, val, flags);
                                //return add_fast_array_element(ctx, self, val.dup(), flags);
                            } else {
                                convert_to_array!(self, idx);
                            }
                        } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            // convert the fast array to normal array
                            convert_to_array!(self, idx);
                        }
                    } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                        update_array_length!(self, idx);
                    }
                }
                JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                    let ret = unsafe { JS_AtomIsNumericIndex(ctx, prop) };
                    if ret != 0 {
                        if ret < 0 {
                            return Err(-1);
                        }
                        return if ctx.JS_ThrowTypeErrorOrFalse(
                            flags,
                            "cannot create numeric index in typed array",
                        ) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }
                }
                cid => {
                    if flags & JS_PROP_NO_EXOTIC == 0 {
                        let em = ctx.rt().class_array[cid as usize].exotic;
                        if !em.is_null() {
                            if let Some(cb) = unsafe { (*em).define_own_property } {
                                let r = cb(
                                    ctx,
                                    &JSValue::Object(self.clone()),
                                    prop,
                                    &val,
                                    &getter,
                                    &setter,
                                    flags as _,
                                );
                                return if r < 0 { Err(-1) } else { Ok(r != 0) };
                            }

                            if !self.is_extensible(ctx)? {
                                return if ctx.JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    "object is not extensible",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }
                        }
                    }
                }
            }
        }

        // If prop is not an exotic property, nor an array element:
        // add shape property and put the value in value slot.

        if !obj_flags.extensible() {
            return if ctx.JS_ThrowTypeErrorOrFalse(flags, "object is not extensible") {
                Err(-1)
            } else {
                Ok(false)
            };
        }

        let prop_flags = if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
            (flags & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
        } else {
            flags & JS_PROP_C_W_E
        };

        self.add_property_then(ctx, prop, prop_flags, |pr| {
            *pr = if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                // getset property
                JSProperty::GetSet {
                    getter: if (flags & JS_PROP_HAS_GET) != 0 && getter.is_callable() {
                        getter.object().cloned()
                    } else {
                        None
                    },
                    setter: if (flags & JS_PROP_HAS_SET) != 0 && setter.is_callable() {
                        setter.object().cloned()
                    } else {
                        None
                    },
                }
            } else {
                // normal property
                JSProperty::Normal(if flags & JS_PROP_HAS_VALUE != 0 {
                    val
                } else {
                    JSValue::UNDEFINED
                })
            };
        })?;

        Ok(true)
    }
}

/// `val` is duplicated inside
#[deprecated(note = "use JSObject::create_property() instead")]
pub unsafe fn JS_CreateProperty(
    ctx: &JSContext,
    p: *mut JSValue,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    (*p).object_unchecked()
        .create_property(ctx, prop, val, getter, setter, flags)
}
