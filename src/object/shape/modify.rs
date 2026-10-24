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
    /// The closure receives a private, unregistered copy of the shape.
    ///
    /// A registered shape is the registry's canonical entry for its content, so
    /// it is never modified in place (`use_count == 1` does not mean private:
    /// the registry's own reference is not counted). Consuming it in place
    /// would make the transition unreusable and force every later object to
    /// allocate a fresh shape (VM-P10).
    /// Returns true if deleted slots were compacted (property indices changed),
    /// false otherwise.
    pub(crate) fn modify_shape(
        &mut self,
        ctx: &mut ScopedContext,
        update_fn: impl FnOnce(&mut ShapeImpl),
    ) -> Result<bool, i32> {
        let mut use_shape = self.shape().unwrap().dup(ctx)?;

        update_fn(&mut use_shape);

        // compact props slot when deleted count ≥ threshold AND deleted count ≥ half of total slots.
        #[cfg(debug_assertions)]
        let should_compact = true;
        #[cfg(not(debug_assertions))]
        let should_compact = self.deleted_slot_count > 8
            && self.deleted_slot_count >= (self.prop_values.len() / 2) as u16;

        let compacted = if should_compact {
            self.compact_deleted_slots(&mut use_shape)
        } else {
            false
        };

        // Both `update_fn` and compaction change hash inputs (proto, flags,
        // prop list); refresh the hash so it matches the contents and the
        // registry bucket the shape is inserted into.
        use_shape.full_rehash();
        ctx.rt_mut().register_shape(&use_shape)?;
        self.set_shape(use_shape);

        Ok(compacted)
    }

    /// Append one extra property to shape.
    /// Single-prop special case of [`Self::modify_shape_add_props`].
    #[inline(always)]
    pub(crate) fn modify_shape_add_prop(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        prop_flags: u32,
    ) -> Result<usize, i32> {
        self.modify_shape_add_props(ctx, &[(prop, prop_flags)])
    }

    /// Append `extras` to shape as **one** transition: a single registry lookup
    /// and, on a miss, a single duplicate + register — no intermediate shapes.
    ///
    /// `extras` must be non-empty, unique, and none may already exist in the
    /// shape (callers verify that when resolving own-property semantics).
    pub(crate) fn modify_shape_add_props(
        &mut self,
        ctx: &mut ScopedContext,
        extras: &[(Atom, u32)],
    ) -> Result<usize, i32> {
        debug_assert!(!extras.is_empty());

        let old_len = self.shape().unwrap().len();
        debug_assert_eq!(self.prop_values.len(), old_len);
        let new_len = old_len + extras.len();

        #[cfg(debug_assertions)]
        {
            let sh = self.shape().unwrap();
            for (i, (prop, _)) in extras.iter().enumerate() {
                debug_assert!(
                    sh.prop_defs.find_hash(prop.id()).next().is_none(),
                    "prop already in shape"
                );
                debug_assert!(
                    extras[..i].iter().all(|(p, _)| p != prop),
                    "duplicate prop in batch"
                );
            }
        }

        // Registry dedup: try the transition even when this object is the only
        // user. `use_count == 1` does not make the source shape private — the
        // registry reference is not counted — and a registered source must stay
        // intact so later objects can reuse this transition.
        if let Some(found) = ctx
            .rt()
            .shape_registry
            .find_extend(self.shape().unwrap(), extras)
        {
            debug_assert_eq!(new_len, found.len());
            self.try_extend_prop_slot(extras.len())
                .map_err(|_| ctx.throw_out_of_memory())?;
            debug_assert_eq!(self.prop_values.len(), new_len);
            self.set_shape(found);
            return Ok(new_len);
        }

        // Not found: extend a private copy. The source shape is left untouched
        // (registered, or a transient unregistered one) — see `modify_shape`.
        let mut use_shape = self.shape().unwrap().dup(ctx)?;

        self.try_extend_prop_slot(extras.len())
            .map_err(|_| ctx.throw_out_of_memory())?;
        debug_assert_eq!(self.prop_values.len(), new_len);

        debug_assert!(!ctx.rt().is_shape_registered(&use_shape));

        // shape can be modified now.
        use_shape.add_shape_properties(ctx, extras);
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
            let compacted = self.modify_shape(ctx, |sh| {
                sh.prop_defs.get_mut(idx).unwrap().set_flags(new_flags);
            })?;

            if compacted {
                // compaction shifted indices; re-find the post-compact index.
                idx = self
                    .shape()
                    .unwrap()
                    .prop_defs
                    .find_hash(prop.id())
                    .next()
                    .map(|(i, _)| i)
                    .unwrap();
            }
        }

        Ok(idx)
    }

    /// Updates flag of a shape property by index.
    /// Returns the property's (post-compact) index: if modifying the shape
    /// compacted deleted slots (shifting indices), the index is re-found and
    /// the updated one is returned; otherwise the passed-in index is returned.
    #[must_use = "returns the (possibly compact-shifted) property index the caller must use to access the property slot"]
    pub(crate) fn modify_shape_property_index_flags(
        &mut self,
        ctx: &mut ScopedContext,
        index: usize,
        flags_fn: impl FnOnce(u32) -> u32,
    ) -> Result<usize, i32> {
        let (flags, prop_id) = self
            .shape()
            .and_then(|s| s.prop_defs.get(index))
            .map(|p| (p.flags(), p.atom.id()))
            .unwrap();

        let new_flags = flags_fn(flags);

        if flags != new_flags {
            let compacted = self.modify_shape(ctx, |sh| {
                sh.prop_defs.get_mut(index).unwrap().set_flags(new_flags);
            })?;

            if compacted {
                // compaction shifted indices; re-find the post-compact index.
                let index = self
                    .shape()
                    .unwrap()
                    .prop_defs
                    .find_hash(prop_id)
                    .next()
                    .map(|(i, _)| i)
                    .unwrap();
                return Ok(index);
            }
        }

        Ok(index)
    }

    /// Returns true if any deleted slot was actually removed (indices shifted).
    fn compact_deleted_slots(&mut self, shape: &mut ShapeImpl) -> bool {
        debug_assert!(shape.use_count == 1);

        // Collect entries from prop_defs whose corresponding slot is Deleted
        let mut removed_indices: SmallVec<[usize; 8]> = SmallVec::new();
        for (idx, _) in shape.prop_defs.iter() {
            if matches!(self.prop_slot(idx), JSProperty::Deleted) {
                removed_indices.push(idx);
            }
        }

        if removed_indices.is_empty() {
            return false;
        }

        // Compaction reorders prop slots, so any cached `(shape, slot)` pair
        // (the bytecode `GET_FIELD_IC`) must be invalidated. The shape always
        // reaches here as a fresh duplicate (`modify_shape`), i.e. with a new
        // pointer and a new uid, so those caches re-resolve by name — mirroring
        // QuickJS, which allocates a brand-new `JSShape` on every modification.

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

        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{JSValue, Runtime, ScopedContext};

    /// VM-P10: instances of the same prototype must share one registered shape
    /// per transition. Before the fix, adding the first property consumed the
    /// class default (registered) shape in place, so every instance allocated a
    /// `ShapeImpl` of its own.
    #[test]
    fn instances_share_shape_after_prop_add() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        ctx.with_root_scope(|mut cx| {
            let proto = cx.create_base_object()?;

            // Keep every instance alive so no shape can be reclaimed between
            // iterations: the shared shape must be observable directly.
            let mut objs = Vec::new();
            for i in 0..64 {
                let mut obj = cx.create_base_object_with_proto(Some(proto))?;
                obj.set_property_str(&mut cx, "a", JSValue::from(i as i64))?;
                objs.push(obj);
            }

            let first = objs[0].shape().unwrap().gc_ref();
            for (i, obj) in objs.iter().enumerate() {
                assert_eq!(
                    obj.shape().unwrap().gc_ref(),
                    first,
                    "instance {i} allocated its own shape"
                );
            }
            Ok(())
        })
    }

    /// `set_prototype` goes through `modify_shape`: the canonical empty shape
    /// must be duplicated (and rehashed for the new proto) instead of being
    /// consumed in place, so the next object with that proto reuses it.
    #[test]
    fn prop_of_shapes_reused_after_set_prototype() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        ctx.with_root_scope(|mut cx| {
            let proto1 = cx.create_base_object()?;
            let proto2 = cx.create_base_object()?;

            let mut moved = cx.create_base_object_with_proto(Some(proto1))?;
            moved.set_prototype(&mut cx, JSValue::from(proto2))?;

            let reused = cx.create_base_object_with_proto(Some(proto2))?;
            assert_eq!(
                moved.shape().unwrap().gc_ref(),
                reused.shape().unwrap().gc_ref(),
                "proto2's empty shape was not registered under its new hash"
            );
            Ok(())
        })
    }

    /// Multi-prop chains must dedup too: each appending transition is looked up
    /// by hash and verified prop-by-prop, so verify the whole chain lands on one
    /// canonical shape.
    #[test]
    fn instances_share_shape_after_multi_prop_chain() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        ctx.with_root_scope(|mut cx| {
            let proto = cx.create_base_object()?;
            let mut objs = Vec::new();
            for i in 0..32 {
                let mut obj = cx.create_base_object_with_proto(Some(proto))?;
                for (k, name) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
                    obj.set_property_str(&mut cx, name, JSValue::from((i * 8 + k) as i64))?;
                }
                objs.push(obj);
            }

            let first = objs[0].shape().unwrap().gc_ref();
            for (i, obj) in objs.iter().enumerate() {
                assert_eq!(
                    obj.shape().unwrap().gc_ref(),
                    first,
                    "instance {i} diverged after a multi-prop chain"
                );
            }
            Ok(())
        })
    }

    /// Batch add: the whole prop list is one transition — one shape allocated on
    /// the first object, zero on every later one, and no intermediate shapes.
    #[test]
    fn batch_prop_add_creates_one_shape() -> Result<(), i32> {
        use crate::{atom::Atom, function::prop_str_to_atom, object::JS_PROP_C_W_E};

        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        ctx.with_root_scope(|mut cx| {
            let proto = cx.create_base_object()?;

            let mut atoms: Vec<Atom> = Vec::new();
            for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
                atoms.push(prop_str_to_atom(&mut cx, name)?);
            }

            let mut shapes = Vec::new();
            for round in 0..4i64 {
                let mut obj = cx.create_base_object_with_proto(Some(proto))?;
                let before = cx.rt().shape_registry.count();

                let props: Vec<(Atom, u32, JSValue)> = atoms
                    .iter()
                    .enumerate()
                    .map(|(i, atom)| (*atom, JS_PROP_C_W_E, JSValue::from(round * 10 + i as i64)))
                    .collect();
                obj.add_own_properties(&mut cx, &props)?;

                let created = cx.rt().shape_registry.count() - before;
                assert_eq!(
                    created,
                    if round == 0 { 1 } else { 0 },
                    "round {round}: batch add allocated {created} shapes"
                );

                for (i, atom) in atoms.iter().enumerate() {
                    let got = obj.get_property(&mut cx, *atom)?;
                    assert_eq!(
                        got.as_int(),
                        Some((round * 10 + i as i64) as i32),
                        "round {round}: prop {i} value"
                    );
                    // Same flags `define_property_value(.., JS_PROP_C_W_E)` stores.
                    let prs = obj.shape().unwrap().prop_defs.get(i).unwrap();
                    assert_eq!(prs.flags(), JS_PROP_C_W_E);
                }
                shapes.push(obj.shape().unwrap().gc_ref());
            }

            // Every batch lands on the same canonical shape.
            for (i, sh) in shapes.iter().enumerate().skip(1) {
                assert_eq!(*sh, shapes[0], "batch round {i} has its own shape");
            }
            Ok(())
        })
    }
}
