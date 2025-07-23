use crate::{
    atom::{__JS_AtomToUInt32, JS_FreeAtom, JSAtom},
    context::JSContext,
    error::JS_ThrowReferenceErrorUninitialized,
    object::{
        JS_PROP_AUTOINIT, JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_NORMAL, JS_PROP_TMASK,
        JS_PROP_VARREF, JSObject, JSProperty, JSPropertyDescriptor,
    },
    value::JSValue,
};

impl JSObject {
    /// test if own property prop exists.
    pub fn has_own_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
        if self.has_shape_property(prop) {
            // shape property is own property
            return Ok(true);
        }

        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < self.array_length_unchecked() as u32 {
                        // array index is own property
                        return Ok(true);
                    }
                }
            } else if let Some(cb) = unsafe {
                ctx.rt().class_defs[self.class_id() as usize]
                    .exotic
                    .as_ref()
                    .and_then(|x| x.get_own_property)
            } {
                return cb(ctx, JSValue::Object(self.clone()), prop).map(|d| d.is_some());
            }
        }

        Ok(false)
    }

    pub fn has_own_property_str(&self, ctx: &JSContext, prop: &str) -> Result<bool, i32> {
        let atom = ctx.create_atom(prop)?;
        let b = self.has_own_property(ctx, atom);
        JS_FreeAtom(ctx, atom);
        b
    }

    /// get description of own property.
    /// Returns None if own property does not exist.
    pub fn get_own_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
            let mut desc = JSPropertyDescriptor {
                flags: prs_flags & JS_PROP_C_W_E,
                ..Default::default()
            };

            match prs_flags & JS_PROP_TMASK {
                JS_PROP_NORMAL => {
                    desc.value = self.0.prop_values.borrow()[idx].value(ctx, &JSValue::UNDEFINED);
                }
                JS_PROP_GETSET => {
                    desc.flags |= JS_PROP_GETSET;
                    if let JSProperty::GetSet { getter, setter } =
                        self.0.prop_values.borrow()[idx].clone()
                    {
                        desc.getter = getter.map_or(JSValue::UNDEFINED, JSValue::Object);
                        desc.setter = setter.map_or(JSValue::UNDEFINED, JSValue::Object);
                    } else {
                        unreachable!()
                    }
                }
                JS_PROP_VARREF => {
                    desc.value = self.0.prop_values.borrow()[idx].value(ctx, &JSValue::UNDEFINED);
                    if desc.value.is_uninitialized() {
                        JS_ThrowReferenceErrorUninitialized(ctx, prop);
                        return Err(-1);
                    }
                }
                JS_PROP_AUTOINIT => {
                    // Instantiate property
                    let mut pr = std::mem::take(&mut self.0.prop_values.borrow_mut()[idx]);
                    desc.value = pr.instantiate(self, prop, self)?;
                    debug_assert!(matches!(pr, JSProperty::Normal(_)));
                    debug_assert_eq!(
                        self.find_shape_property(prop).unwrap().1 & JS_PROP_TMASK,
                        JS_PROP_NORMAL
                    );
                    self.0.prop_values.borrow_mut()[idx] = pr;
                }
                _ => {}
            }

            return Ok(Some(desc));
        }

        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                // specific case for fast arrays
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < self.array_length_unchecked() as u32 {
                        return Ok(Some(JSPropertyDescriptor {
                            flags: JS_PROP_C_W_E,
                            value: self.get_property_value(ctx, &JSValue::from(idx)),
                            getter: JSValue::UNDEFINED,
                            setter: JSValue::UNDEFINED,
                        }));
                    }
                }
            } else {
                let em = ctx.rt().class_defs[self.class_id() as usize].exotic;
                if !em.is_null() {
                    if let Some(cb) = unsafe { (*em).get_own_property } {
                        return cb(ctx, JSValue::Object(self.clone()), prop);
                    }
                }
            }
        }

        Ok(None)
    }
}

impl JSValue {
    /// test if own property prop exists.
    pub fn has_own_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            p.has_own_property(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    pub fn has_own_property_str(&self, ctx: &JSContext, prop: &str) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            p.has_own_property_str(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// get description of own property.
    pub fn get_own_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        if let JSValue::Object(p) = self {
            p.get_own_property(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}
