use crate::{
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{Atom, JS_AtomIsNumericIndex, consts::JS_ATOM_length},
    branch_hints::unlikely,
    context::JSContext,
    error::JS_ThrowReferenceErrorUninitialized,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_LAZYINIT, JS_PROP_NO_EXOTIC, JS_PROP_NORMAL,
        JS_PROP_TMASK, JS_PROP_VARREF, JS_PROP_WRITABLE, JSObject, JSProperty,
        JSPropertyDescriptor, JSShape, ObjectInner, get_prop_flags,
    },
    value::JSValue,
};

impl ObjectInner {
    /// Grows property values slot to accommodate more properties.
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

    /// Add an own property, and grow object's prop value slots.
    /// This method adds the new prop is to shape, it assumes the new prop does not exist in shape yet.
    /// Returns Err if memory error.
    /// `prop` is not consumed.
    pub(crate) fn add_own_property(
        &self,
        ctx: &JSContext,
        prop: Atom,
        prop_flags: u32,
        prop_value: JSProperty,
    ) -> Result<(), i32> {
        let mut shape = self.shape.borrow_mut().take().unwrap();
        let mut shape0: Option<JSShape> = None;

        if shape.is_hashed() {
            // try to find an existing shape
            match ctx
                .rt()
                .find_shape_with_extend_prop(&shape, &prop, prop_flags)
            {
                Some(found) => {
                    let new_len = found.len();
                    // debug_assert!(new_len > shape.len());
                    debug_assert_eq!(new_len, shape.len() + 1);

                    let r = self.grow_prop_value_slots(new_len);
                    if unlikely(r.is_err()) {
                        ctx.throw_out_of_memory();
                        self.shape.borrow_mut().replace(shape); // restore original shape
                        return Err(-1);
                    } else {
                        // use existing shape
                        self.shape.borrow_mut().replace(found);
                        // set value
                        self.prop_values.borrow_mut()[new_len - 1] = prop_value;
                        return Ok(());
                    }
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
        // prop content
        self.prop_values.borrow_mut()[len - 1] = prop_value;

        Ok(())
    }
}

impl JSObject {
    /// Check if shape property `atom` exists.
    /// `prop` is not consumed.
    pub(crate) fn has_shape_property(&self, prop: &Atom) -> bool {
        self.shape()
            .is_some_and(|sh| sh.0.borrow().prop_defs.get(&prop.id()).is_some())
    }

    /// Find shape property by prop atom.
    /// Returns shape property index and prop flags if the prop exists.
    ///
    /// `prop` is not consumed.
    #[inline]
    pub(crate) fn find_shape_property(&self, prop: &Atom) -> Option<(usize, u32)> {
        self.shape().and_then(|sh| {
            sh.0.borrow()
                .prop_defs
                .get_full(&prop.id())
                .map(|(i, _, pr)| (i, pr.flags()))
        })
    }

    /// test if own property `prop` exists:
    /// 1. find in shape,
    /// 1. if not shape property, and if is exotic or fast array, test by exotic or array length.
    pub fn has_own_property(&self, ctx: &JSContext, prop: &Atom) -> Result<bool, i32> {
        // is shape prop?
        if self.has_shape_property(prop) {
            return Ok(true);
        }

        // check exotic
        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                if let Some(idx) = prop.to_small_int() {
                    if idx < self.fast_array_count() as u32 {
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
        let atom = Atom::new(ctx.rt_mut(), prop)?;
        self.has_own_property(ctx, &atom)
    }

    /// get own property descriptor.
    /// Returns None if own property does not exist.
    pub fn get_own_property(
        &self,
        ctx: &JSContext,
        prop: &Atom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        loop {
            if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
                let mut desc = JSPropertyDescriptor {
                    flags: prs_flags & JS_PROP_C_W_E,
                    ..Default::default()
                };

                match prs_flags & JS_PROP_TMASK {
                    JS_PROP_NORMAL => {
                        desc.value =
                            self.0.prop_values.borrow()[idx].value(ctx, &JSValue::UNDEFINED)?;
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
                        desc.value =
                            self.0.prop_values.borrow()[idx].value(ctx, &JSValue::UNDEFINED)?;
                        if unlikely(desc.value.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized(ctx, prop.as_ref());
                            return Err(-1);
                        }
                    }
                    JS_PROP_LAZYINIT => {
                        // Instantiate lazy object
                        desc.value = unsafe {
                            self.0
                                .prop_values
                                .borrow_mut()
                                .get_unchecked_mut(idx)
                                .instantiate(self, idx, self)?
                        };
                        continue;
                    }
                    _ => {
                        unreachable!();
                    }
                }

                return Ok(Some(desc));
            } else {
                break;
            }
        }

        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                // specific case for fast arrays
                if let Some(idx) = prop.to_small_int()
                    && idx < self.fast_array_count() as u32
                {
                    return Ok(Some(JSPropertyDescriptor {
                        flags: JS_PROP_C_W_E,
                        value: self.get_property_value(ctx, &JSValue::from(idx))?,
                        getter: JSValue::UNDEFINED,
                        setter: JSValue::UNDEFINED,
                    }));
                }
            } else if let Some(f) = unsafe {
                ctx.rt().class_defs[self.class_id() as usize]
                    .exotic
                    .as_ref()
                    .and_then(|xm| xm.get_own_property)
            } {
                return f(ctx, JSValue::Object(self.clone()), prop);
            }
        }

        Ok(None)
    }

    /// Add a own property to shape and enlarge prop value slots.
    /// This function implies that the new prop should be add to shape (owned property),
    /// and it does not exist yet.
    /// Returns Err if memory error.
    #[inline(always)]
    pub fn add_own_property(
        &self,
        ctx: &JSContext,
        prop: Atom,
        prop_flags: u32,
        prop_value: JSProperty,
    ) -> Result<(), i32> {
        self.0.add_own_property(ctx, prop, prop_flags, prop_value)
    }

    /// Similar to define_property(), usually used to initialize property for new instance.
    ///
    /// - If property already exists, update its value;
    /// - If property is array index and self is fast array,
    ///   update indexed value. Will destruct fast array to sparse array when needed.
    /// - otherwise, call `Self::add_own_property()` to add a shape property.
    pub(super) fn create_property(
        &self,
        ctx: &JSContext,
        prop: Atom,
        val: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let update_array_length_fn =
            |arr: &JSObject, idx: u32, cur_len: i64| -> Option<Result<bool, i32>> {
                debug_assert!(idx < u32::MAX, "array length<=u32::MAX, so idx<u32::MAX");

                if idx + 1 > cur_len as u32 {
                    // prop index 0 is `length`
                    let prs_flags = arr
                        .shape()
                        .map(|sh| sh.0.borrow().prop_defs.first().unwrap().1.flags())
                        .unwrap();

                    if unlikely(prs_flags & JS_PROP_WRITABLE == 0) {
                        return if ctx.can_throw(flags) {
                            ctx.throw_type_error_read_only(JS_ATOM_length);
                            Some(Err(-1))
                        } else {
                            None
                        };
                    } else {
                        arr.0.prop_values.borrow_mut()[0] =
                            JSProperty::Normal(JSValue::from(idx + 1));
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
                    let Ok(arr_len) = self.0.prop_values.borrow()[0]
                        .value(ctx, &JSValue::UNDEFINED)
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
                                    if let Some(ret) = update_array_length_fn(self, idx, arr_len) {
                                        return ret;
                                    }
                                }

                                let prop_flags = get_prop_flags(flags, 0);
                                if prop_flags != JS_PROP_C_W_E {
                                    match self.destruct_fast_array(ctx) {
                                        Ok(_) => {
                                            if let Some(ret) =
                                                update_array_length_fn(self, idx, arr_len)
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
                                if let Some(ret) = update_array_length_fn(self, idx, arr_len) {
                                    return ret;
                                }
                            }
                        } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            // convert fast array to sparse array
                            if self.destruct_fast_array(ctx).is_err() {
                                return if ctx.can_throw(flags) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            } else if let Some(ret) = update_array_length_fn(self, idx, arr_len) {
                                return ret;
                            }
                        }
                    } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                        if let Some(ret) = update_array_length_fn(self, idx, arr_len) {
                            return ret;
                        }
                    }
                } // Array, Arguments

                JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                    let ret = unsafe { JS_AtomIsNumericIndex(ctx, &prop) };
                    if ret != 0 {
                        return if unlikely(ret < 0) {
                            Err(-1)
                        } else {
                            if ctx.can_throw(flags) {
                                ctx.throw_type_error("cannot create numeric index in typed array");
                                Err(-1)
                            } else {
                                Ok(false)
                            }
                        };
                    }
                }

                cid => {
                    if flags & JS_PROP_NO_EXOTIC == 0 {
                        if let Some(em) =
                            unsafe { ctx.rt().class_defs[cid as usize].exotic.as_ref() }
                        {
                            if let Some(f) = em.define_own_property {
                                return f(
                                    ctx,
                                    &JSValue::Object(self.clone()),
                                    &prop,
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
        }

        // Generic: if prop is not an exotic property, nor an array element:
        // add shape property and put the value in value slot.

        if unlikely(!obj_flags.extensible()) {
            return if ctx.can_throw(flags) {
                ctx.throw_type_error("object is not extensible");
                Err(-1)
            } else {
                Ok(false)
            };
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
                            getter.as_object().cloned()
                        } else {
                            None
                        },
                        setter: if flags & JS_PROP_HAS_SET != 0 && setter.is_function() {
                            setter.as_object().cloned()
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
    pub fn has_own_property(&self, ctx: &JSContext, prop: &Atom) -> Result<bool, i32> {
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
        prop: &Atom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        if let JSValue::Object(p) = self {
            p.get_own_property(ctx, prop)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}
