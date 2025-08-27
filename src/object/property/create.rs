use crate::{
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{JS_AtomIsNumericIndex, JSAtom, consts::*},
    context::JSContext,
    value::JSValue,
};

use super::{
    super::JSObject, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET,
    JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE,
    JSProperty, get_prop_flags,
};

impl JSObject {
    /// Similar to define_property(), usually used to initialize property for new instance.
    ///
    /// - If property already exists, update its value;
    /// - If property is integer and self is fast array,
    ///   update indexed value. Will destruct fast array to sparse array when needed.
    /// - otherwise, call `add_own_property()` to add a shape property.
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
        let update_array_length_fn = |arr: &JSObject, idx: u32| -> Option<Result<bool, i32>> {
            // Note: array length must <= u32::MAX
            if idx < u32::MAX {
                let cur_len = arr.0.prop_values.borrow()[0]
                    .value(ctx, &JSValue::UNDEFINED)
                    .and_then(|v| v.to_length(ctx));

                match cur_len {
                    Ok(len0) => {
                        let new_len = idx + 1;
                        if new_len as i64 > len0 {
                            // prop index 0 is `length`
                            let prop0_flags = arr
                                .shape()
                                .map(|sh| sh.0.borrow().prop_defs.first().unwrap().1.flags())
                                .unwrap();

                            if prop0_flags & JS_PROP_WRITABLE == 0 {
                                return Some(
                                    if ctx.JS_ThrowTypeErrorReadOnly(flags, JS_ATOM_length) {
                                        Ok(false)
                                    } else {
                                        Err(-1)
                                    },
                                );
                            }

                            arr.0.prop_values.borrow_mut()[0] =
                                JSProperty::Normal(JSValue::from(new_len));
                        }
                        None
                    }
                    Err(e) => Some(if ctx.can_throw_error(flags) {
                        Err(e)
                    } else {
                        Ok(false)
                    }),
                }
            } else {
                None
            }
        };

        let obj_flags = self.flags();

        // add a new property or modify an existing exotic one
        if obj_flags.is_exotic() {
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    if obj_flags.is_fast_array() {
                        if let Some(n) = prop.to_small_int() {
                            let idx = n;

                            if idx == self.fast_array_count() {
                                if !self.flags().extensible() {
                                    return if ctx.throw_type_error_or_false(
                                        flags as _,
                                        "object is not extensible",
                                    ) {
                                        Err(-1)
                                    } else {
                                        Ok(false)
                                    };
                                }

                                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                    if let Err(e) = self.destruct_fast_array(ctx) {
                                        return if ctx.can_throw_error(flags) {
                                            Err(e)
                                        } else {
                                            Ok(false)
                                        };
                                    }
                                    if let Some(ret) = update_array_length_fn(self, idx) {
                                        return ret;
                                    }
                                }

                                let prop_flags = get_prop_flags(flags, 0);
                                if prop_flags != JS_PROP_C_W_E {
                                    match self.destruct_fast_array(ctx) {
                                        Ok(_) => {
                                            if let Some(ret) = update_array_length_fn(self, idx) {
                                                return ret;
                                            }
                                        }
                                        Err(e) => {
                                            return if ctx.can_throw_error(flags) {
                                                Err(e)
                                            } else {
                                                Ok(false)
                                            };
                                        }
                                    }
                                } else {
                                    // fast array append element.
                                    return self.add_fast_array_element(ctx, val, flags);
                                }
                            } else {
                                if let Err(e) = self.destruct_fast_array(ctx) {
                                    return if ctx.can_throw_error(flags) {
                                        Err(e)
                                    } else {
                                        Ok(false)
                                    };
                                }
                                if let Some(ret) = update_array_length_fn(self, idx) {
                                    return ret;
                                }
                            }
                        } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            // convert fast array to sparse array
                            if let Err(e) = self.destruct_fast_array(ctx) {
                                return if ctx.can_throw_error(flags) {
                                    Err(e)
                                } else {
                                    Ok(false)
                                };
                            }
                            if let Some(ret) = update_array_length_fn(self, idx) {
                                return ret;
                            }
                        }
                    } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                        if let Some(ret) = update_array_length_fn(self, idx) {
                            return ret;
                        }
                    }
                }
                JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                    let ret = unsafe { JS_AtomIsNumericIndex(ctx, prop) };
                    if ret != 0 {
                        if ret < 0 {
                            return Err(-1);
                        }
                        return if ctx.throw_type_error_or_false(
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
                        if let Some(em) =
                            unsafe { ctx.rt().class_defs[cid as usize].exotic.as_ref() }
                        {
                            if let Some(cb) = em.define_own_property {
                                let r = cb(
                                    ctx,
                                    &JSValue::Object(self.clone()),
                                    prop,
                                    val,
                                    getter,
                                    setter,
                                    flags as _,
                                );
                                return if r < 0 {
                                    if ctx.can_throw_error(flags) {
                                        Err(-1)
                                    } else {
                                        Ok(false)
                                    }
                                } else {
                                    Ok(r != 0)
                                };
                            }

                            if !self.is_extensible(ctx).unwrap_or(false) {
                                return if ctx.throw_type_error_or_false(
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

        // Generic: if prop is not an exotic property, nor an array element:
        // add shape property and put the value in value slot.

        if !obj_flags.extensible() {
            return if ctx.throw_type_error_or_false(flags, "object is not extensible") {
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

        self.add_own_property(ctx, prop, prop_flags, {
            if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                // getset property
                JSProperty::GetSet {
                    getter: if flags & JS_PROP_HAS_GET != 0 && getter.is_callable() {
                        getter.object().cloned()
                    } else {
                        None
                    },
                    setter: if flags & JS_PROP_HAS_SET != 0 && setter.is_callable() {
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
            }
        })?;

        Ok(true)
    }
}
