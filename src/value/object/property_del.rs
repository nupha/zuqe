use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    array::convert_fast_array_to_array,
    atom::{JS_ATOM_MAX_INT, JS_ATOM_NULL, JS_FreeAtom, JSAtom},
    context::{JSContext, is_strict_mode},
    value::object::JSObject,
};

use super::{super::JSValue, JS_PROP_CONFIGURABLE, JS_PROP_THROW, JS_PROP_THROW_STRICT};

impl JSObject {
    /// delete a property in shape or by exotic handler.
    /// returns false if the property is not configurable; true is delete is successful.
    /// prop is consumed, and free'd once.
    pub fn delete_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
        let obj_flags = self.flags();

        loop {
            if let Some((idx, prs_flags)) = self
                .shape()
                .unwrap()
                .0
                .borrow()
                .prop_defs
                .get_full(&prop.0)
                .map(|(i, _, p)| (i, p.flags()))
            {
                // property found
                if prs_flags & JS_PROP_CONFIGURABLE == 0 {
                    return Ok(false);
                }

                self.shape_prepare_update(ctx)?;

                // remove property value
                drop(self.0.prop_values.borrow_mut().remove(idx));

                // remove shape property
                let prs = self
                    .shape()
                    .unwrap()
                    .0
                    .borrow_mut()
                    .prop_defs
                    .shift_remove(&prop.0)
                    .unwrap();
                debug_assert_eq!(prop, prs.atom);
                JS_FreeAtom(ctx, prop);

                return Ok(true);
            }

            // check for array index or exotic.
            if obj_flags.is_exotic() {
                if obj_flags.fast_array() {
                    if let Some(idx) = prop.to_array_index(ctx.rt()) {
                        match self.class_id() {
                            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                                match self
                                    .with_fast_array_values_mut(|lst| -> Result<bool, i32> {
                                        if idx == lst.len() - 1 {
                                            lst.pop();
                                            return Ok(true);
                                        } else if idx < lst.len() {
                                            // convert to slow array, then delete property
                                            if unsafe { convert_fast_array_to_array(ctx, self) }
                                                != 0
                                            {
                                                return Err(-1);
                                            }
                                            return Ok(false);
                                        } else {
                                            unreachable!()
                                        }
                                    })
                                    .unwrap()?
                                {
                                    true => {
                                        return Ok(true); // fast array done
                                    }
                                    false => {
                                        continue; // redo
                                    }
                                }
                            }
                            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                                if idx < self.array_length_unchecked() {
                                    // can't delete element of a TypedArray
                                    return Ok(false);
                                }
                            }
                            _ => {
                                unreachable!()
                            }
                        }
                    }
                } else {
                    // use exotic delete_property callback
                    let em = ctx.rt().class_array[self.class_id() as usize].exotic;
                    if !em.is_null() {
                        if let Some(cb) = unsafe { (*em).delete_property } {
                            return cb(ctx, JSValue::Object(self.clone()), prop);
                        }
                    }
                }
            }

            break;
        }

        // not found
        Ok(true)
    }
}

impl JSValue {
    /// 如果属性不可配置或无效对象则返回 false, 出现异常时返回 Err.
    /// flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT
    pub fn delete_property(&self, ctx: &JSContext, prop: JSAtom, flags: u32) -> Result<bool, i32> {
        if unsafe {
            self.coerce_to_object(ctx)?
                .object_unchecked()
                .delete_property(ctx, prop)?
        } {
            Ok(true)
        } else if flags & JS_PROP_THROW != 0
            || (flags & JS_PROP_THROW_STRICT != 0 && is_strict_mode(ctx))
        {
            ctx.JS_ThrowTypeError("could not delete property");
            Err(-1)
        } else {
            Ok(false)
        }
    }

    pub fn delete_property_i64(&self, ctx: &JSContext, idx: i64, flags: u32) -> Result<bool, i32> {
        if (idx as u64) <= JS_ATOM_MAX_INT as u64 {
            // fast path for fast arrays
            return self.delete_property(ctx, JSAtom::new_u32(idx as u32), flags);
        }

        let prop = ctx.rt_mut().register_atom_i64(idx);
        if prop == JS_ATOM_NULL {
            return Err(-1);
        }

        let res = self.delete_property(ctx, prop, flags);
        JS_FreeAtom(ctx, prop);
        res
    }
}
