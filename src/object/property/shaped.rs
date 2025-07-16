use crate::{
    atom::{JS_FreeAtom, JSAtom},
    context::JSContext,
    object::{JSObject, JSObjectInner, JSProperty, JSShape},
    value::JSValue,
};

impl JSObjectInner {
    /// Grows the property values array to accommodate more properties.
    /// Takes a new length and extends the array with undefined values if needed.
    /// Returns error if memory allocation fails.
    pub(crate) fn grow_prop_value_slots(&self, total_length: usize) -> Result<(), i32> {
        let mut slots = self.prop_values.borrow_mut();

        let len = slots.len();
        if total_length > len {
            let delta = total_length - len;
            slots.try_reserve_exact(delta).map_err(|_| -1)?;

            // init extended values
            for _ in 0..delta {
                slots.push(JSProperty::default());
            }
        }

        Ok(())
    }

    /// Add an own property to shape and enlarge prop value slots.
    /// This function implies that the new prop should be add to shape (owned property),
    /// and it does not exist yet.
    /// Returns Err if memory error.
    /// `prop` is not consumed.
    fn do_add_shape_property<R>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prop_flags: u32,
        then: Option<impl FnOnce(&mut JSProperty) -> R>,
    ) -> Result<Option<R>, i32> {
        let mut shape = self.shape.borrow_mut().take().unwrap();
        let mut shape0: Option<JSShape> = None;

        if shape.is_hashed() {
            // try to find an existing shape
            match ctx.rt().find_shape_with_prop(&shape, prop, prop_flags) {
                Some(found) => {
                    let new_len = found.len();
                    let len = shape.len();
                    debug_assert!(new_len > len);

                    if self.grow_prop_value_slots(new_len).is_err() {
                        ctx.throw_out_of_memory();
                        self.shape.borrow_mut().replace(shape); // restore old shape
                        return Err(-1);
                    }

                    // use found shape
                    self.shape.borrow_mut().replace(found);

                    return if let Some(f) = then {
                        Ok(Some(f(self
                            .prop_values
                            .borrow_mut()
                            .get_mut(new_len - 1)
                            .unwrap())))
                    } else {
                        Ok(None)
                    };
                }
                None => {
                    if shape.strong_count() != 1 {
                        // current shape is shared, clone and use the new
                        let new_shape = shape.clone_to_new(ctx)?;
                        shape0.replace(shape);
                        shape = new_shape;
                    }
                }
            };
        }

        // Note: shape must be exclusive now, so that is can be modified.
        // add shape property, enlarge object prop value slot.
        let len = if let Ok(n) = shape.add_shape_property(ctx, prop, prop_flags, Some(self)) {
            n
        } else {
            // fail to modify shape: restore old shape for object
            if let Some(sh0) = shape0 {
                self.shape.borrow_mut().replace(sh0);
            } else {
                // shape was unregistered
                ctx.rt_mut().register_shape(&shape).unwrap();
                self.shape.borrow_mut().replace(shape);
            }
            return Err(-1);
        };

        // let object use modified shape
        self.shape.borrow_mut().replace(shape.clone());

        if let Some(f) = then {
            Ok(Some(f(&mut self.prop_values.borrow_mut()[len - 1])))
        } else {
            Ok(None)
        }
    }

    /// Add an own property to shape and enlarge prop value slots.
    /// This function implies that the new prop should be add to shape (owned property),
    /// and it does not exist yet.
    /// Returns Err if memory error.
    /// `prop` is not consumed.
    #[inline(always)]
    pub fn add_shape_property_then<R>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prop_flags: u32,
        then: impl FnOnce(&mut JSProperty) -> R,
    ) -> Result<R, i32> {
        self.do_add_shape_property(ctx, prop, prop_flags, Some(then))
            .map(|r| r.unwrap())
    }
}

impl JSObject {
    /// Check if shape property `atom` exists.
    /// `prop` is not consumed.
    #[inline]
    pub fn has_shape_property(&self, prop: JSAtom) -> bool {
        self.shape()
            .is_some_and(|sh| sh.0.borrow().prop_defs.get(&prop.0).is_some())
    }

    pub fn has_shape_property_str(&self, ctx: &JSContext, prop: &str) -> Result<bool, i32> {
        let atom = ctx.create_atom(prop)?;
        let b = self.has_shape_property(atom);
        JS_FreeAtom(ctx, atom);
        Ok(b)
    }

    /// Find shape property by prop atom, a.k.a find_own_property().
    /// Returns shape property index and flags if the prop exists.
    /// `prop` is not consumed.
    #[inline]
    pub fn find_shape_property(&self, prop: JSAtom) -> Option<(usize, u32)> {
        self.shape().and_then(|sh| {
            sh.0.borrow()
                .prop_defs
                .get_full(&prop.0)
                .map(|(i, _, pr)| (i, pr.flags()))
        })
    }

    /// Add a own property to shape and enlarge prop value slots.
    /// This function implies that the new prop should be add to shape (owned property),
    /// and it does not exist yet.
    /// Returns Err if memory error.
    /// `prop` is not consumed.
    #[inline(always)]
    pub fn add_shape_property_then<R>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        prop_flags: u32,
        then: impl FnOnce(&mut JSProperty) -> R,
    ) -> Result<R, i32> {
        self.0.add_shape_property_then(ctx, prop, prop_flags, then)
    }
}

impl JSValue {
    pub fn has_shape_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            Ok(p.has_shape_property(prop))
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    pub fn has_shape_property_str(&self, ctx: &JSContext, prop: &str) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            p.has_shape_property_str(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}
