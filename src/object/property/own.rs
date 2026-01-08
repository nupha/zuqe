use crate::{
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::Atom,
    context::{ContextImpl, ContextPtr},
    error::JS_ThrowReferenceErrorUninitialized,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_LAZYINIT, JS_PROP_NO_EXOTIC, JS_PROP_NORMAL,
        JS_PROP_TMASK, JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty, JSPropertyDescriptor, Object,
        ObjectImpl, get_prop_flags,
    },
    utils::unlikely,
    value::JSValue,
};

impl ObjectImpl {
    /// Grows property values slot to accommodate more properties.
    /// Takes a new length and extends the array with undefined values if needed.
    /// Returns error if memory allocation fails.
    pub(crate) fn grow_prop_value_slots(&mut self, total_length: usize) -> Result<(), i32> {
        let len = self.prop_values.len();
        if total_length > len {
            let delta = total_length - len;
            self.prop_values.try_reserve_exact(delta).map_err(|_| -1)?;

            // init extended values
            for _ in 0..delta {
                self.prop_values.push(JSProperty::default());
            }
        }

        Ok(())
    }
}

impl Object {
    /// Add an own property, and grow object's prop value slots.
    /// This method adds the new prop is to shape, it assumes the new prop does not exist in shape yet.
    /// Returns Err if memory error.
    pub(crate) fn add_own_property(
        &mut self,
        ctx: &ContextImpl,
        prop: Atom,
        prop_flags: u32,
        value: JSProperty,
    ) -> Result<(), i32> {
        let len = self.modify_shape_add_prop(ctx, prop, prop_flags)?;
        debug_assert_eq!(self.prop_values.len(), len);

        self.prop_values[len - 1] = value;
        Ok(())
    }

    /// Find shape property by name atom.
    /// Returns shape property index (the slot index, not the sequence) and prop flags if the prop exists.
    #[inline]
    pub(crate) fn find_shape_property(&self, prop: Atom) -> Option<(usize, u32)> {
        self.shape().and_then(|sh| {
            sh.prop_defs
                .find_hash(prop.id())
                .map(|(i, prs)| (i, prs.flags()))
                .next()
        })
    }

    /// Check if given property name exists in shape.
    #[inline]
    pub(crate) fn has_shape_property(&self, prop: Atom) -> bool {
        self.shape()
            .is_some_and(|sh| sh.prop_defs.find_hash(prop.id()).count() != 0)
    }

