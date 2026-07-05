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

        // compact props slot when deleted count ≥ threshold AND deleted count ≥ half of total slots.
        #[cfg(debug_assertions)]
        let should_compact = true;
        #[cfg(not(debug_assertions))]
        let should_compact = self.deleted_slot_count > 8
            && self.deleted_slot_count >= (self.prop_values.len() / 2) as u16;
        if should_compact {
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

        // Registry dedup: only meaningful when the source shape is shared
        // (use_count != 1). An exclusive shape cannot have a pre-existing
        // target because no other object shares the same source — the lookup
        // would always miss, so skip it.
        if use_count != 1
            && let Some(found) =
                ctx.rt()
                    .find_shape_with_extend_prop(self.shape().unwrap(), prop, prop_flags)
        {
            debug_assert_eq!(new_len, found.len());
            self.try_extend_prop_slot(1).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            debug_assert_eq!(self.prop_values.len(), new_len);
            self.set_shape(found);
            return Ok(new_len);
        }

        // Not found (or exclusive shape — lookup skipped above).
        // Extend slot first, then branch by exclusivity.
        self.try_extend_prop_slot(1).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        debug_assert_eq!(self.prop_values.len(), new_len);

        let mut use_shape = if use_count != 1 {
            // current shape is shared, dup to new without register.
            self.shape().unwrap().dup(ctx)?
        } else {
            // current shape is exclusive, unregister and use it.
            let sh = self.take_shape().unwrap();
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
    /// Returns the property's (post-compact) index so callers can update their
    /// cached idx — the index may change when compact_deleted_slots runs.
    /// When flags are unchanged the original index is still valid.
    pub(crate) fn modify_shape_property_flags(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        flags_fn: impl FnOnce(u32) -> u32,
    ) -> Result<usize, i32> {
        let (mut idx, prs_flag) = self
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
            // modify_shape may compact deleted slots, shifting indices.
            // Re-find to get the post-compact index.
            idx = self
                .shape()
                .unwrap()
                .prop_defs
                .find_hash(prop.id())
                .next()
                .map(|(i, _)| i)
                .unwrap();
        }

        Ok(idx)
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
        // Actually compact the hash list so deleted entries are removed and
        // the hash table is rebuilt — this ensures find_hash returns correct
        // indices after prop_values is compacted.
        shape.prop_defs.compact();

        // compact prop_values
        self.prop_values
            .retain(|p| !matches!(p, JSProperty::Deleted));
        self.deleted_slot_count = 0;
    }
}
