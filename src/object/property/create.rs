use crate::{
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    array::convert_fast_array_to_array,
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
    /// Similar to define_property(), usually for new instance property initialization.
    /// If property already exists, modify its slot value; otherwise add own property and set slot value.
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
                    .coerce_to_i32(ctx)? as usize;

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
                        let em = ctx.rt().class_defs[cid as usize].exotic;
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

        // If prop is not an exotic property, nor an array element:
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

        self.add_shape_property_then(ctx, prop, prop_flags, |pr| {
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