    /// test if own property `prop` exists:
    /// 1. find in shape,
    /// 1. if not shape property, and if is exotic or fast array, test by exotic or array length.
    pub fn has_own_property(&self, ctx: &ContextImpl, prop: Atom) -> Result<bool, i32> {
        // is shape prop?
        if self.has_shape_property(prop) {
            return Ok(true);
        }

        // check exotic
        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                if let Some(idx) = prop.to_small_int()
                    && idx < self.fast_array_count()
                {
                    // array index is own property
                    return Ok(true);
                }
            } else if let Some(cb) = ctx.rt_ptr().class_registry[self.class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|x| x.get_own_property)
            {
                return cb(
                    unsafe { ContextPtr::from_ref_unchecked(ctx) },
                    (*self).into(),
                    prop,
                )
                .map(|d| d.is_some());
            }
        }

        Ok(false)
    }

    #[inline]
    pub fn has_own_property_str(&self, ctx: &ContextImpl, prop: &str) -> Result<bool, i32> {
        let atom = Atom::new(ctx.rt_ptr(), prop)?;
        self.has_own_property(ctx, atom)
    }

    /// get own property descriptor.
    /// Returns None if own property does not exist.
    pub fn get_own_property(
        &self,
        ctx: &ContextImpl,
        prop: Atom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        while let Some((idx, prs_flags)) = self.find_shape_property(prop) {
            let mut desc = JSPropertyDescriptor {
                flags: prs_flags & JS_PROP_C_W_E,
                ..Default::default()
            };

            match prs_flags & JS_PROP_TMASK {
                JS_PROP_NORMAL => {
                    desc.value = self.prop_values[idx].value(ctx, JSValue::UNDEFINED)?;
                }
                JS_PROP_GETSET => {
                    desc.flags |= JS_PROP_GETSET;
                    if let JSProperty::GetSet { getter, setter } = &self.prop_values[idx] {
                        desc.getter = getter.map_or(JSValue::UNDEFINED, JSValue::Object);
                        desc.setter = setter.map_or(JSValue::UNDEFINED, JSValue::Object);
                    } else {
                        unreachable!()
                    }
                }
                JS_PROP_VARREF => {
                    desc.value = self.prop_values[idx].value(ctx, JSValue::UNDEFINED)?;
                    if unlikely(desc.value.is_uninitialized()) {
                        JS_ThrowReferenceErrorUninitialized(ctx, prop);
                        return Err(-1);
                    }
                }
                JS_PROP_LAZYINIT => {
                    // Instantiate lazy object
                    desc.value = unsafe {
                        let mut o = *self;
                        let pr = o.prop_values.get_unchecked_mut(idx);

                        let o = *self;
                        pr.instantiate(o, idx, o)?
                    };
                    continue;
                }
                _ => {
                    unreachable!();
                }
            }

            return Ok(Some(desc));
        }

        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                // specific case for fast arrays
                if let Some(idx) = prop.to_small_int()
                    && idx < self.fast_array_count()
                {
                    return Ok(Some(JSPropertyDescriptor {
                        flags: JS_PROP_C_W_E,
                        value: self.get_property_value(ctx, JSValue::from(idx))?,
                        getter: JSValue::UNDEFINED,
                        setter: JSValue::UNDEFINED,
                    }));
                }
            } else if let Some(f) = ctx.rt_ptr().class_registry[self.class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|x| x.get_own_property)
            {
                return f(
                    unsafe { ContextPtr::from_ref_unchecked(ctx) },
                    JSValue::Object(*self),
                    prop,
                );
            }
        }

        Ok(None)
    }

    /// Similar to define_property(), usually used to initialize property for new instance.
    ///
    /// - If property already exists, update its value;
    /// - If property is array index and self is fast array,
    ///   update indexed value. Will destruct fast array to sparse array when needed.
    /// - otherwise, call `Self::add_own_property()` to add a shape property.
    pub(super) fn create_property(
        &mut self,
        ctx: &ContextImpl,
        prop: Atom,
        val: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let update_array_length_fn =
            |mut arr: Object, idx: u32, cur_len: i64| -> Option<Result<bool, i32>> {
                debug_assert!(idx < u32::MAX, "array length<=u32::MAX, so idx<u32::MAX");

                if idx + 1 > cur_len as u32 {
                    // first prop must be `length`
                    let prs_flags = arr
                        .shape()
                        .and_then(|sh| sh.prop_defs.first())
                        .map(|p| p.flags())
                        .unwrap(); // <-- prop length must exist.

                    if unlikely(prs_flags & JS_PROP_WRITABLE == 0) {
                        return if ctx.can_throw(flags) {
                            ctx.throw_type_error_read_only(Atom::CONST_length);
                            Some(Err(-1))
                        } else {
                            None
                        };
                    } else {
                        arr.prop_values[0] = JSProperty::Normal(JSValue::from(idx + 1));
                    }
                }

                None
            };

        let obj_flags = self.flags();

        // add a new property or modify an existing exotic one
        if obj_flags.is_exotic() {
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    // get current `length`
                    let Ok(arr_len) = self.prop_values[0]
                        .value(ctx, JSValue::UNDEFINED)
                        .and_then(|v| v.to_length(ctx))
                    else {
                        return if ctx.can_throw(flags) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    };

                    if obj_flags.is_fast_array() {
                        if let Some(idx) = prop.to_small_int() {
                            let list_count = self.fast_array_count();
                            if idx == list_count {
                                if unlikely(!self.flags().extensible()) {
                                    return if ctx.throw_type_error_or_false(
                                        flags as _,
                                        "object is not extensible",
                                    ) {
                                        Err(-1)
                                    } else {
                                        Ok(false)
                                    };
                                }

                                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                    if let Err(e) = self.destruct_fast_array(ctx) {
                                        return if ctx.can_throw(flags) {
                                            Err(e)
                                        } else {
                                            Ok(false)
                                        };
                                    }
                                    if let Some(ret) = update_array_length_fn(*self, idx, arr_len) {
                                        return ret;
                                    }
                                }

                                let prop_flags = get_prop_flags(flags, 0);
                                if prop_flags != JS_PROP_C_W_E {
                                    match self.destruct_fast_array(ctx) {
                                        Ok(_) => {
                                            if let Some(ret) =
                                                update_array_length_fn(*self, idx, arr_len)
                                            {
                                                return ret;
                                            }
                                        }
                                        Err(e) => {
                                            return if ctx.can_throw(flags) {
                                                Err(e)
                                            } else {
                                                Ok(false)
                                            };
                                        }
                                    }
                                } else {
                                    // fast array append element.
                                    return self.add_fast_array_element(ctx, val, flags);
                                }
                            } else {
                                // leaves a hole between `arr_idx` and `idx`,
                                // will degrade to sparse array.
                                if let Err(e) = self.destruct_fast_array(ctx) {
                                    return if ctx.can_throw(flags) {
                                        Err(e)
                                    } else {
                                        Ok(false)
                                    };
                                }
                                if let Some(ret) = update_array_length_fn(*self, idx, arr_len) {
                                    return ret;
                                }
                            }
                        } else if let Some(idx) = prop.to_array_index() {
                            // convert fast array to sparse array
                            if self.destruct_fast_array(ctx).is_err() {
                                return if ctx.can_throw(flags) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            } else if let Some(ret) = update_array_length_fn(*self, idx, arr_len) {
                                return ret;
                            }
                        }
                    } else if let Some(idx) = prop.to_array_index()
                        && let Some(ret) = update_array_length_fn(*self, idx, arr_len)
                    {
                        return ret;
                    }
                } // Array, Arguments

                JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                    if prop.is_numeric_index(ctx)? {
                        return if ctx.can_throw(flags) {
                            ctx.throw_type_error("cannot create numeric index in typed array");
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }
                }

                cid => {
                    if flags & JS_PROP_NO_EXOTIC == 0
                        && let Some(pm) = ctx.rt_ptr().class_registry[cid as usize].exotic.as_ref()
                    {
                        if let Some(f) = pm.define_own_property {
                            return f(
                                unsafe { ContextPtr::from_ref_unchecked(ctx) },
                                JSValue::Object(*self),
                                prop,
                                val,
                                getter,
                                setter,
                                flags as _,
                            );
                        }

                        if unlikely(!self.is_extensible(ctx)?) {
                            return if ctx.can_throw(flags) {
                                ctx.throw_type_error("object is not extensible");
                                Err(-1)
                            } else {
                                Ok(false)
                            };
                        }
                    }
                }
            }
        }

        // Generic: if prop is not an exotic property, nor an array element:
        // add shape property and put the value in value slot.

        if unlikely(!obj_flags.extensible()) {
            if ctx.can_throw(flags) {
                ctx.throw_type_error("object is not extensible");
                Err(-1)
            } else {
                Ok(false)
            }
        } else {
            let prop_flags = if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                (flags & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
            } else {
                flags & JS_PROP_C_W_E
            };

            self.add_own_property(ctx, prop, prop_flags, {
                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                    // getset property
                    JSProperty::GetSet {
                        getter: if flags & JS_PROP_HAS_GET != 0 && getter.is_function() {
                            getter.as_object().copied()
                        } else {
                            None
                        },
                        setter: if flags & JS_PROP_HAS_SET != 0 && setter.is_function() {
                            setter.as_object().copied()
                        } else {
                            None
                        },
                    }
                } else {
                    // normal property
                    JSProperty::Normal(if flags & JS_PROP_HAS_VALUE != 0 {
                        val
                    } else {
                        JSValue::UNDEFINED
                    })
                }
            })?;

            Ok(true)
        }
    }
}

impl JSValue {
    /// test if own property prop exists.
    pub fn has_own_property(&self, ctx: &ContextImpl, prop: Atom) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            p.has_own_property(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    pub fn has_own_property_str(&self, ctx: &ContextImpl, prop: &str) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            p.has_own_property_str(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// get description of own property.
    pub fn get_own_property(
        &mut self,
        ctx: &ContextImpl,
        prop: Atom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        if let JSValue::Object(p) = self {
            p.get_own_property(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}
