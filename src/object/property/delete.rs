// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::Atom,
    context::ScopedContext,
    function::prop_str_to_atom,
    object::{JSProperty, Object, ObjectImpl, ThrowPolicy},
    value::JSValue,
};

use super::JS_PROP_CONFIGURABLE;

impl ObjectImpl {
    /// Delete an own property.
    ///
    /// The property is in shape, or delete via exotic/fast-array handler.
    ///
    /// Returns:
    /// - Ok(false) if property is not configurable;
    /// - Ok(true) if successfully deleted.
    /// - Err() on failure. e.g. fail to prepare shape update, fail to convert fast array to sparse array etc.
    pub(crate) fn delete_property_internal(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
    ) -> Result<bool, i32> {
        let obj_flags = self.flags();

        loop {
            // find in shape properties
            if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
                return if prs_flags & JS_PROP_CONFIGURABLE == 0 {
                    // property is not configurable: can't delete
                    Ok(false)
                } else {
                    // Optimized: do not modify shape, only mark slot as Deleted
                    self.set_prop_slot_non_gc(idx, JSProperty::Deleted);
                    self.deleted_slot_count += 1;
                    Ok(true)
                };
            }

            // for array index or exotic.
            if obj_flags.is_exotic() {
                if obj_flags.is_fast_array() {
                    if let Some(idx) = prop.to_array_index(ctx.rt()) {
                        match self.class_id() {
                            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                                let cnt = self.fast_array_count();
                                if idx < cnt {
                                    let arr_data = self.payload_array_mut().unwrap();
                                    debug_assert_eq!(arr_data.len(), cnt as usize);
                                    if idx == arr_data.len() as u32 - 1 {
                                        arr_data.pop();
                                        return Ok(true); // returns true to indicate delete is done.
                                    }

                                    // convert to sparse array then perform delete again
                                    self.destruct_fast_array(ctx)?;
                                    continue;
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
                    if let Some(cb) = ctx.rt().class_registry[self.class_id() as usize]
                        .exotic
                        .as_ref()
                        .and_then(|m| m.delete_property)
                    {
                        return cb(ctx, Object::from_ref(self).into(), prop);
                    }
                }
            }

            break;
        }

        // prop not found
        Ok(true)
    }

    /// Delete `prop` under the given `throw` policy.
    fn delete_property_throw(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        throw: ThrowPolicy,
    ) -> Result<bool, i32> {
        self.delete_property_internal(ctx, prop).and_then(|b| {
            if !b && throw.can_throw(ctx.rt().is_strict_mode()) {
                // can't delete (not configurable) and can throw
                Err(ctx.throw_type_error("could not delete property"))
            } else {
                Ok(b)
            }
        })
    }

    /// `[[Delete]]` for the `delete` operator: mode-faithful, i.e. returns
    /// `false` in sloppy mode and throws a TypeError in strict mode.
    #[inline(always)]
    pub fn delete_property(&mut self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        self.delete_property_throw(ctx, prop, ThrowPolicy::StrictOnly)
    }

    #[inline]
    pub fn delete_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        prop: &str,
    ) -> Result<bool, i32> {
        let atom = prop_str_to_atom(ctx, prop)?;
        self.delete_property(ctx, atom)
    }

    /// Index-keyed `DeletePropertyOrThrow`: always throws on failure.
    #[inline]
    pub fn delete_property_i64(&mut self, ctx: &mut ScopedContext, idx: i64) -> Result<bool, i32> {
        let atom = Atom::new_i64(ctx, idx)?;
        self.delete_property_throw(ctx, atom, ThrowPolicy::Always)
    }
}

impl JSValue {
    /// 如果属性不可配置或无效对象则返回 false, 出现异常时返回 Err.
    /// mode-faithful `[[Delete]]` (strict-only throw): see `ObjectImpl::delete_property`.
    #[inline]
    pub fn delete_property(&mut self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        self.objectify(ctx)
            .and_then(|mut o| o.delete_property(ctx, prop))
    }

    #[inline]
    pub fn delete_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        prop: &str,
    ) -> Result<bool, i32> {
        self.objectify(ctx)
            .and_then(|mut o| o.delete_property_str(ctx, prop))
    }

    #[inline]
    pub fn delete_property_i64(&mut self, ctx: &mut ScopedContext, idx: i64) -> Result<bool, i32> {
        self.objectify(ctx)
            .and_then(|mut o| o.delete_property_i64(ctx, idx))
    }
}
