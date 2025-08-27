use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{JS_ATOM_MAX_INT, JS_FreeAtom, JSAtom},
    context::{JSContext, is_strict_mode},
    object::JSObject,
    value::JSValue,
};

use super::{JS_PROP_CONFIGURABLE, JS_PROP_THROW, JS_PROP_THROW_STRICT};

impl JSObject {
    /// delete an own property in shape or by exotic handler.
    ///
    /// Returns:
    /// - false if the property is not configurable;
    /// - true if successfully deleted.
    ///
    /// Note: `prop` is not consumed.
    pub fn delete_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
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

            // check for array index or exotic.
            if obj_flags.is_exotic() {
                if obj_flags.is_fast_array() {
                    if let Some(idx) = prop.to_index(ctx.rt()) {
                        match self.class_id() {
                            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                                if self
                                    .with_fast_array_values_mut(|lst| -> Result<bool, i32> {
                                        if idx == lst.len() - 1 {
                                            // remove the last
                                            lst.pop();
                                            Ok(true)
                                        } else if idx < lst.len() {
                                            // should convert to sparse array, then delete property
                                            Ok(false)
                                        } else {
                                            unreachable!()
                                        }
                                    })
                                    .unwrap()?
                                {
                                    // early exit for fast array
                                    return Ok(true);
                                } else {
                                    // convert to sparse array, then do delete
                                    self.destruct_fast_array(ctx)?;
                                    continue;
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

    /// 如果属性不可配置或无效对象则返回 false, 出现异常时返回 Err.
    /// flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT
    pub fn delete_property_ex(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        flags: u32,
    ) -> Result<bool, i32> {
        match self.delete_property(ctx, prop) {
            Ok(b) => Ok(b),
            Err(_) => {
                if flags & JS_PROP_THROW != 0
                    || (flags & JS_PROP_THROW_STRICT != 0 && is_strict_mode(ctx))
                {
                    ctx.throw_type_error("could not delete property");
                    Err(-1)
                } else {
                    Ok(false)
                }
            }
        }
    }
}

impl JSValue {
    /// 如果属性不可配置或无效对象则返回 false, 出现异常时返回 Err.
    /// flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT
    pub fn delete_property(&self, ctx: &JSContext, prop: JSAtom, flags: u32) -> Result<bool, i32> {
        self.to_object(ctx)
            .and_then(|o| o.delete_property_ex(ctx, prop, flags))
    }

    pub fn delete_property_i64(&self, ctx: &JSContext, idx: i64, flags: u32) -> Result<bool, i32> {
        if (idx as u64) <= JS_ATOM_MAX_INT as u64 {
            // fast path for fast arrays
            return self.delete_property(ctx, JSAtom::new_u32(idx as u32), flags);
        }

        let prop = JSAtom::from_i64(ctx.rt_mut(), idx)?;
        let res = self.delete_property(ctx, prop, flags);
        JS_FreeAtom(ctx, prop);
        res
    }
}
