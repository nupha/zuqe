use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{JS_ATOM_MAX_INT, JS_FreeAtom, JSAtom},
    context::JSContext,
    object::JSObject,
    value::JSValue,
};

use super::JS_PROP_CONFIGURABLE;

impl JSObject {
    /// Delete an own property.
    ///
    /// The property is in shape, or delete via exotic/fast-array handler.
    ///
    /// Returns:
    /// - Ok(false) if property is not configurable;
    /// - Ok(true) if successfully deleted.
    /// - Err() on failure. e.g. fail to prepare shape update, fail to convert fast array to sparse array etc.
    ///
    /// Note: `prop` is not consumed.
    pub(crate) fn delete_property_internal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
    ) -> Result<bool, i32> {
        let obj_flags = self.flags();

        loop {
            // find in shape properties
            if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
                if prs_flags & JS_PROP_CONFIGURABLE == 0 {
                    return Ok(false);
                }

                self.shape_prepare_update(ctx)?;

                // remove shape property defination.
                // shape hash is unchanged, even a field is removed.
                let prs = self
                    .shape()
                    .unwrap()
                    .0
                    .borrow_mut()
                    .prop_defs
                    .shift_remove(&prop.0)
                    .unwrap();

                JS_FreeAtom(ctx, prs.atom);

                // remove property value
                drop(self.0.prop_values.borrow_mut().remove(idx));

                return Ok(true);
            }

            // for array index or exotic.
            if obj_flags.is_exotic() {
                if obj_flags.is_fast_array() {
                    if let Some(idx) = prop.to_array_index(ctx.rt()) {
                        match self.class_id() {
                            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                                let cnt = self.fast_array_count();
                                if idx < cnt {
                                    if self
                                        .with_fast_array_values_mut(|lst| -> bool {
                                            debug_assert_eq!(lst.len(), cnt as usize);
                                            if idx == lst.len() as u32 - 1 {
                                                // remove the last.
                                                // returns true to indicate delete is done.
                                                lst.pop();
                                                true
                                            } else {
                                                // returns false to
                                                // indicate convert to sparse array, then delete property
                                                false
                                            }
                                        })
                                        .unwrap()
                                    {
                                        // early exit for fast array
                                        return Ok(true);
                                    } else {
                                        // convert to sparse array, then do delete
                                        self.destruct_fast_array(ctx)?;
                                        continue;
                                    }
                                }
                            }
                            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                                if idx < self.fast_array_count() {
                                    // delete element of a TypedArray is not allowed.
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
                    if let Some(cb) = unsafe {
                        ctx.rt().class_defs[self.class_id() as usize]
                            .exotic
                            .as_ref()
                            .and_then(|m| m.delete_property)
                    } {
                        return cb(ctx, JSValue::Object(self.clone()), prop);
                    }
                }
            }

            break;
        }

        // prop not found
        Ok(true)
    }

    pub fn delete_property(&self, ctx: &JSContext, prop: JSAtom, flags: u32) -> Result<bool, i32> {
        if self.delete_property_internal(ctx, prop)? {
            Ok(true)
        } else {
            // not configurable
            if ctx.should_throw_error(flags) {
                ctx.throw_type_error("could not delete property");
                Err(-1)
            } else {
                Ok(false)
            }
        }
    }

    pub fn delete_property_i64(&self, ctx: &JSContext, idx: i64, flags: u32) -> Result<bool, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&idx) {
            // fast path for fast array
            self.delete_property(ctx, JSAtom::new_small_int(idx as u32), flags)
        } else {
            let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
            let res = self.delete_property(ctx, atom, flags);
            JS_FreeAtom(ctx, atom);
            res
        }
    }
}

impl JSValue {
    /// 如果属性不可配置或无效对象则返回 false, 出现异常时返回 Err.
    /// flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT
    #[inline]
    pub fn delete_property(&self, ctx: &JSContext, prop: JSAtom, flags: u32) -> Result<bool, i32> {
        self.clone()
            .ensure_object(ctx)
            .and_then(|o| o.delete_property(ctx, prop, flags))
    }

    #[inline]
    pub fn delete_property_i64(&self, ctx: &JSContext, idx: i64, flags: u32) -> Result<bool, i32> {
        self.clone()
            .ensure_object(ctx)
            .and_then(|o| o.delete_property_i64(ctx, idx, flags))
    }
}
