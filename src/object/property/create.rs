use crate::{
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{__JS_AtomToUInt32, JS_AtomIsNumericIndex, JSAtom, consts::*},
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
    /// If property already exists, modify its slot value;
    /// otherwise add as own property and set slot value.
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
                let lenval = $p.0.prop_values.borrow()[0]
                    .value(ctx, &JSValue::UNDEFINED)?
                    .to_i32(ctx);

                match lenval {
                    Ok(len) => {
                        if ($idx as i32 + 1) > len {
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
                    }
                    Err(e) => {
                        return if ctx.can_throw_error(flags) {
                            Err(e)
                        } else {
                            Ok(false)
                        }
                    }
                }
            };
        }

        // add a new property or modify an existing exotic one
        let obj_flags = self.flags();
        if obj_flags.is_exotic() {
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    let idx: u32;

                    if obj_flags.is_fast_array() {
                        if prop.is_int_type() {
                            idx = __JS_AtomToUInt32(prop);
                            if idx == self.array_length_unchecked() as u32 {
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
                                    update_array_length!(self, idx);
                                }

                                let prop_flags = get_prop_flags(flags, 0);
                                if prop_flags != JS_PROP_C_W_E {
                                    match self.destruct_fast_array(ctx) {
                                        Ok(_) => {
                                            update_array_length!(self, idx);
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
                                    // fast array: exit early for fast array
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
                                update_array_length!(self, idx);
                            }
                        } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            if let Err(e) = self.destruct_fast_array(ctx) {
                                return if ctx.can_throw_error(flags) {
                                    Err(e)
                                } else {
                                    Ok(false)
                                };
                            }
                            update_array_length!(self, idx);
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
