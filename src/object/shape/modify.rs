// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    object::{JSProperty, ObjectImpl, ShapeImpl},
};

use smallvec::SmallVec;

impl ObjectImpl {
    /// modify object's shape by closure.
    /// an exclusive, unregistered shape is passed to closure as argument.
    pub(crate) fn modify_shape<R>(
        &mut self,
        ctx: &mut ScopedContext,
        update_fn: impl FnOnce(&mut ShapeImpl) -> R,
    ) -> Result<R, i32> {
        let use_count = self.shape().unwrap().use_count;
        let mut use_shape = if use_count > 1 {
            // shape is shared: dup to new
            self.shape().unwrap().dup(ctx)?
        } else {
            // shape is exclusive
            let sh = self.take_shape().unwrap();
            ctx.rt_mut().unregister_shape(&sh);
            sh
        };

        let r = update_fn(&mut use_shape);

        // batch compact deleted slots
        if self.deleted_slot_count > 0 {
            self.compact_deleted_slots(&mut use_shape);
        }

        ctx.rt_mut().register_shape(&use_shape)?;
        self.set_shape(use_shape);

        Ok(r)
    }

    /// Append an extra property to shape
    pub(crate) fn modify_shape_add_prop(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        prop_flags: u32,
    ) -> Result<usize, i32> {
        let (old_len, use_count) = {
            let sh = self.shape().unwrap();
            (sh.len(), sh.use_count)
        };
        debug_assert_eq!(self.prop_values.len(), old_len);

        let new_len = old_len + 1;
        self.try_extend_prop_slot(1).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        debug_assert_eq!(self.prop_values.len(), new_len);

        // find shape registry
        // ★ use_count == 1: exclusive shape — no other object could have created the target shape,
        // so global lookup would always miss. Skip the expensive HashMap GET entirely.

        // avoid perform ::find_shape_with_extend_prop() for use_count==1 case.
        if use_count != 1
            && let Some(found) =
                ctx.rt()
                    .find_shape_with_extend_prop(self.shape().unwrap(), prop, prop_flags)
        {
            // found existing shape
            debug_assert_eq!(new_len, found.len());
            self.set_shape(found);
            return Ok(new_len);
        }

        let mut use_shape = if use_count != 1 {
            // current shape is shared, dup to new without register.
            let mut sh2 = self.shape().unwrap().dup(ctx)?;
            sh2.prop_defs.try_reserve(1).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            sh2
        } else {
            // current shape is exclusive, unregister and use it.
            let mut sh = self.take_shape().unwrap();
            if sh.prop_defs.try_reserve(1).is_err() {
                self.set_shape(sh);
                ctx.throw_out_of_memory();
                return Err(-1);
            }
            ctx.rt_mut().unregister_shape(&sh);
            sh
        };

        debug_assert!(!ctx.rt().is_shape_registered(&use_shape));

        // shape can be modified now.
        use_shape.add_shape_properties(ctx, &[(prop, prop_flags)]);
        // register the shape
        ctx.rt_mut().register_shape(&use_shape).unwrap();
        // object uses the shape
        self.set_shape(use_shape);

        Ok(new_len)
    }

    /// Updates flag of a shape property.
    /// if object's shape was changed, returns true
    pub(crate) fn modify_shape_property_flags(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        flags_fn: impl FnOnce(u32) -> u32,
    ) -> Result<bool, i32> {
        let (idx, prs_flag) = self
            .shape()
            .unwrap()
            .prop_defs
            .find_hash(prop.id())
            .next()
            .map(|(i, p)| (i, p.flags()))
            .unwrap();

        let new_flags = flags_fn(prs_flag);

        if prs_flag != new_flags {
            self.modify_shape(ctx, |sh| {
                sh.prop_defs.get_mut(idx).unwrap().set_flags(new_flags);
                sh.full_rehash();
            })?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Updates flag of a shape property by index.
    /// if object's shape was changed, returns true
    pub(crate) fn modify_shape_property_index_flags(
        &mut self,
        ctx: &mut ScopedContext,
        index: usize,
        flags_fn: impl FnOnce(u32) -> u32,
    ) -> Result<bool, i32> {
        let flags = self
            .shape()
            .and_then(|s| s.prop_defs.get(index))
            .map(|p| p.flags())
            .unwrap();

        let new_flags = flags_fn(flags);

        if flags != new_flags {
            self.modify_shape(ctx, |sh| {
                sh.prop_defs.get_mut(index).unwrap().set_flags(new_flags);
                sh.full_rehash();
            })?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn compact_deleted_slots(&mut self, shape: &mut ShapeImpl) {
        debug_assert!(shape.use_count == 1);

        // Remove all entries from prop_defs whose corresponding slot is Deleted
        let mut removed_indices: SmallVec<[usize; 8]> = SmallVec::new();
        for (idx, _) in shape.prop_defs.iter() {
            if matches!(self.prop_slot(idx), JSProperty::Deleted) {
                removed_indices.push(idx);
            }
        }
        // Remove in reverse order to keep indices valid
        for idx in removed_indices.iter().rev() {
            shape.prop_defs.remove_index(*idx);
        }

        // compact prop_values
        self.prop_values
            .retain(|p| !matches!(p, JSProperty::Deleted));
        self.deleted_slot_count = 0;
    }
}
