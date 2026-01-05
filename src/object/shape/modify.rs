use crate::{
    atom::Atom,
    context::ContextImpl,
    object::{ObjectImpl, ShapeImpl},
};

impl ObjectImpl {
    /// modify object's shape by closure.
    /// an exclusive, unregistered shape is passed to closure as argument.
    pub(crate) fn modify_shape<R>(
        &mut self,
        ctx: &ContextImpl,
        f: impl FnOnce(&mut ShapeImpl) -> R,
    ) -> Result<R, i32> {
        let use_count = self.shape().unwrap().use_count();
        let mut use_shape = if use_count != 1 {
            // shape is shared: dup to new
            self.shape().unwrap().dup(ctx)?
        } else {
            // shape is exclusivly used, use it
            let sh = self.take_shape().unwrap();
            ctx.rt_ptr().shape_registry.unregister(&sh);
            sh
        };

        let r = f(&mut use_shape);

        ctx.rt_ptr().shape_registry.register(&use_shape)?;
        self.set_shape(use_shape);

        Ok(r)
    }

    /// Append an extra property to shape
    pub(crate) fn modify_shape_add_prop(
        &mut self,
        ctx: &ContextImpl,
        prop: Atom,
        prop_flags: u32,
    ) -> Result<usize, i32> {
        let (old_len, use_count) = {
            let sh = self.shape().unwrap();
            (sh.len(), sh.use_count())
        };
        debug_assert_eq!(self.prop_values.len(), old_len);

        let new_len = old_len + 1;

        // grow prop values slot
        self.grow_prop_value_slots(new_len).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        debug_assert_eq!(self.prop_values.len(), new_len);

        // find shape registry
        if let Some(found) = ctx.rt_ptr().shape_registry.find_shape_with_extend_prop(
            self.shape().unwrap(),
            prop,
            prop_flags,
        ) {
            // use existing shape
            debug_assert_eq!(new_len, found.len());
            self.set_shape(found);
            Ok(new_len)
        } else {
            let mut target = if use_count != 1 {
                // current shape is shared, dup to new without register.
                let mut sh2 = self.shape().unwrap().dup(ctx)?;
                sh2.prop_defs.try_reserve_exact(1).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                sh2
            } else {
                // current shape is exclusive, use it.
                let mut sh = self.take_shape().unwrap();
                if sh.prop_defs.try_reserve_exact(1).is_err() {
                    self.set_shape(sh); // restore back
                    ctx.throw_out_of_memory();
                    return Err(-1);
                }

                ctx.rt_ptr().shape_registry.unregister(&sh);
                sh
            };

            // shape can be modified now.
            target.add_shape_properties(&[(prop, prop_flags)]);
            // register the shape
            ctx.rt_ptr().shape_registry.register(&target).unwrap();
            // object uses the shape
            self.set_shape(target);

            Ok(new_len)
        }
    }

    /// Updates flag of a shape property.
    /// if object's shape was changed, returns true
    pub(crate) fn modify_shape_property_flags(
        &mut self,
        ctx: &ContextImpl,
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
                sh.prop_defs
                    .get_index_mut(idx)
                    .unwrap()
                    .set_flags(new_flags);

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
        ctx: &ContextImpl,
        index: usize,
        flags_fn: impl FnOnce(u32) -> u32,
    ) -> Result<bool, i32> {
        let cur_flags = self
            .shape()
            .unwrap()
            .prop_defs
            .get(index)
            .map(|p| p.flags())
            .unwrap();
        let new_flags = flags_fn(cur_flags);

        if cur_flags != new_flags {
            self.modify_shape(ctx, |sh| {
                sh.prop_defs
                    .get_index_mut(index)
                    .unwrap()
                    .set_flags(new_flags);

                sh.full_rehash();
            })?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

// impl ObjectImpl {
//     /// modify object's shape.
//     /// An exclusive, unregistered shape is passed to closure as argument
//     #[inline(always)]
//     pub(crate) fn modify_shape<R>(
//         &self,
//         ctx: &ContextImpl,
//         f: impl FnOnce(&mut ShapeImpl) -> R,
//     ) -> Result<R, i32> {
//         self.modify_shape(ctx, f)
//     }

//     /// Updates flag of shape property.
//     /// if object's shape was changed, returns true
//     #[inline(always)]
//     pub(crate) fn modify_shape_property_flags(
//         &self,
//         ctx: &ContextImpl,
//         prop: &Atom,
//         flags_fn: impl FnOnce(u32) -> u32,
//     ) -> Result<bool, i32> {
//         self.0.modify_shape_property_flags(ctx, prop, flags_fn)
//     }

//     /// Updates flag of shape property, by index.
//     /// if object's shape was changed, returns true
//     #[inline(always)]
//     pub(crate) fn modify_shape_property_index_flags(
//         &self,
//         ctx: &ContextImpl,
//         index: usize,
//         flags_fn: impl FnOnce(u32) -> u32,
//     ) -> Result<bool, i32> {
//         self.0
//             .modify_shape_property_index_flags(ctx, index, flags_fn)
//     }
// }
