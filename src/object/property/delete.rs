// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{Atom, JS_ATOM_MAX_INT},
    context::ScopedContext,
    object::{JS_PROP_FORCE, JSProperty, Object, ObjectImpl},
    utils::unlikely,
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
        force: bool,
    ) -> Result<bool, i32> {
        let obj_flags = self.flags();

        loop {
            // find in shape properties
            if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
                return if !force && (prs_flags & JS_PROP_CONFIGURABLE == 0) {
                    // property is not configurable and not-force: can't delete
                    Ok(false)
                } else {
                    // remove shape property
                    let compact = self.modify_shape(ctx, |sh| {
                        sh.prop_defs.remove_index(idx);

                        // 1/8+ deletion: do slot compact
                        if (sh.prop_defs.count_deleted() << 3) >= sh.prop_defs.len() {
                            sh.prop_defs.compact();
                            true
                        } else {
                            false
                        }
                    })?;

                    // mark property value as deleted
                    self.set_prop_slot_non_gc(idx, JSProperty::Deleted);

                    if compact {
                        // compact prop values list too
                        self.prop_values
                            .retain(|p| !matches!(p, JSProperty::Deleted));
                    }

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
                                    let arr_data = self.payload_mut().array_mut().unwrap();
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

    #[inline(always)]
    pub fn delete_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        flags: u32,
    ) -> Result<bool, i32> {
        if self.delete_property_internal(ctx, prop, flags & JS_PROP_FORCE != 0)? {
            Ok(true)
        } else {
            // can't delete (not configurable)
            if ctx.rt().can_throw(flags) {
                ctx.throw_type_error("could not delete property");
                Err(-1)
            } else {
                Ok(false)
            }
        }
    }

    #[inline]
    pub fn delete_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        str: &str,
        flags: u32,
    ) -> Result<bool, i32> {
        self.delete_property(ctx, Atom::from_str(ctx.rt_ptr(), str)?, flags)
    }

    pub fn delete_property_i64(
        &mut self,
        ctx: &mut ScopedContext,
        idx: i64,
        flags: u32,
    ) -> Result<bool, i32> {
        if (0..JS_ATOM_MAX_INT as i64).contains(&idx) {
            // fast path for fast array
            self.delete_property(ctx, Atom::new_small_int(idx as u32), flags)
        } else {
            let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
            self.delete_property(ctx, atom, flags)
        }
    }
}

impl JSValue {
    /// 如果属性不可配置或无效对象则返回 false, 出现异常时返回 Err.
    /// flags 可以是 0, JS_PROP_FORCE, JS_PROP_THROW, JS_PROP_THROW_STRICT
    #[inline(always)]
    pub fn delete_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        flags: u32,
    ) -> Result<bool, i32> {
        self.objectify(ctx)
            .and_then(|mut o| o.delete_property(ctx, prop, flags))
    }

    #[inline]
    pub fn delete_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        str: &str,
        flags: u32,
    ) -> Result<bool, i32> {
        self.objectify(ctx)
            .and_then(|mut o| o.delete_property_str(ctx, str, flags))
    }

    #[inline]
    pub fn delete_property_i64(
        &mut self,
        ctx: &mut ScopedContext,
        idx: i64,
        flags: u32,
    ) -> Result<bool, i32> {
        self.objectify(ctx)
            .and_then(|mut o| o.delete_property_i64(ctx, idx, flags))
    }
}
